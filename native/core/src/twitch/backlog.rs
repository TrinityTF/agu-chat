//! Port of src/main/twitch/backlog.js: chat history for a freshly opened source. recent-messages.robotty.de
//! keeps the last few hundred raw IRC lines of a channel. We shape them into the same message / notice
//! objects the live path builds, marked `historical: true` so the UI can dim them.
//! The service is a courtesy: down, slow or empty just means no history.
use std::time::Duration;

use serde_json::{json, Value};

use super::http::{request_json, ReqOpts};
use super::rt::log;
use super::vendor::irc::{build_chat_msg, build_notice, parse_irc_line};
use crate::util::{js_int_or, now_ms};

const BASE: &str = "https://recent-messages.robotty.de/api/v2/recent-messages";

/// Tries per channel. At startup the app fires one request per open channel while the network and the
/// service are both cold; a single 8 s attempt used to give up for good and leave the chat empty until the
/// app was reopened.
const MAX_ATTEMPTS: u32 = 4;

/// A failed request is worth another try when it never reached the service or the service itself failed.
pub fn is_transient(status: u16) -> bool {
    status == 0 || status == 408 || status == 429 || status >= 500
}

/// Wait before attempt `attempt + 1` (attempt counts from 0): `base`, then 2x, 4x, capped at 15 s, plus a
/// little spread so several channels do not hit the service in the same instant. None = out of attempts.
pub fn retry_delay_ms(attempt: u32, base_ms: u64, spread: u64) -> Option<u64> {
    if attempt + 1 >= MAX_ATTEMPTS {
        return None;
    }
    Some((base_ms << attempt.min(8)).min(15_000) + spread % 500)
}

/// Raw lines for a channel, or [] once every try failed. Retries with backoff when the service is unreachable
/// or failing, and when it says it only just started tracking the channel.
pub async fn fetch_recent(login: &str, limit: usize, retry_delay_ms_base: u64) -> Vec<String> {
    if std::env::var("AGU_IRC_URL").map(|v| !v.is_empty()).unwrap_or(false) {
        return Vec::new(); // benchmark mode: no backlog, identical load for both apps
    }
    let url = format!("{BASE}/{}?limit={limit}", url::form_urlencoded::byte_serialize(login.as_bytes()).collect::<String>());
    for attempt in 0..MAX_ATTEMPTS {
        match request_json(&url, ReqOpts { timeout_ms: 10_000, ..Default::default() }).await {
            Ok(data) => {
                let lines: Vec<String> = data["messages"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
                let has_error = data.get("error_code").map(crate::util::truthy).unwrap_or(false);
                if !lines.is_empty() || !has_error || attempt > 0 {
                    return lines;
                }
            }
            Err(err) => {
                log(&format!("recent-messages unavailable for {login} (try {}): {}", attempt + 1, err.message));
                if !is_transient(err.status) {
                    return Vec::new();
                }
            }
        }
        let Some(wait) = retry_delay_ms(attempt, retry_delay_ms_base, rand::random::<u64>()) else { break };
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
    Vec::new()
}

/// Raw lines -> (entries, room id). Entries are chat messages and `{...notice, notice:true}` rows in time order.
pub fn shape_recent(lines: &[String]) -> (Vec<Value>, Option<String>) {
    let mut entries: Vec<Value> = Vec::new();
    let mut room_id = None;
    for line in lines {
        let m = parse_irc_line(line);
        if !m.tag("room-id").is_empty() {
            room_id = Some(m.tag("room-id").to_string());
        }
        let mut stamp = js_int_or(m.tag("tmi-sent-ts"), 0);
        if stamp == 0 {
            stamp = js_int_or(m.tag("rm-received-ts"), now_ms());
        }
        match m.command.as_str() {
            "PRIVMSG" => {
                let display = if m.tag("display-name").is_empty() { m.user.as_str() } else { m.tag("display-name") };
                let mut msg = build_chat_msg(&m, display);
                msg["ts"] = json!(stamp);
                msg["historical"] = json!(true);
                entries.push(msg);
            }
            "USERNOTICE" => {
                if let Some(mut notice) = build_notice(&m) {
                    notice["ts"] = json!(stamp);
                    notice["historical"] = json!(true);
                    notice["notice"] = json!(true);
                    entries.push(notice);
                }
            }
            "CLEARCHAT" => {
                let user_id = Some(m.tag("target-user-id")).filter(|s| !s.is_empty());
                for e in entries.iter_mut() {
                    if e.get("notice").is_none() && user_id.map(|u| e["user"]["id"].as_str() == Some(u)).unwrap_or(true) {
                        e["deleted"] = json!(true);
                    }
                }
            }
            "CLEARMSG" => {
                let id = m.tag("target-msg-id");
                if !id.is_empty() {
                    if let Some(hit) = entries.iter_mut().find(|e| e["id"].as_str() == Some(id)) {
                        hit["deleted"] = json!(true);
                    }
                }
            }
            _ => {}
        }
    }
    (entries, room_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_statuses() {
        assert!(is_transient(0) && is_transient(503) && is_transient(429));
        assert!(!is_transient(404) && !is_transient(400));
    }

    #[test]
    fn backoff_grows_and_ends() {
        assert_eq!(retry_delay_ms(0, 2000, 0), Some(2000));
        assert_eq!(retry_delay_ms(1, 2000, 0), Some(4000));
        assert_eq!(retry_delay_ms(2, 2000, 0), Some(8000));
        assert_eq!(retry_delay_ms(3, 2000, 0), None);
        assert_eq!(retry_delay_ms(2, 10_000, 0), Some(15_000));
        assert!(retry_delay_ms(0, 2000, 12345).unwrap() < 2500);
    }
}
