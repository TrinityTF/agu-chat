//! Port of src/renderer/chat/modActions.ts: the mod / send HTTP calls (delete, AutoMod resolve, pin, unpin, pin
//! duration, Rumble and Kick moderation) as async functions over the bridge. Each returns the toast to show
//! (text, warn) instead of calling a toast callback; the pane shows it. The `window.confirm` of the Rumble /
//! Kick ban is the pane's confirm bar, so these functions are only called once that was accepted.
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, Platform, SourceRef};
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
    let r = relay_api(bridge, source, "/api/mod/automod", Some("POST"), Some(automod_body(id, action))).await;
    automod_result(&r)
}

/// The body of an AutoMod Allow ("allow") / Deny ("deny").
pub fn automod_body(id: &str, action: &str) -> Value {
    json!({"msgId": id, "action": action})
}

pub fn automod_result(r: &RelayResponse) -> (AutomodResult, ToastMsg) {
    if r.ok {
        return (AutomodResult::Ok, None);
    }
    if r.status == 404 {
        return (AutomodResult::Gone, warn("Message is no longer held"));
    }
    (AutomodResult::Fail, warn(fail_text(r, "action failed")))
}

/// The "Pin for" menu: Twitch takes 30 seconds to 30 minutes; `None` pins until unpinned or the stream ends.
pub const PIN_CHOICES: [(&str, Option<u32>); 7] = [
    ("30 seconds", Some(30)),
    ("1 minute", Some(60)),
    ("2 minutes", Some(120)),
    ("5 minutes", Some(300)),
    ("10 minutes", Some(600)),
    ("30 minutes", Some(1800)),
    ("Until unpinned or end of stream", None),
];

pub fn pin_body(id: &str, seconds: Option<u32>) -> serde_json::Value {
    let mut body = json!({"action": "pin", "messageId": id});
    if let Some(s) = seconds {
        body["durationSeconds"] = json!(s);
    }
    body
}

pub async fn pin_message(bridge: &Arc<dyn AguBridge>, source: &SourceRef, id: &str, seconds: Option<u32>) -> (bool, ToastMsg) {
    twitch_action(bridge, source, pin_body(id, seconds), Some("Pinned".into())).await
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

/// A request to the local relay: what a moderation action sends.
#[derive(Clone, Debug, PartialEq)]
pub struct ModRequest {
    pub path: &'static str,
    pub method: &'static str,
    pub body: Value,
}

/// Who and what a moderation action is about: the message (`id`), its sender (`user_id`, `login`, `name`) and, on
/// YouTube, the message's context-menu params (`menu`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModTarget {
    pub id: String,
    pub user_id: String,
    pub login: String,
    pub name: String,
    pub menu: Option<String>,
}

impl ModTarget {
    pub fn from_json(v: &Value) -> ModTarget {
        let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        ModTarget { id: s("id"), user_id: s("uid"), login: s("login"), name: s("name"), menu: v.get("menu").and_then(|x| x.as_str()).filter(|m| !m.is_empty()).map(String::from) }
    }

    /// Rumble moderation names the person by login.
    fn rumble_who(&self) -> String {
        if self.login.is_empty() { self.name.clone() } else { self.login.clone() }
    }

    fn display(&self) -> String {
        if self.name.is_empty() { "user".to_string() } else { self.name.clone() }
    }
}

/// Rumble moderation targets the PERSON (`user.login`), not the channel a message was posted under.
pub fn rumble_body(action: &str, message_id: &str, who: &str, seconds: u32) -> Value {
    if action == "delete" {
        json!({"action": action, "messageId": message_id})
    } else if action == "timeout" {
        json!({"action": action, "username": who, "seconds": seconds})
    } else {
        json!({"action": action, "username": who})
    }
}

pub fn kick_body(action: &str, user_id: &str, message_id: &str, seconds: Option<u32>) -> Value {
    json!({"action": action, "userId": user_id, "messageId": message_id, "seconds": seconds})
}

/// YouTube moderation asks YouTube's own context menu for the message (`menu`), so it only works for an owner or
/// moderator of that channel; the timeout length is YouTube's.
pub fn youtube_body(action: &str, menu: &Option<String>) -> Value {
    json!({"action": action, "menu": menu})
}

/// The body of a Twitch moderation call (`/api/mod/action`). `reason` is the ban / warn reason (a warn needs one).
pub fn twitch_body(action: &str, t: &ModTarget, seconds: Option<u32>, reason: &str) -> Result<Value, String> {
    let reason = reason.trim();
    Ok(match action {
        "ban" => {
            let mut b = json!({"action": "ban", "userId": t.user_id});
            if !reason.is_empty() {
                b["reason"] = json!(reason.chars().take(500).collect::<String>());
            }
            b
        }
        "timeout" => json!({"action": "timeout", "userId": t.user_id, "duration": seconds.ok_or("no timeout length")?}),
        "warn" => {
            if reason.is_empty() {
                return Err("A warning needs a reason".into());
            }
            json!({"action": "warn", "userId": t.user_id, "reason": reason.chars().take(500).collect::<String>()})
        }
        "unban" => json!({"action": "unban", "userId": t.user_id}),
        "delete" => json!({"action": "delete", "messageId": t.id}),
        other => return Err(format!("unknown action {other}")),
    })
}

/// What an action sends and what to say when it worked.
#[derive(Clone, Debug, PartialEq)]
pub struct ModPlan {
    pub platform: Platform,
    pub action: String,
    pub req: ModRequest,
    pub ok: Option<String>,
}

/// The request for `action` ("ban", "timeout", "warn", "delete", "unban") on `platform`, or why it can't be one.
/// `quick`: a Twitch timeout from a mod button says who ("timed out Bob (10m)"), one from the ladder only how long.
pub fn plan(platform: Platform, action: &str, t: &ModTarget, seconds: Option<u32>, reason: &str, quick: bool) -> Result<ModPlan, String> {
    let (req, ok) = match platform {
        Platform::Twitch => {
            let body = twitch_body(action, t, seconds, reason)?;
            let ok = match action {
                "ban" => Some("banned".to_string()),
                "warn" => Some("warned".to_string()),
                "unban" => Some("unbanned".to_string()),
                "timeout" => {
                    let d = format_duration(seconds.unwrap_or(0) as i64);
                    Some(if quick { format!("timed out {} ({d})", t.name) } else { format!("timed out {d}") })
                }
                _ => None,
            };
            (ModRequest { path: "/api/mod/action", method: "POST", body }, ok)
        }
        Platform::Rumble => {
            if !matches!(action, "ban" | "timeout" | "delete") {
                return Err("Rumble can't do that".into());
            }
            let who = t.rumble_who();
            let secs = seconds.unwrap_or(300);
            let ok = match action {
                "timeout" => format!("timed out {who} on Rumble ({})", format_duration(secs as i64)),
                "ban" => format!("banned {who} on Rumble"),
                _ => "deleted on Rumble".to_string(),
            };
            (ModRequest { path: "/api/rumble/action", method: "POST", body: rumble_body(action, &t.id, &who, secs) }, Some(ok))
        }
        Platform::Kick => {
            if !matches!(action, "ban" | "timeout" | "unban" | "delete") {
                return Err("Kick can't do that".into());
            }
            let who = t.display();
            let ok = match action {
                "timeout" => format!("Timed out {who} on Kick"),
                "ban" => format!("Banned {who} on Kick"),
                "unban" => format!("Unbanned {who} on Kick"),
                _ => "Message deleted on Kick".to_string(),
            };
            (ModRequest { path: "/api/kick/action", method: "POST", body: kick_body(action, &t.user_id, &t.id, if action == "timeout" { seconds } else { None }) }, Some(ok))
        }
        Platform::Youtube => {
            if !matches!(action, "ban" | "timeout" | "delete") {
                return Err("YouTube can't do that".into());
            }
            if t.menu.is_none() {
                return Err("YouTube didn't say how to moderate this message".into());
            }
            let who = t.display();
            let ok = match action {
                "timeout" => format!("Timed out {who} on YouTube"),
                "ban" => format!("Banned {who} on YouTube"),
                _ => "Message deleted on YouTube".to_string(),
            };
            (ModRequest { path: "/api/youtube/action", method: "POST", body: youtube_body(action, &t.menu) }, Some(ok))
        }
        Platform::X => return Err("X chat has no moderation".into()),
    };
    Ok(ModPlan { platform, action: action.to_string(), req, ok })
}

/// Kick, Rumble and YouTube ask before a ban (the desktop's confirm bar); Twitch's ban has its own reason step.
pub fn confirm_text(platform: Platform, action: &str, t: &ModTarget) -> Option<String> {
    if action != "ban" {
        return None;
    }
    match platform {
        Platform::Rumble => Some(format!("Ban {} from the Rumble chat?", t.rumble_who())),
        Platform::Kick => Some(format!("Ban {} from the Kick chat?", if t.name.is_empty() { "this user" } else { &t.name })),
        Platform::Youtube => Some(format!("Ban {} from the YouTube chat?", if t.name.is_empty() { "this user" } else { &t.name })),
        _ => None,
    }
}

/// What the relay's answer to a plan means: success and the toast to show.
pub fn plan_result(p: &ModPlan, r: &RelayResponse) -> (bool, ToastMsg) {
    match p.platform {
        Platform::Twitch => {
            if !r.ok {
                return (false, warn(fail_text(r, "action failed")));
            }
        }
        Platform::Rumble => {
            if !r.ok {
                let t = r.text();
                return (false, warn(if t.is_empty() { "Rumble rejected the action".to_string() } else { t }));
            }
        }
        other => {
            if !r.ok {
                let name = if other == Platform::Kick { "Kick" } else { "YouTube" };
                let t = r.text();
                return (false, warn(format!("{name}: {}", if t.is_empty() { "action failed".to_string() } else { t })));
            }
        }
    }
    (true, p.ok.clone().and_then(ok_toast))
}

async fn run(bridge: &Arc<dyn AguBridge>, source: &SourceRef, p: &ModPlan) -> (bool, ToastMsg) {
    let r = relay_api(bridge, source, p.req.path, Some(p.req.method), Some(p.req.body.clone())).await;
    plan_result(p, &r)
}

pub async fn rumble_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &RumbleMsg, seconds: u32) -> ToastMsg {
    let t = ModTarget { id: msg.id.clone(), login: msg.user.login.clone(), name: msg.user.name.clone(), ..Default::default() };
    match plan(Platform::Rumble, action, &t, Some(seconds), "", false) {
        Ok(p) => run(bridge, source, &p).await.1,
        Err(e) => warn(e),
    }
}

pub async fn kick_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &KickMsg, seconds: Option<u32>) -> ToastMsg {
    let t = ModTarget { id: msg.id.clone(), user_id: msg.user.id.clone(), name: msg.user.name.clone(), login: msg.user.login.clone(), menu: None };
    match plan(Platform::Kick, action, &t, seconds, "", false) {
        Ok(p) => run(bridge, source, &p).await.1,
        Err(e) => warn(e),
    }
}

pub async fn youtube_mod_action(bridge: &Arc<dyn AguBridge>, source: &SourceRef, action: &str, msg: &YouTubeMsg) -> ToastMsg {
    let t = ModTarget { id: msg.id.clone(), name: msg.user.name.clone(), menu: msg.menu.clone(), ..Default::default() };
    match plan(Platform::Youtube, action, &t, None, "", false) {
        Ok(p) => run(bridge, source, &p).await.1,
        Err(e) => warn(e),
    }
}

#[cfg(test)]
mod pin_tests {
    use super::*;

    #[test]
    fn pin_choices_are_within_twitchs_range() {
        for (_, s) in PIN_CHOICES {
            if let Some(s) = s {
                assert!((30..=1800).contains(&s));
            }
        }
        assert_eq!(PIN_CHOICES.last().unwrap().1, None);
    }

    #[test]
    fn pin_body_omits_duration_for_until_unpinned() {
        assert_eq!(pin_body("m1", Some(300)), json!({"action": "pin", "messageId": "m1", "durationSeconds": 300}));
        assert_eq!(pin_body("m1", None), json!({"action": "pin", "messageId": "m1"}));
    }
}

#[cfg(test)]
mod plan_tests {
    use super::*;

    fn t() -> ModTarget {
        ModTarget { id: "m1".into(), user_id: "42".into(), login: "bob".into(), name: "Bob".into(), menu: Some("MENU".into()) }
    }

    fn p(platform: Platform, action: &str, seconds: Option<u32>, reason: &str) -> ModPlan {
        plan(platform, action, &t(), seconds, reason, true).unwrap()
    }

    #[test]
    fn twitch_requests() {
        let r = p(Platform::Twitch, "ban", None, "  spam ").req;
        assert_eq!((r.path, r.method), ("/api/mod/action", "POST"));
        assert_eq!(r.body, json!({"action": "ban", "userId": "42", "reason": "spam"}));
        assert_eq!(p(Platform::Twitch, "ban", None, "").req.body, json!({"action": "ban", "userId": "42"}));
        assert_eq!(p(Platform::Twitch, "timeout", Some(600), "").req.body, json!({"action": "timeout", "userId": "42", "duration": 600}));
        assert_eq!(p(Platform::Twitch, "warn", None, "rule 1; rule 2").req.body, json!({"action": "warn", "userId": "42", "reason": "rule 1; rule 2"}));
        assert_eq!(p(Platform::Twitch, "unban", None, "").req.body, json!({"action": "unban", "userId": "42"}));
        assert_eq!(p(Platform::Twitch, "delete", None, "").req.body, json!({"action": "delete", "messageId": "m1"}));
        assert!(plan(Platform::Twitch, "warn", &t(), None, "  ", true).is_err(), "a warning needs a reason");
        assert!(plan(Platform::Twitch, "timeout", &t(), None, "", true).is_err());
        assert!(plan(Platform::Twitch, "explode", &t(), None, "", true).is_err());
    }

    #[test]
    fn twitch_toasts_read_like_the_desktops() {
        assert_eq!(p(Platform::Twitch, "ban", None, "").ok.as_deref(), Some("banned"));
        assert_eq!(p(Platform::Twitch, "warn", None, "x").ok.as_deref(), Some("warned"));
        assert_eq!(p(Platform::Twitch, "unban", None, "").ok.as_deref(), Some("unbanned"));
        assert_eq!(p(Platform::Twitch, "delete", None, "").ok, None);
        assert_eq!(p(Platform::Twitch, "timeout", Some(600), "").ok.as_deref(), Some("timed out Bob (10m)"));
        assert_eq!(plan(Platform::Twitch, "timeout", &t(), Some(3600), "", false).unwrap().ok.as_deref(), Some("timed out 1h"));
    }

    #[test]
    fn kick_requests() {
        let r = p(Platform::Kick, "timeout", Some(300), "").req;
        assert_eq!((r.path, r.method), ("/api/kick/action", "POST"));
        assert_eq!(r.body, json!({"action": "timeout", "userId": "42", "messageId": "m1", "seconds": 300}));
        assert_eq!(p(Platform::Kick, "ban", Some(300), "").req.body, json!({"action": "ban", "userId": "42", "messageId": "m1", "seconds": null}));
        assert_eq!(p(Platform::Kick, "unban", None, "").ok.as_deref(), Some("Unbanned Bob on Kick"));
        assert_eq!(p(Platform::Kick, "delete", None, "").ok.as_deref(), Some("Message deleted on Kick"));
        assert!(plan(Platform::Kick, "warn", &t(), None, "x", true).is_err());
    }

    #[test]
    fn rumble_requests_name_the_person_by_login() {
        let r = p(Platform::Rumble, "timeout", Some(300), "").req;
        assert_eq!((r.path, r.method), ("/api/rumble/action", "POST"));
        assert_eq!(r.body, json!({"action": "timeout", "username": "bob", "seconds": 300}));
        assert_eq!(p(Platform::Rumble, "ban", None, "").req.body, json!({"action": "ban", "username": "bob"}));
        assert_eq!(p(Platform::Rumble, "delete", None, "").req.body, json!({"action": "delete", "messageId": "m1"}));
        assert_eq!(p(Platform::Rumble, "timeout", Some(300), "").ok.as_deref(), Some("timed out bob on Rumble (5m)"));
        assert_eq!(p(Platform::Rumble, "ban", None, "").ok.as_deref(), Some("banned bob on Rumble"));
        assert!(plan(Platform::Rumble, "unban", &t(), None, "", true).is_err());
    }

    #[test]
    fn youtube_requests_carry_the_menu() {
        let r = p(Platform::Youtube, "ban", None, "").req;
        assert_eq!((r.path, r.method), ("/api/youtube/action", "POST"));
        assert_eq!(r.body, json!({"action": "ban", "menu": "MENU"}));
        assert_eq!(p(Platform::Youtube, "timeout", None, "").ok.as_deref(), Some("Timed out Bob on YouTube"));
        assert!(plan(Platform::Youtube, "ban", &ModTarget { menu: None, ..t() }, None, "", true).is_err());
        assert!(plan(Platform::X, "ban", &t(), None, "", true).is_err(), "X has no moderation");
    }

    #[test]
    fn only_kick_rumble_and_youtube_ask_before_a_ban() {
        assert_eq!(confirm_text(Platform::Kick, "ban", &t()).as_deref(), Some("Ban Bob from the Kick chat?"));
        assert_eq!(confirm_text(Platform::Rumble, "ban", &t()).as_deref(), Some("Ban bob from the Rumble chat?"));
        assert_eq!(confirm_text(Platform::Youtube, "ban", &t()).as_deref(), Some("Ban Bob from the YouTube chat?"));
        assert_eq!(confirm_text(Platform::Twitch, "ban", &t()), None);
        assert_eq!(confirm_text(Platform::Kick, "timeout", &t()), None);
        assert_eq!(confirm_text(Platform::Kick, "delete", &t()), None);
    }

    #[test]
    fn answers_become_toasts() {
        let ok = RelayResponse { ok: true, status: 200, body: json!({}) };
        let bad = |status, body: Value| RelayResponse { ok: false, status, body };
        let ban = p(Platform::Twitch, "ban", None, "");
        assert_eq!(plan_result(&ban, &ok), (true, Some(("banned".into(), false))));
        assert_eq!(plan_result(&ban, &bad(403, json!({"error": "relogin"}))), (false, Some(("Log in again to grant mod permissions".into(), true))));
        assert_eq!(plan_result(&ban, &bad(400, json!({"error": "nope"}))), (false, Some(("nope".into(), true))));
        assert_eq!(plan_result(&p(Platform::Twitch, "delete", None, ""), &ok), (true, None));
        assert_eq!(plan_result(&p(Platform::Kick, "ban", None, ""), &bad(500, json!("boom"))), (false, Some(("Kick: boom".into(), true))));
        assert_eq!(plan_result(&p(Platform::Youtube, "ban", None, ""), &bad(500, json!(""))), (false, Some(("YouTube: action failed".into(), true))));
        assert_eq!(plan_result(&p(Platform::Rumble, "ban", None, ""), &bad(500, json!(""))), (false, Some(("Rumble rejected the action".into(), true))));
    }

    #[test]
    fn automod_requests_and_answers() {
        assert_eq!(automod_body("h1", "allow"), json!({"msgId": "h1", "action": "allow"}));
        assert_eq!(automod_result(&RelayResponse { ok: true, status: 200, body: json!({}) }), (AutomodResult::Ok, None));
        assert_eq!(automod_result(&RelayResponse { ok: false, status: 404, body: json!({}) }), (AutomodResult::Gone, Some(("Message is no longer held".into(), true))));
        assert_eq!(automod_result(&RelayResponse { ok: false, status: 500, body: json!({"error": "x"}) }), (AutomodResult::Fail, Some(("x".into(), true))));
    }
}
