//! Port of src/renderer/chat/modActions.ts: the mod / send HTTP calls (delete, AutoMod resolve, pin, unpin, pin
//! duration, Rumble and Kick moderation) as async functions over the bridge. Each returns the toast to show
//! (text, warn) instead of calling a toast callback; the pane shows it. The `window.confirm` of the Rumble /
//! Kick ban is the pane's confirm bar, so these functions are only called once that was accepted.
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, SourceRef};
use serde_json::{json, Value};

use super::mod_buttons::format_duration;
use super::mod_caps::{relay_api, RelayResponse};
use super::types::{KickMsg, RumbleMsg, YouTubeMsg};

/// A toast to show: (text, warn).
pub type ToastMsg = Option<(String, bool)>;

fn ok_toast(t: impl Into<String>) -> ToastMsg {
    Some((t.into(), false))
}
fn warn(t: impl Into<String>) -> ToastMsg {
    Some((t.into(), true))
}

fn fail_text(r: &RelayResponse, fallback: &str) -> String {
    // Bodies from the local relay may be JSON ({error}); strip to the message.
    let msg = match &r.body {
        Value::Object(o) => o.get("error").map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.to_string())).unwrap_or_else(|| r.text()),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if r.status == 403 && msg == "relogin" {
        return "Log in again to grant mod permissions".into();
    }
    if msg.is_empty() { fallback.to_string() } else { msg }
}

/// POST /api/mod/action (Twitch). (success, toast)
pub async fn twitch_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, body: Value, ok: Option<String>) -> (bool, ToastMsg) {
    let r = relay_api(bridge, source, "/api/mod/action", Some("POST"), Some(body)).await;
    if !r.ok {
        return (false, warn(fail_text(&r, "action failed")));
    }
    (true, ok.and_then(ok_toast))
}

pub async fn twitch_delete(bridge: &Arc<dyn AguBridge>, source: &SourceRef, message_id: &str) -> (bool, ToastMsg) {
    twitch_action(bridge, source, json!({"action": "delete", "messageId": message_id}), None).await
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutomodResult {
    Ok,
    Gone,
    Fail,
}

/// Resolve an AutoMod hold. On success the row is left alone: the authoritative "automodupdate" broadcast
/// flips it. A 404 means Twitch already expired/resolved it, so the caller marks it locally (Gone).
pub async fn resolve_automod(bridge: &Arc<dyn AguBridge>, source: &SourceRef, id: &str, action: &str) -> (AutomodResult, ToastMsg) {
    let r = relay_api(bridge, source, "/api/mod/automod", Some("POST"), Some(json!({"msgId": id, "action": action}))).await;
    if r.ok {
        return (AutomodResult::Ok, None);
    }
    if r.status == 404 {
        return (AutomodResult::Gone, warn("Message is no longer held"));
    }
    (AutomodResult::Fail, warn(fail_text(&r, "action failed")))
}

pub async fn pin_message(bridge: &Arc<dyn AguBridge>, source: &SourceRef, id: &str) -> (bool, ToastMsg) {
    twitch_action(bridge, source, json!({"action": "pin", "messageId": id, "durationSeconds": 1200}), Some("Pinned".into())).await
}

pub async fn unpin_message(bridge: &Arc<dyn AguBridge>, source: &SourceRef, id: &str) -> (bool, ToastMsg) {
    twitch_action(bridge, source, json!({"action": "unpin", "messageId": id}), Some("unpinned".into())).await
}

pub async fn set_pin_duration(bridge: &Arc<dyn AguBridge>, source: &SourceRef, id: &str, seconds: Option<u32>) -> (bool, ToastMsg) {
    let mut body = json!({"action": "pinduration", "messageId": id});
    if let Some(s) = seconds {
        body["durationSeconds"] = json!(s);
    }
    let ok = if seconds.is_none() { "Pinned until unpinned or the stream ends" } else { "Pin time updated" };
    twitch_action(bridge, source, body, Some(ok.into())).await
}

/// Rumble moderation targets the PERSON (`user.login`), not the channel a message was posted under.
pub async fn rumble_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &RumbleMsg, seconds: u32) -> ToastMsg {
    let who = if msg.user.login.is_empty() { msg.user.name.clone() } else { msg.user.login.clone() };
    let body = if action == "delete" {
        json!({"action": action, "messageId": msg.id})
    } else if action == "timeout" {
        json!({"action": action, "username": who, "seconds": seconds})
    } else {
        json!({"action": action, "username": who})
    };
    let r = relay_api(bridge, source, "/api/rumble/action", Some("POST"), Some(body)).await;
    if !r.ok {
        let t = r.text();
        return warn(if t.is_empty() { "Rumble rejected the action".to_string() } else { t });
    }
    ok_toast(match action {
        "timeout" => format!("timed out {who} on Rumble ({})", format_duration(seconds as i64)),
        "ban" => format!("banned {who} on Rumble"),
        _ => "deleted on Rumble".to_string(),
    })
}

pub async fn kick_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &KickMsg, seconds: Option<u32>) -> ToastMsg {
    let body = json!({"action": action, "userId": msg.user.id, "messageId": msg.id, "seconds": seconds});
    let r = relay_api(bridge, source, "/api/kick/action", Some("POST"), Some(body)).await;
    if !r.ok {
        let t = r.text();
        return warn(format!("Kick: {}", if t.is_empty() { "action failed".to_string() } else { t }));
    }
    let who = if msg.user.name.is_empty() { "user".to_string() } else { msg.user.name.clone() };
    ok_toast(match action {
        "timeout" => format!("Timed out {who} on Kick"),
        "ban" => format!("Banned {who} on Kick"),
        "unban" => format!("Unbanned {who} on Kick"),
        _ => "Message deleted on Kick".to_string(),
    })
}

/// YouTube moderation asks YouTube's own context menu for the message (`msg.menu`), so it only works for an
/// owner or moderator of that channel; the timeout length is YouTube's.
pub async fn youtube_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &YouTubeMsg) -> ToastMsg {
    let body = json!({"action": action, "menu": msg.menu});
    let r = relay_api(bridge, source, "/api/youtube/action", Some("POST"), Some(body)).await;
    if !r.ok {
        let t = r.text();
        return warn(format!("YouTube: {}", if t.is_empty() { "action failed".to_string() } else { t }));
    }
    let who = if msg.user.name.is_empty() { "user".to_string() } else { msg.user.name.clone() };
    ok_toast(match action {
        "timeout" => format!("Timed out {who} on YouTube"),
        "ban" => format!("Banned {who} on YouTube"),
        _ => "Message deleted on YouTube".to_string(),
    })
}
