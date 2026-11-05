//! Port of src/main/twitch/vendor/chatters.js (vendored from modtools.agu.lol render/lib/hiddenChatters.js,
//! render commit 7704aec): chatterKey, cleanName, editDistance, closeness, suggestChatters. Only those
//! functions (the hidden-chatters list/filter half is not needed here).
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::util::{js_str_or_empty, js_string, js_to_number, js_trim, len_utf16, slice_utf16};

const HIDDEN_PLATFORMS: [&str; 5] = ["twitch", "kick", "youtube", "rumble", "x"];

fn is_js_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

fn clean_name(name: &str) -> String {
    // name.trim().replace(/^@+/, "").replace(/\s+/g, " ").trim()
    let t = js_trim(name).trim_start_matches('@');
    let mut out = String::new();
    let mut in_space = false;
    for c in t.chars() {
        if is_js_space(c) {
            if !in_space {
                out.push(' ');
            }
            in_space = true;
        } else {
            out.push(c);
            in_space = false;
        }
    }
    js_trim(&out).to_string()
}

pub fn chatter_key(name: &Value) -> String {
    match name {
        Value::String(s) => clean_name(s).to_lowercase(),
        _ => String::new(),
    }
}

// Edit distance, given up past `max`: only "is this a typo of that" is asked.
fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let a: Vec<u16> = a.encode_utf16().collect();
    let b: Vec<u16> = b.encode_utf16().collect();
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i];
        let mut best = i;
        for j in 1..=b.len() {
            let v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
            cur.push(v);
            if v < best {
                best = v;
            }
        }
        if best > max {
            return max + 1;
        }
        prev = cur;
    }
    prev[b.len()]
}

// How close a chatter's key is to what was typed; lower is closer, None is not close at all. Exact, then
// starts-with, then contains, then a typo of it (the whole name, or its first q.length letters, a few edits away).
fn closeness(key: &str, q: &str) -> Option<usize> {
    if key.is_empty() {
        return None;
    }
    if key == q {
        return Some(0);
    }
    if key.starts_with(q) {
        return Some(1);
    }
    if key.contains(q) {
        return Some(2);
    }
    let ql = len_utf16(q);
    if ql < 3 {
        return None;
    }
    let max = if ql < 6 { 1 } else { 2 };
    let d = edit_distance(key, q, max).min(edit_distance(&slice_utf16(key, ql), q, max));
    if d <= max {
        Some(3 + d)
    } else {
        None
    }
}

pub struct Pool<'a> {
    pub platform: &'a str,
    pub messages: &'a [Value],
}

struct Best {
    score: usize,
    ts: f64,
    row: Value,
}

/// The chatters closest to `query` across the channel's chats, one row per account per platform: the same
/// name on two platforms is two rows. Within a closeness, whoever spoke most recently comes first.
///
/// `hide` is the name to store: the display name when it is just the login in other letters (reads better),
/// else the login (a localized display name can change; the login is what the account is).
pub fn suggest_chatters(query: &str, pools: &[Pool], limit: usize) -> Vec<Value> {
    let q = chatter_key(&Value::String(query.to_string()));
    if q.is_empty() {
        return Vec::new();
    }
    let mut best: Vec<Best> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for pool in pools {
        if !HIDDEN_PLATFORMS.contains(&pool.platform) {
            continue;
        }
        for m in pool.messages {
            let Some(u) = m.get("user").filter(|u| u.is_object()) else { continue };
            let login = chatter_key(&u["login"]);
            let name_key = chatter_key(&u["name"]);
            if login.is_empty() && name_key.is_empty() {
                continue;
            }
            let id = format!("{}|{}", pool.platform, if login.is_empty() { &name_key } else { &login });
            let Some(score) = [closeness(&login, &q), closeness(&name_key, &q)].into_iter().flatten().min() else { continue };
            let ts = {
                let n = js_to_number(m.get("ts"));
                if n.is_nan() {
                    0.0
                } else {
                    n
                }
            };
            let was = index.get(&id).map(|&i| &best[i]);
            if let Some(w) = was {
                if w.score <= score && w.ts >= ts {
                    continue;
                }
            }
            let shown = {
                let n = js_str_or_empty(u.get("name"));
                if n.is_empty() {
                    js_str_or_empty(u.get("login"))
                } else {
                    n
                }
            };
            let name = clean_name(&shown);
            let login_s = js_string(&u["login"]);
            let hide = if login.is_empty() || name_key == login { name.clone() } else { clean_name(&login_s) };
            let entry = Best {
                score: was.map(|w| w.score.min(score)).unwrap_or(score),
                ts: was.map(|w| w.ts.max(ts)).unwrap_or(ts),
                row: json!({
                    "name": name,
                    "login": if !login.is_empty() && login != name_key { clean_name(&login_s) } else { String::new() },
                    "hide": hide,
                    "platform": pool.platform,
                }),
            };
            match index.get(&id) {
                Some(&i) => best[i] = entry,
                None => {
                    index.insert(id, best.len());
                    best.push(entry);
                }
            }
        }
    }
    best.sort_by(|a, b| a.score.cmp(&b.score).then(b.ts.partial_cmp(&a.ts).unwrap_or(std::cmp::Ordering::Equal)));
    best.into_iter().take(limit).map(|b| b.row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_closest_first() {
        let msgs = vec![
            json!({"ts": 1, "user": {"login": "alice", "name": "Alice"}}),
            json!({"ts": 2, "user": {"login": "malice", "name": "Malice"}}),
            json!({"ts": 3, "user": {"login": "bob", "name": "Bob"}}),
            json!({"ts": 4, "user": {"login": "alyce", "name": "Alyce"}}),
        ];
        let out = suggest_chatters("alice", &[Pool { platform: "twitch", messages: &msgs }], 8);
        let names: Vec<&str> = out.iter().map(|r| r["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Alice", "Malice", "Alyce"]);
        assert_eq!(out[0]["login"], "");
        assert_eq!(out[0]["platform"], "twitch");
        assert!(suggest_chatters("", &[], 8).is_empty());
    }
}
