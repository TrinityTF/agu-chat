//! Port of src/renderer/chat/modCaps.ts (and the `relayApi()` wrapper of bridge.ts): ModCaps / fetchModCaps /
//! modReach, the timeout ladder, plus the Rumble and Kick caps fetches. Every call goes through the bridge's
//! `api(source, path, init)`; the answer is not cached for the page's life (the account can log in later,
//! callers refetch when the account changes), the backend caches what is worth caching.
use std::collections::HashSet;
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, ApiInit, Platform, SourceRef};
use serde_json::Value;

/// `relayApi()`'s Response-like shape.
pub struct RelayResponse {
    pub ok: bool,
    pub status: u16,
    pub body: Value,
}

impl RelayResponse {
    /// `r.text()`
    pub fn text(&self) -> String {
        match &self.body {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
}

pub async fn relay_api(bridge: &Arc<dyn AguBridge>, source: &SourceRef, path: &str, method: Option<&str>, body: Option<Value>) -> RelayResponse {
    let r = bridge.api(source.clone(), path.to_string(), ApiInit { method: method.map(|m| m.to_string()), body }).await;
    RelayResponse { ok: (200..300).contains(&r.status), status: r.status, body: r.body }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModCaps {
    pub pin: bool,
    pub ban: bool,
    pub warn: bool,
    pub delete: bool,
    pub send: bool,
    pub modes: bool,
    pub announce: bool,
    pub shoutout: bool,
    pub shield: bool,
    pub terms: bool,
    pub raid: bool,
    /// moderator:manage:automod - gates the AutoMod Allow/Deny buttons.
    pub automod: bool,
    /// From /api/mod/caps of the local relay: logged in / is this channel's broadcaster / a mod.
    pub logged_in: Option<bool>,
    pub is_broadcaster: Option<bool>,
    pub is_mod: Option<bool>,
}

pub async fn fetch_mod_caps(bridge: &Arc<dyn AguBridge>, source: &SourceRef) -> ModCaps {
    let r = relay_api(bridge, source, "/api/mod/caps", None, None).await;
    if !r.ok {
        return ModCaps::default();
    }
    let b = |k: &str| r.body.get(k).map(|v| !v.is_null() && v != &Value::Bool(false) && v != &Value::from(0) && v != &Value::from("")).unwrap_or(false);
    let ob = |k: &str| r.body.get(k).and_then(|v| v.as_bool());
    ModCaps {
        pin: b("pin"),
        ban: b("ban"),
        warn: b("warn"),
        delete: b("delete"),
        send: b("send"),
        modes: b("modes"),
        announce: b("announce"),
        shoutout: b("shoutout"),
        shield: b("shield"),
        terms: b("terms"),
        raid: b("raid"),
        automod: b("automod"),
        logged_in: ob("loggedIn"),
        is_broadcaster: ob("isBroadcaster"),
        is_mod: ob("isMod"),
    }
}

/// Whether the acting user can moderate this Rumble / Kick chat (/api/rumble/caps, /api/kick/caps).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CoCaps {
    pub connected: bool,
    pub can_moderate: bool,
    pub can_delete: bool,
}

pub async fn fetch_co_caps(bridge: &Arc<dyn AguBridge>, source: &SourceRef) -> CoCaps {
    let path = match source.platform {
        Platform::Rumble => "/api/rumble/caps",
        Platform::Kick => "/api/kick/caps",
        _ => return CoCaps::default(),
    };
    let r = relay_api(bridge, source, path, None, None).await;
    if !r.ok {
        return CoCaps::default();
    }
    let b = |k: &str| r.body.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    CoCaps {
        connected: b("connected"),
        can_moderate: b("canModerate"),
        // Rumble deletes with the same right as bans; Kick reports canDelete separately.
        can_delete: if source.platform == Platform::Kick { b("canDelete") } else { b("canModerate") },
    }
}

pub const TIMEOUT_LADDER: [(&str, u32); 9] = [("1m", 60), ("5m", 300), ("10m", 600), ("30m", 1800), ("1h", 3600), ("12h", 43200), ("24h", 86400), ("1w", 604800), ("2w", 1209600)];

/// Who's looking, for mod_reach: their user id, and whether they're this chat's broadcaster or one of its
/// lead moderators.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModViewer {
    pub id: String,
    pub broadcaster: bool,
    pub lead: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModReach {
    pub ban: bool,
    pub timeout: bool,
    pub warn: bool,
    pub delete: bool,
}

pub const REACH_ALL: ModReach = ModReach { ban: true, timeout: true, warn: true, delete: true };

pub fn badge_names(tag: &str) -> HashSet<&str> {
    tag.split(',').map(|b| b.split('/').next().unwrap_or("")).filter(|b| !b.is_empty()).collect()
}

/// What the viewer may do to a chatter, by Twitch's rules: nobody acts on the broadcaster; a mod can't act on
/// another mod, but the broadcaster can, and a lead moderator can on ordinary mods. Nobody bans themselves,
/// but they can time out or warn themselves. The viewer's mod rights (ModCaps) are checked separately.
pub fn mod_reach(viewer: &ModViewer, target_id: &str, target_badges: &str) -> ModReach {
    let b = badge_names(target_badges);
    let is_self = !viewer.id.is_empty() && viewer.id == target_id;
    if b.contains("broadcaster") {
        return ModReach { ban: false, timeout: false, warn: false, delete: is_self };
    }
    if is_self {
        return ModReach { ban: false, timeout: true, warn: true, delete: true };
    }
    let lead = b.contains("lead_moderator");
    let ok = !(lead || b.contains("moderator")) || viewer.broadcaster || (viewer.lead && !lead);
    ModReach { ban: ok, timeout: ok, warn: ok, delete: ok }
}

/// Kick / Rumble: whether the viewer may time out or ban a chatter. Nobody acts on the channel's owner (by badge,
/// or because the chatter's name is the channel's), on Kick staff, or on themselves; only the owner acts on a
/// moderator. Names compare loosely (Kick's channel slug writes the username's `_` as `-`).
pub fn co_reach(platform: Platform, viewer_login: &str, channel: &str, target_login: &str, target_badges: &[&str]) -> ModReach {
    let norm = |s: &str| s.trim().to_lowercase().replace('_', "-");
    let (viewer, channel, target) = (norm(viewer_login), norm(channel), norm(target_login));
    let owner_badge = |b: &&str| match platform {
        Platform::Rumble => matches!(*b, "admin" | "broadcaster" | "streamer"),
        _ => matches!(*b, "broadcaster" | "staff" | "global_admin" | "global_moderator"),
    };
    let none = ModReach { ban: false, timeout: false, warn: false, delete: false };
    let target_owner = target_badges.iter().any(owner_badge) || (!target.is_empty() && target == channel);
    let is_self = !viewer.is_empty() && viewer == target;
    if target_owner || is_self {
        return none;
    }
    let viewer_owner = !viewer.is_empty() && viewer == channel;
    if target_badges.iter().any(|b| *b == "moderator") && !viewer_owner {
        return none;
    }
    REACH_ALL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_and_rumble_reach() {
        let k = |viewer: &str, target: &str, badges: &[&str]| co_reach(Platform::Kick, viewer, "agu-tf", target, badges).ban;
        // the channel owner: by badge, or by name (slug "agu-tf" is the user "Agu_TF")
        assert!(!k("somemod", "Agu_TF", &[]));
        assert!(!k("somemod", "whoever", &["broadcaster"]));
        assert!(!k("somemod", "kickstaff", &["staff"]));
        // a mod can't act on a mod, the owner can
        assert!(!k("somemod", "othermod", &["moderator"]));
        assert!(k("Agu_TF", "othermod", &["moderator"]));
        // not on yourself; on an ordinary chatter yes
        assert!(!k("somemod", "SomeMod", &[]));
        assert!(k("somemod", "viewer1", &["subscriber", "og"]));
        let r = |target: &str, badges: &[&str]| co_reach(Platform::Rumble, "me", "streamer", target, badges).timeout;
        assert!(!r("x", &["admin"]));
        assert!(!r("x", &["moderator"]));
        assert!(r("x", &["premium"]));
    }

    fn viewer(id: &str, broadcaster: bool, lead: bool) -> ModViewer {
        ModViewer { id: id.into(), broadcaster, lead }
    }

    #[test]
    fn nobody_acts_on_the_broadcaster() {
        let r = mod_reach(&viewer("1", false, false), "2", "broadcaster/1,subscriber/0");
        assert_eq!((r.ban, r.timeout, r.warn, r.delete), (false, false, false, false));
        assert!(mod_reach(&viewer("2", true, false), "2", "broadcaster/1").delete);
    }

    #[test]
    fn mods_vs_mods() {
        assert!(!mod_reach(&viewer("1", false, false), "2", "moderator/1").ban);
        assert!(mod_reach(&viewer("1", true, false), "2", "moderator/1").ban);
        assert!(mod_reach(&viewer("1", false, true), "2", "moderator/1").ban);
        assert!(!mod_reach(&viewer("1", false, true), "2", "lead_moderator/1").ban);
    }

    #[test]
    fn self_can_timeout_but_not_ban() {
        let r = mod_reach(&viewer("1", false, false), "1", "");
        assert_eq!((r.ban, r.timeout, r.warn, r.delete), (false, true, true, true));
    }

    #[test]
    fn badge_names_strip_versions() {
        assert_eq!(badge_names("moderator/1,subscriber/12").len(), 2);
        assert!(badge_names("").is_empty());
    }
}
