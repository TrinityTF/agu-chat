//! Port of src/renderer/chat/twitch/useTwitchData.ts: what a Twitch source's rows render against, loaded in the
//! background: badges (`/api/chat/badges`, kept in memory for an hour like the site's sessionStorage entry),
//! 7TV / BTTV / FFZ channel + global emotes, and the account's mod capabilities. The React effects become plain
//! async functions; `ChatPane` starts them and applies the answers.
//!
//! Also loaded here (round 3): Twitch's own picker emotes and the account's 7TV personal set, which only the
//! input box uses.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::{AguBridge, SourceRef};
use serde_json::Value;

use super::badges::{build_badge_map, BadgeMap};
use super::emotes::{bttv_sets, fetch_seven_tv_on, ffz_sets, flush_cache, seven_tv_sets, twitch_sets_cached, twitch_sets_fresh, CacheMode, ProviderScopedMap, SevenTvIds, SevenTvPlatform, TwitchEmotesResp};
use crate::chat::mod_caps::{relay_api, ModCaps};

#[derive(Clone, Default)]
pub struct ProviderSets {
    pub seven: ProviderScopedMap,
    pub bttv: ProviderScopedMap,
    pub ffz: ProviderScopedMap,
    /// the channel's 7TV user and active emote set (live updates)
    pub seven_ids: Option<SevenTvIds>,
}

#[derive(Clone, Default)]
pub struct TwitchData {
    pub badge_map: Arc<BadgeMap>,
    /// Bits cheermotes; None logged out (Bits messages stay plain text).
    pub cheermotes: Option<Arc<super::cheermotes::Cheermotes>>,
    pub provider_sets: Option<Arc<ProviderSets>>,
    pub twitch_emotes: TwitchEmotesResp,
    pub caps: ModCaps,
}

thread_local! {
    static BADGE_CACHE: RefCell<HashMap<String, (Instant, Value)>> = RefCell::new(HashMap::new());
}

/// (badge map, channel id) for a source. `None` when the relay had nothing.
pub async fn load_badges(bridge: &Arc<dyn AguBridge>, source: &SourceRef, sk: &str) -> Option<(BadgeMap, Option<String>)> {
    let cached = BADGE_CACHE.with(|c| c.borrow().get(sk).filter(|(t, _)| t.elapsed() < Duration::from_secs(3600)).map(|(_, v)| v.clone()));
    let data = match cached {
        Some(d) => d,
        None => {
            let r = relay_api(bridge, source, "/api/chat/badges", None, None).await;
            if !r.ok || r.body.is_null() {
                return None;
            }
            // never pin an empty answer
            if r.body.get("global").and_then(|g| g.as_array()).map(|g| !g.is_empty()).unwrap_or(false) {
                BADGE_CACHE.with(|c| c.borrow_mut().insert(sk.to_string(), (Instant::now(), r.body.clone())));
            }
            r.body
        }
    };
    let channel_id = data.get("channelId").filter(|c| !c.is_null()).map(|c| c.as_str().map(|s| s.to_string()).unwrap_or_else(|| c.to_string()));
    Some((build_badge_map(&data), channel_id))
}

async fn provider_sets(client: &reqwest::Client, channel_id: &str, mode: CacheMode) -> ProviderSets {
    let ((seven, seven_ids), bttv, ffz) = tokio::join!(seven_tv_sets(client, channel_id, mode), bttv_sets(client, channel_id, mode), ffz_sets(client, channel_id, mode));
    ProviderSets { seven, bttv, ffz, seven_ids }
}

/// A Kick channel's emotes beyond Kick's own: the 7TV set linked to the broadcaster's Kick account (`kick_user_id`, the
/// user id, not the channel id) plus 7TV global, what kick.com shows with the 7TV extension. Kick has no BTTV or FFZ.
pub async fn load_kick_provider_sets(client: reqwest::Client, kick_user_id: String) -> ProviderSets {
    let (seven, seven_ids) = fetch_seven_tv_on(&client, SevenTvPlatform::Kick, &kick_user_id, false).await;
    flush_cache();
    ProviderSets { seven, seven_ids, ..Default::default() }
}

/// 7TV + BTTV + FFZ, kept raw per provider so the pane can apply settings.sevenTv/bttv/ffz. The flag is false when
/// some provider did not answer (what it had before is kept): the pane tries again later.
pub async fn load_provider_sets(client: reqwest::Client, channel_id: String) -> (ProviderSets, bool) {
    let sets = provider_sets(&client, &channel_id, CacheMode::Normal).await;
    flush_cache();
    (sets, twitch_sets_fresh(&channel_id))
}

/// The channel's sets as last cached, however old, without the network: what startup shows at once while
/// `load_provider_sets` refreshes them. None when nothing is cached.
pub async fn load_cached_provider_sets(client: reqwest::Client, channel_id: String) -> Option<ProviderSets> {
    super::emotes::cache_ready().await;
    if !twitch_sets_cached(&channel_id) {
        return None;
    }
    Some(provider_sets(&client, &channel_id, CacheMode::CacheOnly).await)
}

/// Only the channel's 7TV set again, past the cache (it changed live, or changes may have been missed).
pub async fn reload_seven_tv(client: reqwest::Client, platform: SevenTvPlatform, channel_id: String) -> (ProviderScopedMap, Option<SevenTvIds>) {
    let r = fetch_seven_tv_on(&client, platform, &channel_id, true).await;
    flush_cache();
    r
}
