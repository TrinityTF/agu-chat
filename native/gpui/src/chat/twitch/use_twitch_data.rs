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
use super::emotes::{fetch_bttv, fetch_ffz, fetch_seven_tv, flush_cache, ProviderScopedMap, TwitchEmotesResp};
use crate::chat::mod_caps::{relay_api, ModCaps};

#[derive(Clone, Default)]
pub struct ProviderSets {
    pub seven: ProviderScopedMap,
    pub bttv: ProviderScopedMap,
    pub ffz: ProviderScopedMap,
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

/// 7TV + BTTV + FFZ, kept raw per provider so the pane can apply settings.sevenTv/bttv/ffz.
pub async fn load_provider_sets(client: reqwest::Client, channel_id: String) -> ProviderSets {
    let (seven, bttv, ffz) = tokio::join!(fetch_seven_tv(&client, &channel_id), fetch_bttv(&client, &channel_id), fetch_ffz(&client, &channel_id));
    flush_cache();
    ProviderSets { seven, bttv, ffz }
}
