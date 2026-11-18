//! Kick's chat modes and what the logged-in account is in a channel, in the shapes `crate::chat_rules` uses.
//! The modes come from `GET kick.com/api/v2/channels/<slug>/chatroom` (the one the website reads: the channel
//! endpoint's `chatroom` copy carries an older follow time) and live from Pusher's `ChatroomUpdatedEvent`, which
//! has the same shape. The viewer comes from the channel-user lookup the user card already uses.
use serde_json::Value;

use super::vendor::kick::{enc, kick_fetch};
use crate::chat_rules::{ChatModes, ChatViewer};
use crate::net;
use crate::util::parse_rfc3339_ms;

fn num(v: &Value) -> i64 {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0)
}

/// `{enabled, ...}` (the chatroom endpoint, the Pusher event) or a bare bool (the channel endpoint's copy).
fn enabled(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Object(o) => o.get("enabled").and_then(|e| e.as_bool()).unwrap_or(false),
        _ => false,
    }
}

/// The modes of a Kick chatroom answer, either shape.
pub fn modes_from_chatroom(c: &Value) -> ChatModes {
    let followers = &c["followers_mode"];
    let slow = &c["slow_mode"];
    let age = &c["account_age"];
    let followers_min = if followers.is_object() { num(&followers["min_duration"]) } else { num(&c["following_min_duration"]) };
    let slow_secs = if slow.is_object() { num(&slow["message_interval"]) } else { num(&c["message_interval"]) };
    ChatModes {
        followers_min: enabled(followers).then_some(followers_min.max(0)),
        subs_only: enabled(&c["subscribers_mode"]),
        emote_only: enabled(&c["emotes_mode"]),
        slow_secs: if enabled(slow) { slow_secs.max(0) } else { 0 },
        account_age_min: enabled(age).then(|| num(&age["min_duration"]).max(0)).filter(|m| *m > 0),
        ..Default::default()
    }
}

/// The logged-in account in a channel, from the channel-user lookup (`following_since`, `subscribed_for`, roles).
pub fn viewer_from_channel_user(u: &Value) -> ChatViewer {
    let badge = |t: &str| u["badges"].as_array().is_some_and(|a| a.iter().any(|b| b["type"].as_str() == Some(t)));
    let exempt = u["is_channel_owner"] == Value::Bool(true)
        || u["is_moderator"] == Value::Bool(true)
        || u["is_staff"] == Value::Bool(true)
        || ["broadcaster", "moderator", "vip", "staff"].iter().any(|t| badge(t));
    let since = u["following_since"].as_str().and_then(parse_rfc3339_ms);
    ChatViewer {
        exempt,
        subscribed: Some(num(&u["subscribed_for"]) > 0 || badge("subscriber") || badge("founder")),
        follows: Some(since.is_some()),
        following_since: since,
        account_created: u["created_at"].as_str().and_then(parse_rfc3339_ms),
    }
}

/// The chatroom's settings. None when Kick does not know the channel.
pub async fn get_kick_chatroom(slug: &str, user_agent: &str) -> Result<Option<Value>, String> {
    let r = kick_fetch(&format!("https://kick.com/api/v2/channels/{}/chatroom", enc(slug)), user_agent, "", None).await.map_err(|e| e.message)?;
    if r.status().as_u16() == 404 {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(format!("Kick answered {}", r.status().as_u16()));
    }
    net::json(r).await.map(Some).map_err(|e| e.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_chatroom_endpoint_and_the_live_event() {
        // as kick.com answered for a big channel (2026-11)
        let c = json!({"id": 668, "slow_mode": {"enabled": false, "message_interval": 0}, "subscribers_mode": {"enabled": false}, "followers_mode": {"enabled": true, "min_duration": 60}, "emotes_mode": {"enabled": false}, "account_age": {"enabled": true, "min_duration": 43830}, "pinned_message": null});
        assert_eq!(modes_from_chatroom(&c), ChatModes { followers_min: Some(60), account_age_min: Some(43830), ..Default::default() });
        let ev = json!({"id": 668, "slow_mode": {"enabled": true, "message_interval": 5}, "subscribers_mode": {"enabled": true}, "followers_mode": {"enabled": false, "min_duration": 60}, "emotes_mode": {"enabled": true}});
        assert_eq!(modes_from_chatroom(&ev), ChatModes { subs_only: true, emote_only: true, slow_secs: 5, ..Default::default() });
    }

    #[test]
    fn the_channel_endpoints_flat_copy() {
        let c = json!({"slow_mode": false, "followers_mode": true, "subscribers_mode": false, "emotes_mode": false, "message_interval": 5, "following_min_duration": 10});
        assert_eq!(modes_from_chatroom(&c), ChatModes { followers_min: Some(10), ..Default::default() });
    }

    #[test]
    fn a_viewer_from_the_channel_user_lookup() {
        let owner = json!({"is_staff": false, "is_channel_owner": true, "is_moderator": false, "badges": [{"type": "broadcaster"}], "following_since": null, "created_at": "2022-10-22T06:59:51.000000Z", "subscribed_for": 0});
        let v = viewer_from_channel_user(&owner);
        assert!(v.exempt);
        assert_eq!((v.subscribed, v.follows, v.following_since), (Some(false), Some(false), None));
        assert!(v.account_created.is_some());
        let fan = json!({"is_channel_owner": false, "is_moderator": false, "badges": [{"type": "subscriber"}], "following_since": "2026-11-18T10:00:00.000000Z", "subscribed_for": 3});
        let v = viewer_from_channel_user(&fan);
        assert!(!v.exempt);
        assert_eq!((v.subscribed, v.follows), (Some(true), Some(true)));
        assert_eq!(v.following_since, parse_rfc3339_ms("2026-11-18T10:00:00.000000Z"));
        let vip = json!({"badges": [{"type": "vip"}], "following_since": null, "subscribed_for": 0});
        assert!(viewer_from_channel_user(&vip).exempt);
    }
}
