//! Port of src/renderer/chat/twitch/emotes.ts: 7TV / BTTV / FFZ emote sets, the priority merge and the Twitch
//! picker emotes. The localStorage cache (`emotes:<provider>:<scope>:<id>` keys, `{v:1, at, data}` values)
//! becomes one JSON file in the data dir (`emotes-cache.json`) holding the same entries.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SizeOpt {
    Small,
    Medium,
    Large,
}

impl SizeOpt {
    pub fn parse(s: &str) -> SizeOpt {
        match s {
            "small" => SizeOpt::Small,
            "large" => SizeOpt::Large,
            _ => SizeOpt::Medium,
        }
    }
    fn density(self) -> usize {
        match self {
            SizeOpt::Small => 1,
            SizeOpt::Medium => 2,
            SizeOpt::Large => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EmoteProvider {
    SevenTv,
    Bttv,
    Ffz,
}

impl EmoteProvider {
    pub fn label(self) -> &'static str {
        match self {
            EmoteProvider::SevenTv => "7TV",
            EmoteProvider::Bttv => "BetterTTV",
            EmoteProvider::Ffz => "FrankerFaceZ",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderEmote {
    pub provider: EmoteProvider,
    pub id: String,
    pub name: String,
    pub zero_width: bool,
    pub animated: bool,
    /// Absolute CDN URLs by density 1..4 (index 0..3); providers don't all offer the same set.
    pub urls: [Option<String>; 4],
}

impl ProviderEmote {
    fn url(&self, d: usize) -> Option<&str> {
        self.urls[d - 1].as_deref()
    }
}

pub fn provider_emote_url(e: &ProviderEmote, size: SizeOpt) -> String {
    let want = size.density();
    e.url(want).or_else(|| e.url(2)).or_else(|| e.url(1)).or_else(|| e.url(3)).or_else(|| e.url(4)).unwrap_or("").to_string()
}

pub fn provider_tooltip_url(e: &ProviderEmote) -> String {
    e.url(4).or_else(|| e.url(3)).or_else(|| e.url(2)).or_else(|| e.url(1)).unwrap_or("").to_string()
}

pub type EmoteMap = HashMap<String, Arc<ProviderEmote>>;

#[derive(Clone, Default)]
pub struct ProviderScopedMap {
    pub global: EmoteMap,
    pub channel: EmoteMap,
}

#[derive(Clone, Default)]
pub struct EmoteSets {
    pub channel: EmoteMap,
    pub global: EmoteMap,
    /// final lookup: channel overrides global
    pub all: EmoteMap,
}

// ---- JSON file cache --------------------------------------------------------------------------

pub const EMOTE_CHANNEL_TTL_MS: i64 = 3_600_000;
pub const EMOTE_GLOBAL_TTL_MS: i64 = 6 * 3_600_000;

struct Cache {
    path: Option<PathBuf>,
    map: serde_json::Map<String, Value>,
    dirty: bool,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { path: None, map: Default::default(), dirty: false }))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Point the cache at `<data dir>/emotes-cache.json` and load it.
pub fn init_cache(dir: PathBuf) {
    let path = dir.join("emotes-cache.json");
    let map = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut c = cache().lock().unwrap();
    c.path = Some(path);
    c.map = map;
}

/// readLsCache: the entry's data while it is fresh.
pub fn read_cache(key: &str, ttl_ms: i64) -> Option<Value> {
    let c = cache().lock().unwrap();
    let e = c.map.get(key)?;
    if e.get("v")?.as_i64()? != 1 {
        return None;
    }
    let at = e.get("at")?.as_i64()?;
    if now_ms() - at >= ttl_ms {
        return None;
    }
    e.get("data").cloned()
}

pub fn write_cache(key: &str, data: &Value) {
    let mut c = cache().lock().unwrap();
    c.map.insert(key.to_string(), json!({"v": 1, "at": now_ms(), "data": data}));
    c.dirty = true;
}

/// Write the cache file if it changed (called after a batch of fetches).
pub fn flush_cache() {
    let mut c = cache().lock().unwrap();
    if !c.dirty {
        return;
    }
    c.dirty = false;
    if let (Some(p), Ok(s)) = (c.path.clone(), serde_json::to_vec(&Value::Object(c.map.clone()))) {
        let _ = std::fs::write(p, s);
    }
}

async fn fetch_json_cached(client: &reqwest::Client, key: &str, ttl: i64, url: &str) -> Option<Value> {
    if let Some(v) = read_cache(key, ttl) {
        return Some(v);
    }
    let r = client.get(url).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let data: Value = r.json().await.ok()?;
    write_cache(key, &data);
    Some(data)
}

// ---- 7TV --------------------------------------------------------------------------------------

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn seven_tv_map(emotes: Option<&Value>) -> EmoteMap {
    let mut m = EmoteMap::new();
    for item in emotes.and_then(|e| e.as_array()).into_iter().flatten() {
        let flags = item.get("flags").and_then(|f| f.as_i64()).unwrap_or(0);
        let data_flags = item.pointer("/data/flags").and_then(|f| f.as_i64()).unwrap_or(0);
        let zero_width = flags & 1 != 0 || data_flags & 256 != 0;
        let id = s(item, "id");
        let name = s(item, "name");
        let u = |d: u8| Some(format!("https://cdn.7tv.app/emote/{id}/{d}x.webp"));
        m.insert(
            name.clone(),
            Arc::new(ProviderEmote {
                provider: EmoteProvider::SevenTv,
                id: id.clone(),
                name,
                zero_width,
                animated: item.pointer("/data/animated").and_then(|a| a.as_bool()).unwrap_or(false),
                urls: [u(1), u(2), u(3), u(4)],
            }),
        );
    }
    m
}

pub async fn fetch_seven_tv(client: &reqwest::Client, channel_id: &str) -> ProviderScopedMap {
    let (ck, cu) = (format!("emotes:7tv:channel:{channel_id}"), format!("https://7tv.io/v3/users/twitch/{channel_id}"));
    let (g, c) = tokio::join!(
        fetch_json_cached(client, "emotes:7tv:global:global", EMOTE_GLOBAL_TTL_MS, "https://7tv.io/v3/emote-sets/global"),
        fetch_json_cached(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu),
    );
    ProviderScopedMap { global: g.map(|g| seven_tv_map(g.get("emotes"))).unwrap_or_default(), channel: c.map(|c| seven_tv_map(c.pointer("/emote_set/emotes"))).unwrap_or_default() }
}

// ---- BTTV -------------------------------------------------------------------------------------

const BTTV_ZW_NAMES: [&str; 8] = ["SoSnowy", "IceCold", "SantaHat", "TopHat", "ReinDeer", "CandyCane", "cvMask", "cvHazmat"];

fn bttv_emote(e: &Value) -> Arc<ProviderEmote> {
    let code = s(e, "code");
    let id = s(e, "id");
    let zero_width = e.get("modifier").and_then(|m| m.as_bool()) == Some(true) || BTTV_ZW_NAMES.contains(&code.as_str());
    let u = |d: u8| Some(format!("https://cdn.betterttv.net/emote/{id}/{d}x"));
    Arc::new(ProviderEmote { provider: EmoteProvider::Bttv, id: id.clone(), name: code, zero_width, animated: s(e, "imageType") == "gif", urls: [u(1), u(2), u(3), None] })
}

pub async fn fetch_bttv(client: &reqwest::Client, channel_id: &str) -> ProviderScopedMap {
    let (ck, cu) = (format!("emotes:bttv:channel:{channel_id}"), format!("https://api.betterttv.net/3/cached/users/twitch/{channel_id}"));
    let (g, c) = tokio::join!(
        fetch_json_cached(client, "emotes:bttv:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.betterttv.net/3/cached/emotes/global"),
        fetch_json_cached(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu),
    );
    let mut out = ProviderScopedMap::default();
    for e in g.as_ref().and_then(|g| g.as_array()).into_iter().flatten() {
        let p = bttv_emote(e);
        out.global.insert(p.name.clone(), p);
    }
    if let Some(c) = c {
        for key in ["channelEmotes", "sharedEmotes"] {
            for e in c.get(key).and_then(|a| a.as_array()).into_iter().flatten() {
                let p = bttv_emote(e);
                out.channel.insert(p.name.clone(), p);
            }
        }
    }
    out
}

// ---- FFZ --------------------------------------------------------------------------------------

fn ffz_emote(e: &Value) -> Arc<ProviderEmote> {
    let mut urls: [Option<String>; 4] = Default::default();
    for (slot, k) in [(0usize, "1"), (1, "2"), (3, "4")] {
        if let Some(u) = e.pointer(&format!("/urls/{k}")).and_then(|u| u.as_str()) {
            urls[slot] = Some(if let Some(rest) = u.strip_prefix("//") { format!("https://{rest}") } else { u.to_string() });
        }
    }
    let zero_width = e.get("modifier").and_then(|m| m.as_bool()) == Some(true) || e.get("modifier_flags").and_then(|m| m.as_i64()).unwrap_or(0) > 0;
    Arc::new(ProviderEmote { provider: EmoteProvider::Ffz, id: s(e, "id"), name: s(e, "name"), zero_width, animated: false, urls })
}

pub async fn fetch_ffz(client: &reqwest::Client, channel_id: &str) -> ProviderScopedMap {
    let (ck, cu) = (format!("emotes:ffz:channel:{channel_id}"), format!("https://api.frankerfacez.com/v1/room/id/{channel_id}"));
    let (g, r) = tokio::join!(
        fetch_json_cached(client, "emotes:ffz:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.frankerfacez.com/v1/set/global"),
        fetch_json_cached(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu),
    );
    let mut out = ProviderScopedMap::default();
    if let Some(g) = g {
        for set_id in g.get("default_sets").and_then(|a| a.as_array()).into_iter().flatten() {
            let key = match set_id {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            for e in g.pointer(&format!("/sets/{key}/emoticons")).and_then(|a| a.as_array()).into_iter().flatten() {
                let p = ffz_emote(e);
                out.global.insert(p.name.clone(), p);
            }
        }
    }
    if let Some(r) = r {
        let set = r.pointer("/room/set").map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        });
        if let Some(set) = set.filter(|s| !s.is_empty()) {
            for e in r.pointer(&format!("/sets/{set}/emoticons")).and_then(|a| a.as_array()).into_iter().flatten() {
                let p = ffz_emote(e);
                out.channel.insert(p.name.clone(), p);
            }
        }
    }
    out
}

// ---- merge ------------------------------------------------------------------------------------

/// Priority high -> low: 7TV-channel, BTTV-channel, FFZ-channel, 7TV-global, BTTV-global, FFZ-global; any
/// channel emote beats any global emote regardless of provider.
pub fn merge_emote_sets(seven: &ProviderScopedMap, bttv: &ProviderScopedMap, ffz: &ProviderScopedMap) -> EmoteSets {
    let mut channel = EmoteMap::new();
    for m in [&ffz.channel, &bttv.channel, &seven.channel] {
        channel.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let mut global = EmoteMap::new();
    for m in [&ffz.global, &bttv.global, &seven.global] {
        global.extend(m.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    let mut all = global.clone();
    all.extend(channel.iter().map(|(k, v)| (k.clone(), v.clone())));
    EmoteSets { channel, global, all }
}

// ---- Twitch picker emotes -----------------------------------------------------------------------

/// The 7TV personal emote set of the ACTING user; best effort, any missing step yields nothing.
pub async fn fetch_seven_tv_personal(client: &reqwest::Client, twitch_user_id: &str) -> Vec<Arc<ProviderEmote>> {
    async fn go(client: &reqwest::Client, uid: &str) -> Option<Vec<Arc<ProviderEmote>>> {
        let id_key = format!("emotes:7tv:personal-id:{uid}");
        let seven_id = match read_cache(&id_key, EMOTE_GLOBAL_TTL_MS) {
            Some(v) => v.as_str().unwrap_or("").to_string(),
            None => {
                let r = client.get(format!("https://7tv.io/v3/users/twitch/{uid}")).send().await.ok()?;
                if !r.status().is_success() {
                    return None;
                }
                let d: Value = r.json().await.ok()?;
                let id = d.pointer("/user/id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                write_cache(&id_key, &json!(id));
                id
            }
        };
        if seven_id.is_empty() {
            return None;
        }
        let set_key = format!("emotes:7tv:personal-set:{seven_id}");
        let set_id = match read_cache(&set_key, EMOTE_GLOBAL_TTL_MS) {
            Some(v) => v.as_str().unwrap_or("").to_string(),
            None => {
                let r = client.get(format!("https://7tv.io/v3/users/{seven_id}")).send().await.ok()?;
                if !r.status().is_success() {
                    return None;
                }
                let d: Value = r.json().await.ok()?;
                let id = d
                    .get("emote_sets")
                    .and_then(|a| a.as_array())
                    .and_then(|sets| {
                        sets.iter().find(|st| st.get("flags").and_then(|f| f.as_i64()).unwrap_or(0) & 4 == 4 || s(st, "name").to_lowercase().contains("personal"))
                    })
                    .map(|st| s(st, "id"))
                    .unwrap_or_default();
                write_cache(&set_key, &json!(id));
                id
            }
        };
        if set_id.is_empty() {
            return None;
        }
        let emotes_key = format!("emotes:7tv:personal-emotes:{set_id}");
        let list = match read_cache(&emotes_key, EMOTE_CHANNEL_TTL_MS) {
            Some(v) => v,
            None => {
                let r = client.get(format!("https://7tv.io/v3/emote-sets/{set_id}")).send().await.ok()?;
                if !r.status().is_success() {
                    return None;
                }
                let d: Value = r.json().await.ok()?;
                let l = d.get("emotes").cloned().unwrap_or(json!([]));
                write_cache(&emotes_key, &l);
                l
            }
        };
        Some(seven_tv_map(Some(&list)).into_values().collect())
    }
    go(client, twitch_user_id).await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_tv_flags_and_urls() {
        let m = seven_tv_map(Some(&json!([{"id": "abc", "name": "ZW", "flags": 1, "data": {"animated": true}}, {"id": "d", "name": "N", "flags": 0, "data": {"flags": 256}}])));
        assert!(m["ZW"].zero_width && m["ZW"].animated);
        assert!(m["N"].zero_width);
        assert_eq!(provider_emote_url(&m["ZW"], SizeOpt::Medium), "https://cdn.7tv.app/emote/abc/2x.webp");
        assert_eq!(provider_tooltip_url(&m["ZW"]), "https://cdn.7tv.app/emote/abc/4x.webp");
    }

    #[test]
    fn bttv_has_no_4x_and_falls_back() {
        let e = bttv_emote(&json!({"id": "x", "code": "OMEGALUL", "imageType": "gif"}));
        assert_eq!(provider_tooltip_url(&e), "https://cdn.betterttv.net/emote/x/3x");
        assert!(e.animated);
    }

    #[test]
    fn channel_beats_global() {
        let mk = |n: &str, id: &str| Arc::new(ProviderEmote { provider: EmoteProvider::Bttv, id: id.into(), name: n.into(), zero_width: false, animated: false, urls: Default::default() });
        let mut seven = ProviderScopedMap::default();
        seven.global.insert("A".into(), mk("A", "g"));
        let mut bttv = ProviderScopedMap::default();
        bttv.channel.insert("A".into(), mk("A", "c"));
        let sets = merge_emote_sets(&seven, &bttv, &ProviderScopedMap::default());
        assert_eq!(sets.all["A"].id, "c");
    }
}
