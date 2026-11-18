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

/// Entries not refreshed for this long are dropped when the file is loaded (channels not opened in a week); the
/// file otherwise only grew, to ~10 MB in real use.
pub const CACHE_PRUNE_MS: i64 = 7 * 24 * 3_600_000;

struct Cache {
    path: Option<PathBuf>,
    map: serde_json::Map<String, Value>,
    dirty: bool,
    /// the file is still being read (in the background): it must not be written over yet
    loading: bool,
    /// forgotten while loading: the file's copy must not come back
    forgotten: std::collections::HashSet<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static C: OnceLock<Mutex<Cache>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(Cache { path: None, map: Default::default(), dirty: false, loading: false, forgotten: Default::default() }))
}

/// true once the cache file was read (or there is none to read)
fn ready() -> &'static tokio::sync::watch::Sender<bool> {
    static R: OnceLock<tokio::sync::watch::Sender<bool>> = OnceLock::new();
    R.get_or_init(|| tokio::sync::watch::channel(true).0)
}

/// Wait until the cache file is loaded (it is read off the UI thread at startup). Readers that skip this just miss.
pub async fn cache_ready() {
    let mut rx = ready().subscribe();
    let _ = rx.wait_for(|v| *v).await;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The entries of a cache file worth keeping: well-formed and refreshed within `CACHE_PRUNE_MS`.
fn parse_cache_file(bytes: &[u8], now: i64) -> serde_json::Map<String, Value> {
    let mut map = match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => m,
        _ => Default::default(),
    };
    map.retain(|_, e| e.get("at").and_then(|a| a.as_i64()).is_some_and(|at| now - at < CACHE_PRUNE_MS));
    map
}

/// Point the cache at `<data dir>/emotes-cache.json` and load it on a background thread (the file is megabytes of
/// JSON; parsing it on the UI thread held up startup). `cache_ready()` resolves when it is in.
pub fn init_cache(dir: PathBuf) {
    let path = dir.join("emotes-cache.json");
    {
        let mut c = cache().lock().unwrap();
        c.path = Some(path.clone());
        c.loading = true;
    }
    ready().send_replace(false);
    let load = move || {
        let map = std::fs::read(&path).map(|b| parse_cache_file(&b, now_ms())).unwrap_or_default();
        let mut c = cache().lock().unwrap();
        let forgotten = std::mem::take(&mut c.forgotten);
        for (k, v) in map {
            // anything written meanwhile is newer than the file's copy
            if !forgotten.contains(&k) {
                c.map.entry(k).or_insert(v);
            }
        }
        c.loading = false;
        drop(c);
        ready().send_replace(true);
    };
    if let Err(e) = std::thread::Builder::new().name("agu-emote-cache".into()).spawn(load) {
        // no thread: nothing gets loaded, but nobody waits forever either
        eprintln!("[emotes] cache load thread: {e}");
        cache().lock().unwrap().loading = false;
        ready().send_replace(true);
    }
}

fn entry_data(e: &Value, ttl_ms: Option<i64>) -> Option<&Value> {
    if e.get("v")?.as_i64()? != 1 {
        return None;
    }
    let at = e.get("at")?.as_i64()?;
    if ttl_ms.is_some_and(|ttl| now_ms() - at >= ttl) {
        return None;
    }
    e.get("data")
}

/// readLsCache: the entry's data while it is fresh.
pub fn read_cache(key: &str, ttl_ms: i64) -> Option<Value> {
    let c = cache().lock().unwrap();
    entry_data(c.map.get(key)?, Some(ttl_ms)).cloned()
}

/// The entry's data however old it is (shown at once while a refresh runs, or kept when the refresh fails).
pub fn read_cache_any(key: &str) -> Option<Value> {
    let c = cache().lock().unwrap();
    entry_data(c.map.get(key)?, None).cloned()
}

/// Whether the entry is there and fresh (without copying it).
pub fn cache_fresh(key: &str, ttl_ms: i64) -> bool {
    let c = cache().lock().unwrap();
    c.map.get(key).and_then(|e| entry_data(e, Some(ttl_ms))).is_some()
}

/// Drop an entry (its data went stale: the next load fetches it again).
pub fn forget_cache(key: &str) {
    let mut c = cache().lock().unwrap();
    if c.loading {
        c.forgotten.insert(key.to_string());
    }
    if c.map.remove(key).is_some() {
        c.dirty = true;
    }
}

pub fn write_cache(key: &str, data: &Value) {
    let mut c = cache().lock().unwrap();
    c.map.insert(key.to_string(), json!({"v": 1, "at": now_ms(), "data": data}));
    c.dirty = true;
}

/// Write the cache file if it changed (called after a batch of fetches). The map is copied under the lock and
/// written outside it, through a temp file, so readers never wait on the disk and a crash never leaves half a file.
pub fn flush_cache() {
    let (path, map) = {
        let mut c = cache().lock().unwrap();
        // still loading: writing now would replace the file with the few entries fetched so far
        if !c.dirty || c.loading {
            return;
        }
        let Some(path) = c.path.clone() else { return };
        c.dirty = false;
        (path, c.map.clone())
    };
    if let Ok(bytes) = serde_json::to_vec(&Value::Object(map)) {
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// How a set's cached entry is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheMode {
    /// a fresh entry, else the network (the old entry if that fails)
    Normal,
    /// the network (the old entry if that fails): a live update said it changed
    Fresh,
    /// only what is cached, however old: shown at once on startup while the network catches up
    CacheOnly,
}

#[allow(dead_code)] // the plain forms, for sets that need no startup / retry handling
async fn fetch_json_cached(client: &reqwest::Client, key: &str, ttl: i64, url: &str) -> Option<Value> {
    fetch_json(client, key, ttl, url, false).await
}

/// `fresh` skips the cached entry (the answer still replaces it).
#[allow(dead_code)]
async fn fetch_json(client: &reqwest::Client, key: &str, ttl: i64, url: &str, fresh: bool) -> Option<Value> {
    fetch_json_mode(client, key, ttl, url, if fresh { CacheMode::Fresh } else { CacheMode::Normal }).await
}

/// A provider answer through the cache. A 404 (the channel has no account there) is an answer too and is cached
/// as `null`; any other failure falls back to the old entry and leaves it un-refreshed, so the caller tries again.
async fn fetch_json_mode(client: &reqwest::Client, key: &str, ttl: i64, url: &str, mode: CacheMode) -> Option<Value> {
    cache_ready().await;
    match mode {
        CacheMode::CacheOnly => return read_cache_any(key),
        CacheMode::Normal => {
            if let Some(v) = read_cache(key, ttl) {
                return Some(v);
            }
        }
        CacheMode::Fresh => {}
    }
    let got = async {
        let r = client.get(url).send().await.ok()?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Some(Value::Null);
        }
        if !r.status().is_success() {
            return None;
        }
        r.json::<Value>().await.ok()
    }
    .await;
    match got {
        Some(data) => {
            write_cache(key, &data);
            Some(data)
        }
        None => read_cache_any(key),
    }
}

/// The cache entries a Twitch channel's 7TV / BTTV / FFZ sets come from, with how long each stays fresh.
pub fn twitch_set_keys(channel_id: &str) -> [(String, i64); 6] {
    [
        ("emotes:7tv:global:global".to_string(), EMOTE_GLOBAL_TTL_MS),
        (seven_tv_channel_key(channel_id), EMOTE_CHANNEL_TTL_MS),
        ("emotes:bttv:global:global".to_string(), EMOTE_GLOBAL_TTL_MS),
        (bttv_channel_key(channel_id), EMOTE_CHANNEL_TTL_MS),
        ("emotes:ffz:global:global".to_string(), EMOTE_GLOBAL_TTL_MS),
        (ffz_channel_key(channel_id), EMOTE_CHANNEL_TTL_MS),
    ]
}

/// Every set of the channel is in and fresh (else a fetch failed, and the load is tried again).
pub fn twitch_sets_fresh(channel_id: &str) -> bool {
    twitch_set_keys(channel_id).iter().all(|(k, ttl)| cache_fresh(k, *ttl))
}

/// Whether anything of the channel's sets is cached at all, however old (worth showing before the network answers).
pub fn twitch_sets_cached(channel_id: &str) -> bool {
    let c = cache().lock().unwrap();
    twitch_set_keys(channel_id).iter().any(|(k, _)| c.map.get(k).and_then(|e| entry_data(e, None)).is_some())
}

/// The wait before trying a set load again that did not get everything (`attempt` = tries so far); None = stop.
pub fn set_retry_delay(attempt: u32) -> Option<std::time::Duration> {
    const SECS: [u64; 8] = [3, 8, 20, 45, 90, 180, 300, 600];
    SECS.get(attempt as usize).map(|s| std::time::Duration::from_secs(*s))
}

// ---- 7TV --------------------------------------------------------------------------------------

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// One ActiveEmote of a 7TV set (`{id, name, flags, data: {flags, animated}}`); `name` is the set's name for it.
pub fn seven_tv_emote(item: &Value) -> Arc<ProviderEmote> {
    let flags = item.get("flags").and_then(|f| f.as_i64()).unwrap_or(0);
    let data_flags = item.pointer("/data/flags").and_then(|f| f.as_i64()).unwrap_or(0);
    let zero_width = flags & 1 != 0 || data_flags & 256 != 0;
    let id = s(item, "id");
    let u = |d: u8| Some(format!("https://cdn.7tv.app/emote/{id}/{d}x.webp"));
    Arc::new(ProviderEmote {
        provider: EmoteProvider::SevenTv,
        id: id.clone(),
        name: s(item, "name"),
        zero_width,
        animated: item.pointer("/data/animated").and_then(|a| a.as_bool()).unwrap_or(false),
        urls: [u(1), u(2), u(3), u(4)],
    })
}

fn seven_tv_map(emotes: Option<&Value>) -> EmoteMap {
    let mut m = EmoteMap::new();
    for item in emotes.and_then(|e| e.as_array()).into_iter().flatten() {
        let e = seven_tv_emote(item);
        m.insert(e.name.clone(), e);
    }
    m
}

/// The emotes of one 7TV set by id, never cached (a set the channel just switched to).
pub async fn fetch_seven_tv_set(client: &reqwest::Client, set_id: &str) -> Option<EmoteMap> {
    let r = client.get(format!("https://7tv.io/v3/emote-sets/{set_id}")).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let d: Value = r.json().await.ok()?;
    Some(seven_tv_map(d.get("emotes")))
}

/// A channel's 7TV account: its user id and the id of its active emote set (what the EventAPI subscribes to).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SevenTvIds {
    pub user_id: String,
    pub set_id: String,
}

/// Where a channel's 7TV emote set is linked from: 7TV keeps one set per linked Twitch or Kick account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SevenTvPlatform {
    Twitch,
    Kick,
}

impl SevenTvPlatform {
    /// The platform segment of 7TV's `/v3/users/<platform>/<id>`.
    fn path(self) -> &'static str {
        match self {
            SevenTvPlatform::Twitch => "twitch",
            SevenTvPlatform::Kick => "kick",
        }
    }
}

/// Cache key of a channel's 7TV answer. Twitch and Kick ids are both plain numbers, so Kick's carry a prefix
/// (a Twitch key keeps the shape it always had, so existing caches stay valid).
pub fn seven_tv_channel_key_on(platform: SevenTvPlatform, channel_id: &str) -> String {
    match platform {
        SevenTvPlatform::Twitch => format!("emotes:7tv:channel:{channel_id}"),
        SevenTvPlatform::Kick => format!("emotes:7tv:channel:kick:{channel_id}"),
    }
}

pub fn seven_tv_channel_key(channel_id: &str) -> String {
    seven_tv_channel_key_on(SevenTvPlatform::Twitch, channel_id)
}

/// 7TV's lookup of the account linked to a Twitch user id or a Kick user id (the broadcaster's, not the channel's).
pub fn seven_tv_user_url(platform: SevenTvPlatform, channel_id: &str) -> String {
    format!("https://7tv.io/v3/users/{}/{channel_id}", platform.path())
}

/// The ids in a `/v3/users/<platform>/<id>` answer.
fn seven_tv_ids(c: &Value) -> Option<SevenTvIds> {
    let user_id = c.pointer("/user/id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let set_id = c.pointer("/emote_set/id").or_else(|| c.get("emote_set_id")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    (!user_id.is_empty() || !set_id.is_empty()).then_some(SevenTvIds { user_id, set_id })
}

/// 7TV global + a Twitch channel's emotes and its 7TV ids. `fresh` reloads the channel past its cache (a live
/// update said it changed).
pub async fn fetch_seven_tv(client: &reqwest::Client, channel_id: &str, fresh: bool) -> (ProviderScopedMap, Option<SevenTvIds>) {
    seven_tv_sets(client, channel_id, if fresh { CacheMode::Fresh } else { CacheMode::Normal }).await
}

/// 7TV global + the emotes of the 7TV set linked to a Twitch or Kick account, and its 7TV ids.
pub async fn fetch_seven_tv_on(client: &reqwest::Client, platform: SevenTvPlatform, channel_id: &str, fresh: bool) -> (ProviderScopedMap, Option<SevenTvIds>) {
    seven_tv_sets_on(client, platform, channel_id, if fresh { CacheMode::Fresh } else { CacheMode::Normal }).await
}

pub async fn seven_tv_sets(client: &reqwest::Client, channel_id: &str, mode: CacheMode) -> (ProviderScopedMap, Option<SevenTvIds>) {
    seven_tv_sets_on(client, SevenTvPlatform::Twitch, channel_id, mode).await
}

pub async fn seven_tv_sets_on(client: &reqwest::Client, platform: SevenTvPlatform, channel_id: &str, mode: CacheMode) -> (ProviderScopedMap, Option<SevenTvIds>) {
    let (ck, cu) = (seven_tv_channel_key_on(platform, channel_id), seven_tv_user_url(platform, channel_id));
    // a live update reloads the channel's set only
    let global_mode = if mode == CacheMode::Fresh { CacheMode::Normal } else { mode };
    let (g, c) = tokio::join!(
        fetch_json_mode(client, "emotes:7tv:global:global", EMOTE_GLOBAL_TTL_MS, "https://7tv.io/v3/emote-sets/global", global_mode),
        fetch_json_mode(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu, mode),
    );
    let (channel, ids) = c.as_ref().map(seven_tv_channel).unwrap_or_default();
    (ProviderScopedMap { global: g.map(|g| seven_tv_map(g.get("emotes"))).unwrap_or_default(), channel }, ids)
}

/// A `/v3/users/<platform>/<id>` answer -> the linked account's channel emotes and its 7TV ids.
pub fn seven_tv_channel(c: &Value) -> (EmoteMap, Option<SevenTvIds>) {
    (seven_tv_map(c.pointer("/emote_set/emotes")), seven_tv_ids(c))
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

fn bttv_channel_key(channel_id: &str) -> String {
    format!("emotes:bttv:channel:{channel_id}")
}

pub async fn fetch_bttv(client: &reqwest::Client, channel_id: &str) -> ProviderScopedMap {
    bttv_sets(client, channel_id, CacheMode::Normal).await
}

pub async fn bttv_sets(client: &reqwest::Client, channel_id: &str, mode: CacheMode) -> ProviderScopedMap {
    let (ck, cu) = (bttv_channel_key(channel_id), format!("https://api.betterttv.net/3/cached/users/twitch/{channel_id}"));
    let (g, c) = tokio::join!(
        fetch_json_mode(client, "emotes:bttv:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.betterttv.net/3/cached/emotes/global", mode),
        fetch_json_mode(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu, mode),
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

fn ffz_channel_key(channel_id: &str) -> String {
    format!("emotes:ffz:channel:{channel_id}")
}

pub async fn fetch_ffz(client: &reqwest::Client, channel_id: &str) -> ProviderScopedMap {
    ffz_sets(client, channel_id, CacheMode::Normal).await
}

pub async fn ffz_sets(client: &reqwest::Client, channel_id: &str, mode: CacheMode) -> ProviderScopedMap {
    let (ck, cu) = (ffz_channel_key(channel_id), format!("https://api.frankerfacez.com/v1/room/id/{channel_id}"));
    let (g, r) = tokio::join!(
        fetch_json_mode(client, "emotes:ffz:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.frankerfacez.com/v1/set/global", mode),
        fetch_json_mode(client, &ck, EMOTE_CHANNEL_TTL_MS, &cu, mode),
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
        cache_ready().await;
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

/// Picker / autocomplete ONLY - never merged into the message-rendering map; incoming messages already carry
/// Twitch emote positions from the IRC tag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TwitchPickerEmote {
    pub id: String,
    pub name: String,
    pub url: String,
}

/// `other` = emotes from the user's subscriptions to other channels (local relay only).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TwitchEmotesResp {
    pub channel: Vec<TwitchPickerEmote>,
    pub global: Vec<TwitchPickerEmote>,
    pub other: Vec<TwitchPickerEmote>,
}

fn picker_list(v: Option<&Value>) -> Vec<TwitchPickerEmote> {
    let sv = |e: &Value, k: &str| e.get(k).map(|x| x.as_str().map(|s| s.to_string()).unwrap_or_else(|| x.to_string())).unwrap_or_default();
    v.and_then(|a| a.as_array()).map(|a| a.iter().map(|e| TwitchPickerEmote { id: sv(e, "id"), name: sv(e, "name"), url: sv(e, "url") }).collect()).unwrap_or_default()
}

fn picker_json(r: &TwitchEmotesResp) -> Value {
    let l = |v: &Vec<TwitchPickerEmote>| Value::Array(v.iter().map(|e| json!({"id": e.id, "name": e.name, "url": e.url})).collect());
    json!({"channel": l(&r.channel), "global": l(&r.global), "other": l(&r.other)})
}

/// fetchTwitchPickerEmotes: the relay's `/api/chat/twitch-emotes` (cached like the site, 6 h).
pub async fn fetch_twitch_picker_emotes(bridge: &Arc<dyn agu_chat_core::protocol::AguBridge>, source: &agu_chat_core::protocol::SourceRef, channel_id: &str) -> TwitchEmotesResp {
    let key = format!("emotes:twitch:channel:{channel_id}");
    cache_ready().await;
    if let Some(c) = read_cache(&key, EMOTE_GLOBAL_TTL_MS) {
        return TwitchEmotesResp { channel: picker_list(c.get("channel")), global: picker_list(c.get("global")), other: picker_list(c.get("other")) };
    }
    let r = crate::chat::mod_caps::relay_api(bridge, source, "/api/chat/twitch-emotes", None, None).await;
    if !r.ok {
        return TwitchEmotesResp::default();
    }
    let resp = TwitchEmotesResp { channel: picker_list(r.body.get("channel")), global: picker_list(r.body.get("global")), other: picker_list(r.body.get("other")) };
    write_cache(&key, &picker_json(&resp));
    resp
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kick_channels_look_up_their_kick_linked_7tv_set() {
        assert_eq!(seven_tv_user_url(SevenTvPlatform::Kick, "676"), "https://7tv.io/v3/users/kick/676");
        assert_eq!(seven_tv_user_url(SevenTvPlatform::Twitch, "71092938"), "https://7tv.io/v3/users/twitch/71092938");
        // Kick and Twitch ids are both numbers: their cache entries must not collide; Twitch keeps its old key
        assert_eq!(seven_tv_channel_key_on(SevenTvPlatform::Kick, "676"), "emotes:7tv:channel:kick:676");
        assert_eq!(seven_tv_channel_key_on(SevenTvPlatform::Twitch, "676"), "emotes:7tv:channel:676");
        assert_eq!(seven_tv_channel_key("676"), seven_tv_channel_key_on(SevenTvPlatform::Twitch, "676"));

        // a real answer for a Kick channel (2026-11-18, trimmed to three emotes)
        let c: Value = serde_json::from_str(include_str!("fixtures/seven_tv_kick_user.json")).unwrap();
        let (map, ids) = seven_tv_channel(&c);
        assert_eq!(ids, Some(SevenTvIds { user_id: "01FE9DRF000009TR6M9N941CYW".into(), set_id: "01FE9DRF000009TR6M9N941CYW".into() }));
        let lo = &map["LO"];
        assert_eq!(lo.provider, EmoteProvider::SevenTv);
        assert!(!lo.zero_width);
        assert_eq!(provider_emote_url(lo, SizeOpt::Medium), format!("https://cdn.7tv.app/emote/{}/2x.webp", lo.id));
        assert!(map["notL"].zero_width);
        assert_eq!(map.len(), 3);
        assert_eq!(seven_tv_channel(&json!({})), (EmoteMap::new(), None));
    }

    #[test]
    fn seven_tv_flags_and_urls() {
        let m = seven_tv_map(Some(&json!([{"id": "abc", "name": "ZW", "flags": 1, "data": {"animated": true}}, {"id": "d", "name": "N", "flags": 0, "data": {"flags": 256}}])));
        assert!(m["ZW"].zero_width && m["ZW"].animated);
        assert!(m["N"].zero_width);
        assert_eq!(provider_emote_url(&m["ZW"], SizeOpt::Medium), "https://cdn.7tv.app/emote/abc/2x.webp");
        assert_eq!(provider_tooltip_url(&m["ZW"]), "https://cdn.7tv.app/emote/abc/4x.webp");
    }

    #[test]
    fn seven_tv_channel_ids() {
        let c = json!({"id": "71092938", "emote_set_id": "SET", "emote_set": {"id": "SET", "emotes": []}, "user": {"id": "USER"}});
        assert_eq!(seven_tv_ids(&c), Some(SevenTvIds { user_id: "USER".into(), set_id: "SET".into() }));
        assert_eq!(seven_tv_ids(&json!({"emote_set": null})), None);
    }

    #[test]
    fn forgotten_entries_are_not_read() {
        write_cache("test:forget", &json!(1));
        assert_eq!(read_cache("test:forget", EMOTE_CHANNEL_TTL_MS), Some(json!(1)));
        forget_cache("test:forget");
        assert_eq!(read_cache("test:forget", EMOTE_CHANNEL_TTL_MS), None);
    }

    fn put_aged(key: &str, data: Value, age_ms: i64) {
        cache().lock().unwrap().map.insert(key.to_string(), json!({"v": 1, "at": now_ms() - age_ms, "data": data}));
    }

    #[test]
    fn a_stale_entry_is_still_there_for_startup_and_fallback() {
        put_aged("test:stale", json!([1]), EMOTE_CHANNEL_TTL_MS + 1000);
        assert_eq!(read_cache("test:stale", EMOTE_CHANNEL_TTL_MS), None);
        assert!(!cache_fresh("test:stale", EMOTE_CHANNEL_TTL_MS));
        assert_eq!(read_cache_any("test:stale"), Some(json!([1])));
        // still fresh under the longer global rule
        assert!(cache_fresh("test:stale", EMOTE_GLOBAL_TTL_MS));
    }

    #[test]
    fn loading_the_file_prunes_old_and_broken_entries() {
        let now = 10 * CACHE_PRUNE_MS;
        let file = json!({
            "keep": {"v": 1, "at": now - EMOTE_CHANNEL_TTL_MS * 5, "data": 1},
            "old": {"v": 1, "at": now - CACHE_PRUNE_MS - 1, "data": 2},
            "broken": {"v": 1, "data": 3},
        });
        let m = parse_cache_file(&serde_json::to_vec(&file).unwrap(), now);
        assert_eq!(m.keys().map(|k| k.as_str()).collect::<Vec<_>>(), vec!["keep"]);
        assert!(parse_cache_file(b"not json", now).is_empty());
        assert!(parse_cache_file(b"[1,2]", now).is_empty());
    }

    #[test]
    fn retries_back_off_and_stop() {
        let d: Vec<u64> = (0..20).map_while(set_retry_delay).map(|d| d.as_secs()).collect();
        assert_eq!(d.first(), Some(&3));
        assert!(d.windows(2).all(|w| w[0] < w[1]));
        assert!(d.len() >= 6 && d.len() < 20);
    }

    #[test]
    fn set_freshness_needs_every_provider() {
        let cid = "test-fresh-1";
        assert!(!twitch_sets_cached(cid));
        for (k, _) in twitch_set_keys(cid) {
            write_cache(&k, &Value::Null);
        }
        assert!(twitch_sets_fresh(cid));
        // the 7TV channel answer failed to refresh: only its old copy is left
        put_aged(&seven_tv_channel_key(cid), json!({}), EMOTE_CHANNEL_TTL_MS + 1);
        assert!(!twitch_sets_fresh(cid));
        assert!(twitch_sets_cached(cid));
    }

    #[test]
    fn startup_shows_cached_sets_however_old_without_the_network() {
        let cid = "test-startup-1";
        put_aged(&seven_tv_channel_key(cid), json!({"user": {"id": "U"}, "emote_set": {"id": "S", "emotes": [{"id": "e1", "name": "catJAM"}]}}), 30 * EMOTE_CHANNEL_TTL_MS);
        put_aged(&bttv_channel_key(cid), json!({"channelEmotes": [{"id": "b1", "code": "OMEGALUL"}], "sharedEmotes": []}), 30 * EMOTE_CHANNEL_TTL_MS);
        // a CacheOnly load never touches the network
        let client = reqwest::Client::builder().build().unwrap();
        let (seven, ids) = futures::executor::block_on(seven_tv_sets(&client, cid, CacheMode::CacheOnly));
        assert!(seven.channel.contains_key("catJAM"));
        assert_eq!(ids, Some(SevenTvIds { user_id: "U".into(), set_id: "S".into() }));
        let bttv = futures::executor::block_on(bttv_sets(&client, cid, CacheMode::CacheOnly));
        assert!(bttv.channel.contains_key("OMEGALUL"));
        let ffz = futures::executor::block_on(ffz_sets(&client, cid, CacheMode::CacheOnly));
        assert!(ffz.channel.is_empty());
    }

    #[test]
    fn nothing_waits_on_a_cache_that_was_never_pointed_at_a_file() {
        futures::executor::block_on(cache_ready());
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
