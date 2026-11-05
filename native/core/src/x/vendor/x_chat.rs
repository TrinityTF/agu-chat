//! Port of src/main/x/vendor/xChat.js (render lib/xChat.js): X (Twitter) broadcast chat, READ-ONLY.
//!
//! x.com's live chat reads one streaming endpoint:
//!   GET x.com/i/api/live-chat?broadcastId=<id>&sessionId=<random>[&cursor=<ts>]
//! It answers with newline-delimited JSON: first a backfill of recent messages ({isBackfill: true}, ~100),
//! then new ones as they are sent, and ends every so often; x.com reconnects a second later with the last
//! `ts` as the cursor, or 30 seconds later after an error. An event is {ts, chatType, userId, message}: `ts`
//! is epoch nanoseconds as a string, chatType 1 is a chat message, userId is the X account's rest id.
//!
//! A GUEST TOKEN IS ENOUGH to read it. NAMES need a login (the lookup x.com uses answers 404 to a guest), so
//! messages arrive under a placeholder name made from the last digits of the user id.
//!
//! Differences from the JS: the `fetch`/`guestToken` injection points are gone, `onEvent` is a closure
//! argument, and the request gets a 30 s header deadline (JS had none; a hang there would stall the loop).
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::x_live::{guest_token, WEB_BEARER};
use crate::net::{self, FetchOpts};
use crate::util::{js_number, js_string, js_trim, now_ms, truthy};
use crate::x::ua::get_user_agent;

// Both hosts x.com serves the endpoint on; the second is tried when the first fails outright.
const LIVE_CHAT_URLS: [&str; 2] = ["https://x.com/i/api/live-chat", "https://api.x.com/live-chat"];
const HEADER_TIMEOUT_MS: u64 = 30_000;

/// What `readLiveChat` threw: an Error with `.message` and, when X answered, `.status`.
#[derive(Clone, Debug)]
pub struct XError {
    pub message: String,
    pub status: Option<u16>,
}

impl std::fmt::Display for XError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn err(m: impl Into<String>) -> XError {
    XError { message: m.into(), status: None }
}

/// X's nanosecond `ts` string -> epoch milliseconds.
pub fn ts_to_ms(ts: &str) -> i64 {
    // JS slices UTF-16 units; the string is digits.
    if ts.chars().count() > 6 {
        let head: String = ts.chars().take(ts.chars().count() - 6).collect();
        let n = js_number(&head);
        if n.is_nan() || n == 0.0 { 0 } else { n as i64 }
    } else {
        0
    }
}

/// What a message shows for someone whose name is not known (a guest cannot look names up).
pub fn placeholder_name(user_id: &str) -> String {
    let chars: Vec<char> = user_id.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(5)..].iter().collect();
    format!("X user {tail}")
}

/// One live-chat event -> a chat message, or None for everything else. Only chatType 1 with text is a
/// message; the stream also carries moderator lists, can/cannot-chat notices and mutes.
pub fn to_x_chat_msg(ev: &Value) -> Option<Value> {
    if !ev.is_object() || ev["chatType"] != 1 {
        return None;
    }
    let text = ev["message"].as_str().unwrap_or("");
    if js_trim(text).is_empty() || !truthy(&ev["userId"]) || !truthy(&ev["ts"]) {
        return None;
    }
    let (ts, uid) = (js_string(&ev["ts"]), js_string(&ev["userId"]));
    let ms = ts_to_ms(&ts);
    Some(json!({
        "platform": "x",
        // `ts` is what x.com itself uses as the message id; the user id makes two messages in the same nanosecond still differ.
        "id": format!("{ts}:{uid}"),
        "ts": if ms != 0 { ms } else { now_ms() },
        "user": {
            "id": uid,
            "name": placeholder_name(&uid),
            "login": "",
            "avatar": "",
            "twitterId": uid,
        },
        "text": text,
    }))
}

/// Read one connection of X's live-chat stream, calling `on_event` per line, until X ends it. Errs (with
/// `.status` when X answered) when it cannot be read.
pub async fn read_live_chat(broadcast_id: &str, cursor: &str, mut on_event: impl FnMut(Value) + Send) -> Result<(), XError> {
    let session_id: u32 = rand::random();
    let enc = |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
    let mut q = format!("?broadcastId={}&sessionId={session_id}", enc(broadcast_id));
    if !cursor.is_empty() {
        q.push_str(&format!("&cursor={}", enc(cursor)));
    }

    let mut res: Option<reqwest::Response> = None;
    let mut failure: Option<XError> = None;
    for base in LIVE_CHAT_URLS {
        let token = guest_token(false).await.map_err(err)?;
        let o = FetchOpts::new(HEADER_TIMEOUT_MS)
            .header("authorization", &format!("Bearer {WEB_BEARER}"))
            .header("x-guest-token", &token)
            .header("user-agent", &get_user_agent())
            .header("x-twitter-active-user", "yes")
            .header("x-twitter-client-language", "en");
        match net::fetch(&format!("{base}{q}"), o).await {
            Err(e) => {
                failure = Some(err(e.message));
                continue;
            }
            Ok(r) if r.status().is_success() => {
                res = Some(r);
                break;
            }
            Ok(r) => {
                let status = r.status().as_u16();
                failure = Some(XError { message: format!("live-chat {status}"), status: Some(status) });
                // A stale guest token shows up as 401/403; the next attempt gets a fresh one.
                if status == 401 || status == 403 {
                    let _ = guest_token(true).await;
                }
            }
        }
    }
    let Some(res) = res else { return Err(failure.unwrap_or_else(|| err("live-chat unreachable"))) };

    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        buf.extend_from_slice(&chunk.map_err(|e| err(e.to_string()))?);
        // lines = buf.split("\n"); buf = lines.pop()
        while let Some(i) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let t = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            let t = js_trim(&t);
            if t.is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<Value>(t) {
                on_event(ev);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_and_names() {
        assert_eq!(ts_to_ms("1794052800123456789"), 1_794_052_800_123);
        assert_eq!(ts_to_ms("123456"), 0);
        assert_eq!(ts_to_ms("1234567"), 1);
        assert_eq!(ts_to_ms(""), 0);
        assert_eq!(placeholder_name("1234567890"), "X user 67890");
        assert_eq!(placeholder_name("42"), "X user 42");
    }

    #[test]
    fn events_to_rows() {
        let ev = json!({"ts": "1794052800123456789", "chatType": 1, "userId": "1234567890", "message": "hello"});
        let m = to_x_chat_msg(&ev).unwrap();
        assert_eq!(m["id"], "1794052800123456789:1234567890");
        assert_eq!(m["ts"], 1_794_052_800_123i64);
        assert_eq!(m["user"], json!({"id": "1234567890", "name": "X user 67890", "login": "", "avatar": "", "twitterId": "1234567890"}));
        assert_eq!(m["text"], "hello");
        assert_eq!(m["platform"], "x");
        // numeric userId is stringified
        assert_eq!(to_x_chat_msg(&json!({"ts": "1794052800123456789", "chatType": 1, "userId": 99, "message": "x"})).unwrap()["user"]["id"], "99");
        // not messages
        assert!(to_x_chat_msg(&json!({"ts": "1", "chatType": 2, "userId": "1", "message": "x"})).is_none());
        assert!(to_x_chat_msg(&json!({"ts": "1", "chatType": 1, "userId": "1", "message": "   "})).is_none());
        assert!(to_x_chat_msg(&json!({"ts": "1", "chatType": 1, "userId": "", "message": "x"})).is_none());
        assert!(to_x_chat_msg(&json!({"chatType": 1, "userId": "1", "message": "x"})).is_none());
        assert!(to_x_chat_msg(&json!({"ts": "1", "chatType": 1, "userId": "1", "message": 5})).is_none());
        assert!(to_x_chat_msg(&json!("str")).is_none());
        // a short ts has no millisecond part: falls back to now
        let short = to_x_chat_msg(&json!({"ts": "12345", "chatType": 1, "userId": "1", "message": "x"})).unwrap();
        assert!(short["ts"].as_i64().unwrap() > 1_700_000_000_000);
    }
}
