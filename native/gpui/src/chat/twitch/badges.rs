//! Port of src/renderer/chat/twitch/badges.ts. The site cached badges in sessionStorage per channel; here
//! `use_twitch_data` keeps the map per source in memory.
use std::collections::HashMap;

use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct BadgeEntry {
    pub url: String,
    pub title: String,
}
pub type BadgeMap = HashMap<String, BadgeEntry>;

/// From the `/api/chat/badges` answer `{ global: [...], channel: [...] }` (sets of `{set_id, versions}`).
pub fn build_badge_map(resp: &Value) -> BadgeMap {
    let mut m = BadgeMap::new();
    for key in ["global", "channel"] {
        for set in resp.get(key).and_then(|v| v.as_array()).into_iter().flatten() {
            let set_id = set.get("set_id").and_then(|v| v.as_str()).unwrap_or("");
            for v in set.get("versions").and_then(|v| v.as_array()).into_iter().flatten() {
                let id = v.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let url = v.get("image_url_2x").and_then(|v| v.as_str()).unwrap_or("");
                let title = v.get("title").and_then(|v| v.as_str()).filter(|t| !t.is_empty()).unwrap_or(set_id);
                m.insert(format!("{set_id}/{id}"), BadgeEntry { url: url.into(), title: title.into() });
            }
        }
    }
    m
}

pub fn parse_badge_list(tag: &str, map: &BadgeMap) -> Vec<BadgeEntry> {
    if tag.is_empty() {
        return Vec::new();
    }
    tag.split(',').filter_map(|b| map.get(b).cloned()).collect()
}

/// Badge <img>s get the same vertical-align:middle + negative vertical margin treatment as emotes.
pub const BADGE_PX: f32 = 16.0;
pub const BADGE_VMARGIN: f32 = -3.0;
