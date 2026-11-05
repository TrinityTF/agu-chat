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

/// Raw lines for a channel, or [] on any failure. One retry when the service says it only just started
/// tracking the channel.
pub async fn fetch_recent(login: &str, limit: usize, retry_delay_ms: u64) -> Vec<String> {
    if std::env::var("AGU_IRC_URL").map(|v| !v.is_empty()).unwrap_or(false) {
        return Vec::new(); // benchmark mode: no backlog, identical load for both apps
    }
    for attempt in 0..2 {
        match request_json(&format!("{BASE}/{}?limit={limit}", url::form_urlencoded::byte_serialize(login.as_bytes()).collect::<String>()), ReqOpts { timeout_ms: 8000, ..Default::default() }).await {
            Ok(data) => {
                let lines: Vec<String> = data["messages"].as_array().map(|a| a.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
                let has_error = data.get("error_code").map(crate::util::truthy).unwrap_or(false);
                if !lines.is_empty() || !has_error || attempt > 0 {
                    return lines;
                }
            }
            Err(err) => {
                log(&format!("recent-messages unavailable for {login}: {}", err.message));
                return Vec::new();
            }
        }
        tokio::time::sleep(Duration::from_millis(retry_delay_ms)).await;
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
