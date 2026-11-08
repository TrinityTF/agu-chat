//! What this app has seen done to a person in a channel (timeouts, bans, unbans, warnings) plus the moderator
//! comments typed on their card. Twitch only tells a moderator's token the broadcaster-side ban list, so the
//! card's history is built from the frames the chat already receives and kept in `modhistory.json` in the data
//! folder. It is local to this PC by design: nothing here is sent anywhere.
//!
//! Entries are capped per user, users per channel, and users unseen for 90 days are dropped (unless they carry a
//! comment). Writes are debounced onto a short-lived thread, so the UI thread never waits for the disk.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const MAX_ENTRIES: usize = 50;
pub const MAX_COMMENTS: usize = 50;
pub const MAX_USERS: usize = 1500;
pub const COMMENT_MAX_CHARS: usize = 500;
const KEEP_MS: i64 = 90 * 24 * 3600 * 1000;
/// The same action reaching us twice (IRC CLEARCHAT and the named EventSub entry, or a replayed backlog) is one.
const MERGE_MS: i64 = 20_000;
const FILE: &str = "modhistory.json";
const WRITE_DELAY_MS: u64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Timeout,
    Ban,
    Unban,
    Untimeout,
    Warn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: i64,
    pub kind: Kind,
    /// timeout length
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secs: Option<u64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// the moderator, when the frame named one
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    /// when it was written; also its id (kept unique per user)
    pub ts: i64,
    pub by: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UserHistory {
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub seen: i64,
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BanState {
    Banned { reason: String, by: String },
    TimedOut { until: i64, reason: String, by: String },
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ModHistory {
    #[serde(default)]
    v: u32,
    /// source key (lower case) -> user key -> history
    #[serde(default)]
    channels: HashMap<String, HashMap<String, UserHistory>>,
}

/// The key a person is filed under: their id, else their login (X and some Kick lines have no id).
pub fn user_key(id: &str, login: &str) -> String {
    if id.is_empty() { format!("~{}", login.to_lowercase()) } else { id.to_string() }
}

fn chan_key(sk: &str) -> String {
    sk.to_lowercase()
}

impl ModHistory {
    pub fn from_json(bytes: &[u8]) -> ModHistory {
        let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
        serde_json::from_slice(bytes).unwrap_or_default()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(&ModHistory { v: 1, channels: self.channels.clone() }).unwrap_or_default()
    }

    pub fn get(&self, sk: &str, ukey: &str) -> Option<&UserHistory> {
        self.channels.get(&chan_key(sk))?.get(ukey)
    }

    /// Files one action; false when nothing changed (a repeat of what is already there).
    pub fn record(&mut self, sk: &str, ukey: &str, login: &str, ev: Entry, now: i64) -> bool {
        let u = self.channels.entry(chan_key(sk)).or_default().entry(ukey.to_string()).or_default();
        u.seen = now.max(u.seen);
        if !login.is_empty() {
            u.login = login.to_string();
        }
        if let Some(e) = u.entries.iter_mut().find(|e| e.kind == ev.kind && (e.ts - ev.ts).abs() <= MERGE_MS && (e.by.is_empty() || ev.by.is_empty() || (e.ts == ev.ts && e.by == ev.by))) {
            let mut changed = false;
            if e.by.is_empty() && !ev.by.is_empty() {
                // the named entry is the authoritative one, so a replay of it lines up with this one
                e.by = ev.by;
                e.ts = ev.ts;
                changed = true;
            }
            if e.reason.is_empty() && !ev.reason.is_empty() {
                e.reason = ev.reason;
                changed = true;
            }
            if e.secs.is_none() && ev.secs.is_some() {
                e.secs = ev.secs;
                changed = true;
            }
            u.entries.sort_by_key(|e| e.ts);
            return changed;
        }
        let at = u.entries.partition_point(|e| e.ts <= ev.ts);
        u.entries.insert(at, ev);
        if u.entries.len() > MAX_ENTRIES {
            let cut = u.entries.len() - MAX_ENTRIES;
            u.entries.drain(..cut);
        }
        true
    }

    /// Adds a comment; false for an empty one.
    pub fn add_comment(&mut self, sk: &str, ukey: &str, login: &str, by: &str, text: &str, now: i64) -> bool {
        let text: String = text.trim().chars().take(COMMENT_MAX_CHARS).collect();
        if text.is_empty() {
            return false;
        }
        let u = self.channels.entry(chan_key(sk)).or_default().entry(ukey.to_string()).or_default();
        u.seen = now.max(u.seen);
        if !login.is_empty() {
            u.login = login.to_string();
        }
        let mut ts = now;
        while u.comments.iter().any(|c| c.ts == ts) {
            ts += 1;
        }
        u.comments.push(Comment { ts, by: by.to_string(), text });
        u.comments.sort_by_key(|c| c.ts);
        if u.comments.len() > MAX_COMMENTS {
            let cut = u.comments.len() - MAX_COMMENTS;
            u.comments.drain(..cut);
        }
        true
    }

    /// Removes the comment `id`, only when `by` wrote it.
    pub fn delete_comment(&mut self, sk: &str, ukey: &str, id: i64, by: &str) -> bool {
        let Some(u) = self.channels.get_mut(&chan_key(sk)).and_then(|c| c.get_mut(ukey)) else { return false };
        let n = u.comments.len();
        u.comments.retain(|c| !(c.ts == id && c.by.eq_ignore_ascii_case(by)));
        u.comments.len() != n
    }

    /// Drops users not seen for 90 days (a comment keeps its person) and the least recently seen beyond the cap.
    pub fn prune(&mut self, now: i64) {
        for users in self.channels.values_mut() {
            users.retain(|_, u| !u.comments.is_empty() || now - u.seen <= KEEP_MS);
            if users.len() > MAX_USERS {
                let mut loose: Vec<(i64, String)> = users.iter().filter(|(_, u)| u.comments.is_empty()).map(|(k, u)| (u.seen, k.clone())).collect();
                loose.sort();
                for (_, k) in loose.into_iter().take(users.len() - MAX_USERS) {
                    users.remove(&k);
                }
            }
        }
        self.channels.retain(|_, u| !u.is_empty());
    }
}

/// Whether the last ban or timeout on record still stands at `now`: not lifted by an unban / untimeout after it,
/// and a timeout not yet run out. A timeout of unknown length counts as over.
pub fn ban_state(entries: &[Entry], now: i64) -> Option<BanState> {
    let last = entries.iter().rev().find(|e| e.kind != Kind::Warn)?;
    match last.kind {
        Kind::Ban => Some(BanState::Banned { reason: last.reason.clone(), by: last.by.clone() }),
        Kind::Timeout => {
            let until = last.ts + last.secs? as i64 * 1000;
            (until > now).then(|| BanState::TimedOut { until, reason: last.reason.clone(), by: last.by.clone() })
        }
        _ => None,
    }
}

// ---- frames -> entries ----------------------------------------------------------------------------------

fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(x)) => x.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn from_record(rec: &Value, ts: i64) -> Option<(String, String, Entry)> {
    let kind = match rec.get("action").and_then(|a| a.as_str())? {
        "timeout" => Kind::Timeout,
        "ban" => Kind::Ban,
        "unban" => Kind::Unban,
        "untimeout" => Kind::Untimeout,
        "warn" => Kind::Warn,
        _ => return None,
    };
    let (id, login) = (s(rec, "userId"), s(rec, "login"));
    if id.is_empty() && login.is_empty() {
        return None;
    }
    let secs = rec.get("secs").and_then(|x| x.as_i64()).filter(|x| *x > 0).map(|x| x as u64);
    Some((id, login, Entry { ts, kind, secs, reason: s(rec, "reason"), by: s(rec, "by") }))
}

/// The one-person actions a chat frame carries: (user id, login, entry). Twitch: IRC CLEARCHAT (`clearchat`, no
/// moderator named) and the EventSub feed entries (`modfeed`, also replayed inside `chatlog`); Kick: `banned`.
pub fn events_from_frame(platform: agu_chat_core::protocol::Platform, ty: &str, f: &Value, now: i64) -> Vec<(String, String, Entry)> {
    use agu_chat_core::protocol::Platform;
    let mut out = Vec::new();
    match (platform, ty) {
        (Platform::Twitch, "clearchat") => {
            let (id, login) = (s(f, "userId"), s(f, "login"));
            if id.is_empty() && login.is_empty() {
                return out; // the whole chat was cleared
            }
            let secs = f.get("duration").and_then(|d| d.as_i64()).filter(|d| *d > 0).map(|d| d as u64);
            let kind = if secs.is_some() { Kind::Timeout } else { Kind::Ban };
            out.push((id, login, Entry { ts: now, kind, secs, reason: String::new(), by: String::new() }));
        }
        (Platform::Twitch, "modfeed") => {
            if let Some(feed) = f.get("feed") {
                if let Some(e) = feed.get("mod").and_then(|r| from_record(r, feed.get("ts").and_then(|t| t.as_i64()).unwrap_or(now))) {
                    out.push(e);
                }
            }
        }
        (Platform::Twitch, "chatlog") => {
            for feed in f.get("modfeed").and_then(|m| m.as_array()).into_iter().flatten() {
                if let Some(e) = feed.get("mod").and_then(|r| from_record(r, feed.get("ts").and_then(|t| t.as_i64()).unwrap_or(now))) {
                    out.push(e);
                }
            }
        }
        (Platform::Kick, "banned") => {
            let (id, login) = (s(f, "userId"), s(f, "username"));
            let permanent = f.get("permanent").and_then(|p| p.as_bool()).unwrap_or(true);
            let expires = f.get("expiresAt").and_then(|e| e.as_i64()).unwrap_or(0);
            let secs = (!permanent && expires > now).then(|| ((expires - now + 500) / 1000) as u64);
            let kind = if permanent { Kind::Ban } else { Kind::Timeout };
            out.push((id, login, Entry { ts: now, kind, secs, reason: s(f, "reason"), by: s(f, "by") }));
        }
        _ => {}
    }
    out
}

// ---- process-wide store ---------------------------------------------------------------------------------

struct Global {
    h: Mutex<ModHistory>,
    scheduled: AtomicBool,
    /// bumps on every change, so an open card knows to redraw
    version: AtomicU64,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn path() -> Option<PathBuf> {
    crate::window_state::data_dir().map(|d| d.join(FILE))
}

fn global() -> &'static Global {
    static G: OnceLock<Global> = OnceLock::new();
    G.get_or_init(|| {
        // a small file (capped, see the constants), read once
        let mut h = path().and_then(|p| std::fs::read(p).ok()).map(|b| ModHistory::from_json(&b)).unwrap_or_default();
        h.prune(now_ms());
        Global { h: Mutex::new(h), scheduled: AtomicBool::new(false), version: AtomicU64::new(0) }
    })
}

fn lock() -> std::sync::MutexGuard<'static, ModHistory> {
    global().h.lock().unwrap_or_else(|e| e.into_inner())
}

fn changed() {
    let g = global();
    g.version.fetch_add(1, Ordering::Relaxed);
    if !crate::window_state::writable() || g.scheduled.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_millis(WRITE_DELAY_MS));
        let g = global();
        g.scheduled.store(false, Ordering::Release);
        let json = {
            let mut h = lock();
            h.prune(now_ms());
            h.to_json()
        };
        if let Some(p) = path() {
            let _ = write_atomic(&p, json.as_bytes());
        }
    });
}

fn write_atomic(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, p)
}

pub fn version() -> u64 {
    global().version.load(Ordering::Relaxed)
}

/// Called for every frame the store reduces.
pub fn observe(sk: &str, ty: &str, frame: &Value) {
    let platform = super::store::platform_of_key(sk);
    let now = now_ms();
    let evs = events_from_frame(platform, ty, frame, now);
    if evs.is_empty() {
        return;
    }
    let mut any = false;
    {
        let mut h = lock();
        for (id, login, e) in evs {
            any |= h.record(sk, &user_key(&id, &login), &login, e, now);
        }
    }
    if any {
        changed();
    }
}

pub fn snapshot(sk: &str, ukey: &str) -> UserHistory {
    lock().get(sk, ukey).cloned().unwrap_or_default()
}

pub fn add_comment(sk: &str, ukey: &str, login: &str, by: &str, text: &str) -> bool {
    let ok = lock().add_comment(sk, ukey, login, by, text, now_ms());
    if ok {
        changed();
    }
    ok
}

pub fn delete_comment(sk: &str, ukey: &str, id: i64, by: &str) -> bool {
    let ok = lock().delete_comment(sk, ukey, id, by);
    if ok {
        changed();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Platform;
    use serde_json::json;

    fn e(ts: i64, kind: Kind, secs: Option<u64>, by: &str) -> Entry {
        Entry { ts, kind, secs, reason: String::new(), by: by.into() }
    }

    #[test]
    fn records_and_caps_entries() {
        let mut h = ModHistory::default();
        for i in 0..70 {
            h.record("twitch:a", "1", "bob", e(i * 100_000, Kind::Timeout, Some(10), "m"), 0);
        }
        let u = h.get("twitch:A", "1").unwrap();
        assert_eq!(u.entries.len(), MAX_ENTRIES);
        assert_eq!(u.entries[0].ts, 20 * 100_000);
        assert_eq!(u.login, "bob");
    }

    #[test]
    fn the_same_action_from_two_frames_is_one() {
        let mut h = ModHistory::default();
        assert!(h.record("twitch:a", "1", "bob", e(1000, Kind::Timeout, Some(60), ""), 0));
        // the named EventSub entry arrives a moment later: fills in the moderator
        assert!(h.record("twitch:a", "1", "bob", Entry { reason: "spam".into(), ..e(3000, Kind::Timeout, Some(60), "Mod") }, 0));
        // and a replay of it changes nothing
        assert!(!h.record("twitch:a", "1", "bob", Entry { reason: "spam".into(), ..e(3000, Kind::Timeout, Some(60), "Mod") }, 0));
        let u = h.get("twitch:a", "1").unwrap();
        assert_eq!(u.entries.len(), 1);
        assert_eq!((u.entries[0].by.as_str(), u.entries[0].reason.as_str()), ("Mod", "spam"));
        // two timeouts by named moderators close together stay two
        assert!(h.record("twitch:a", "1", "bob", e(5000, Kind::Timeout, Some(600), "Other"), 0));
        assert_eq!(h.get("twitch:a", "1").unwrap().entries.len(), 2);
    }

    #[test]
    fn ban_state_follows_the_last_ban_or_timeout() {
        let now = 1_000_000;
        assert_eq!(ban_state(&[], now), None);
        let t = e(now - 10_000, Kind::Timeout, Some(60), "m");
        assert!(matches!(ban_state(&[t.clone()], now), Some(BanState::TimedOut { until, .. }) if until == now + 50_000));
        assert_eq!(ban_state(&[t.clone()], now + 60_000), None, "run out");
        let lift = e(now - 5000, Kind::Untimeout, None, "m");
        assert_eq!(ban_state(&[t.clone(), lift], now), None);
        let ban = e(now - 4000, Kind::Ban, None, "m");
        assert!(matches!(ban_state(&[t.clone(), ban.clone()], now), Some(BanState::Banned { .. })));
        let unban = e(now - 1000, Kind::Unban, None, "m");
        assert_eq!(ban_state(&[t.clone(), ban, unban], now), None);
        // a warning after a ban does not lift it
        let w = e(now - 500, Kind::Warn, None, "m");
        assert!(matches!(ban_state(&[e(now - 9000, Kind::Ban, None, ""), w], now), Some(BanState::Banned { .. })));
        assert_eq!(ban_state(&[e(now - 9000, Kind::Timeout, None, "")], now), None, "unknown length counts as over");
    }

    #[test]
    fn comments_add_delete_and_keep_the_person() {
        let mut h = ModHistory::default();
        assert!(!h.add_comment("twitch:a", "1", "bob", "me", "   ", 10));
        assert!(h.add_comment("twitch:a", "1", "bob", "me", " first ", 10));
        assert!(h.add_comment("twitch:a", "1", "bob", "other", "second", 10));
        let ids: Vec<i64> = h.get("twitch:a", "1").unwrap().comments.iter().map(|c| c.ts).collect();
        assert_eq!(ids, vec![10, 11]);
        assert_eq!(h.get("twitch:a", "1").unwrap().comments[0].text, "first");
        assert!(!h.delete_comment("twitch:a", "1", 11, "me"), "not mine");
        assert!(h.delete_comment("twitch:a", "1", 10, "ME"));
        assert_eq!(h.get("twitch:a", "1").unwrap().comments.len(), 1);
        // pruning 100 days later keeps the commented person, drops an uncommented one
        h.record("twitch:a", "2", "x", e(1, Kind::Ban, None, ""), 1);
        h.prune(100 * 24 * 3600 * 1000);
        assert!(h.get("twitch:a", "1").is_some());
        assert!(h.get("twitch:a", "2").is_none());
    }

    #[test]
    fn prune_caps_users_by_last_seen() {
        let mut h = ModHistory::default();
        for i in 0..(MAX_USERS + 5) {
            h.record("twitch:a", &i.to_string(), "", e(i as i64, Kind::Ban, None, ""), i as i64);
        }
        h.prune(10_000);
        let users = &h.channels["twitch:a"];
        assert_eq!(users.len(), MAX_USERS);
        assert!(!users.contains_key("0") && users.contains_key(&(MAX_USERS + 4).to_string()));
    }

    #[test]
    fn survives_a_file_round_trip() {
        let mut h = ModHistory::default();
        h.record("twitch:a", "1", "bob", Entry { reason: "spam".into(), ..e(5, Kind::Timeout, Some(600), "Mod") }, 5);
        h.add_comment("twitch:a", "1", "bob", "me", "hi", 6);
        let p = std::env::temp_dir().join(format!("agu-modhistory-test-{}.json", std::process::id()));
        write_atomic(&p, h.to_json().as_bytes()).unwrap();
        let back = ModHistory::from_json(&std::fs::read(&p).unwrap());
        let _ = std::fs::remove_file(&p);
        assert_eq!(back.get("twitch:a", "1"), h.get("twitch:a", "1"));
        assert!(ModHistory::from_json(b"not json").channels.is_empty());
    }

    #[test]
    fn frames_become_entries() {
        let f = json!({"userId": "7", "login": "bob", "duration": 600});
        let v = events_from_frame(Platform::Twitch, "clearchat", &f, 99);
        assert_eq!((v[0].0.as_str(), v[0].2.kind, v[0].2.secs), ("7", Kind::Timeout, Some(600)));
        assert_eq!(events_from_frame(Platform::Twitch, "clearchat", &json!({"duration": null}), 99).len(), 0);
        let perma = events_from_frame(Platform::Twitch, "clearchat", &json!({"userId": "7", "login": "bob", "duration": null}), 99);
        assert_eq!(perma[0].2.kind, Kind::Ban);
        let feed = json!({"ts": 50, "kind": "ban", "text": "x", "mod": {"action": "unban", "by": "Mod", "userId": "7", "login": "bob", "secs": null, "reason": ""}});
        let v = events_from_frame(Platform::Twitch, "modfeed", &json!({"feed": feed.clone()}), 99);
        assert_eq!((v[0].2.ts, v[0].2.kind, v[0].2.by.as_str()), (50, Kind::Unban, "Mod"));
        assert_eq!(events_from_frame(Platform::Twitch, "chatlog", &json!({"modfeed": [feed, {"ts": 1, "kind": "mode", "text": "x"}]}), 99).len(), 1);
        let k = events_from_frame(Platform::Kick, "banned", &json!({"userId": "9", "username": "k", "by": "mod", "reason": "", "permanent": false, "expiresAt": 100_000}), 40_000);
        assert_eq!((k[0].2.kind, k[0].2.secs), (Kind::Timeout, Some(60)));
        let k = events_from_frame(Platform::Kick, "banned", &json!({"userId": "9", "username": "k", "permanent": true, "expiresAt": 0}), 40_000);
        assert_eq!(k[0].2.kind, Kind::Ban);
    }
}
