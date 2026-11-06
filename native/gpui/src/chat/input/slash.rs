//! Port of src/renderer/chat/input/slash.ts: SLASH_COMMANDS and runSlashCommand (Twitch removed IRC/API text
//! commands for third-party apps in Feb 2023 - only "/me" still sends as literal text; every other command maps
//! to a real Helix-backed relay route).
//!
//! The JS did the parsing and the HTTP call in one function. Here the parsing is `plan_slash` (pure, so it is
//! unit tested) and `run_slash_command` executes the plan through the bridge; both keep the JS' messages.
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, SourceRef};
use serde_json::{json, Map, Value};

use crate::chat::mod_actions::ToastMsg;
use crate::chat::mod_buttons::parse_duration;
use crate::chat::mod_caps::{relay_api, ModCaps, RelayResponse};

/// `capKey` names a field of `ModCaps`; the dropdown dims a command the account can't use.
#[derive(Clone, Copy, Debug)]
pub struct SlashCommand {
    pub name: &'static str,
    pub usage: &'static str,
    pub cap_key: Option<&'static str>,
}

const fn sc(name: &'static str, usage: &'static str, cap_key: &'static str) -> SlashCommand {
    SlashCommand { name, usage, cap_key: Some(cap_key) }
}

pub const SLASH_COMMANDS: [SlashCommand; 25] = [
    sc("ban", "<user> [reason]", "ban"),
    sc("timeout", "<user> <duration> [reason]", "ban"),
    sc("unban", "<user>", "ban"),
    sc("untimeout", "<user>", "ban"),
    sc("warn", "<user> <reason>", "warn"),
    sc("clear", "clear the whole chat", "delete"),
    sc("slow", "<seconds>", "modes"),
    sc("slowoff", "turn off slow mode", "modes"),
    sc("followers", "[10m|30m|1h|1d|1w|1mo]", "modes"),
    sc("followersoff", "turn off followers-only", "modes"),
    sc("emoteonly", "turn on emote-only", "modes"),
    sc("emoteonlyoff", "turn off emote-only", "modes"),
    sc("subscribers", "turn on subscribers-only", "modes"),
    sc("subscribersoff", "turn off subscribers-only", "modes"),
    sc("uniquechat", "turn on unique (r9k)", "modes"),
    sc("uniquechatoff", "turn off unique (r9k)", "modes"),
    sc("announce", "[blue|green|orange|purple] <text>", "announce"),
    sc("shoutout", "<user>", "shoutout"),
    sc("shield", "on|off", "shield"),
    sc("raid", "<user>", "raid"),
    sc("unraid", "cancel the pending raid", "raid"),
    sc("delete", "<messageId>", "delete"),
    sc("pin", "[messageId] - pins the last message by default", "pin"),
    sc("unpin", "unpin the current pin", "pin"),
    sc("me", "<action text> - passthrough, still supported", "send"),
];

/// `twitchCaps[c.capKey]`
pub fn cap_value(caps: &ModCaps, key: &str) -> bool {
    match key {
        "pin" => caps.pin,
        "ban" => caps.ban,
        "warn" => caps.warn,
        "delete" => caps.delete,
        "send" => caps.send,
        "modes" => caps.modes,
        "announce" => caps.announce,
        "shoutout" => caps.shoutout,
        "shield" => caps.shield,
        "terms" => caps.terms,
        "raid" => caps.raid,
        "automod" => caps.automod,
        _ => false,
    }
}

/// Followers-only duration, in MINUTES (Helix's follower_mode_duration unit) - "mo" (month) must be tried before
/// the single-letter "m" (minute). A bare number with no suffix is already minutes. Empty string = "Any" (0).
pub fn parse_follower_minutes(s: &str) -> Option<i64> {
    let t = s.trim().to_lowercase();
    if t.is_empty() {
        return Some(0);
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let mult: f64 = match &t[digits.len()..] {
        "" | "m" => 1.0,
        "h" => 60.0,
        "d" => 1440.0,
        "w" => 10080.0,
        "mo" => 43200.0,
        _ => return None,
    };
    let n: f64 = digits.parse().ok()?;
    Some((n * mult).round() as i64)
}

const ANNOUNCE_COLOR_WORDS: [&str; 4] = ["blue", "green", "orange", "purple"];

/// "@name" -> "name", trimmed + lowercased (Twitch logins are already case-insensitive; the backend's userLogin
/// resolver lowercases too).
pub fn parse_user_arg(s: &str) -> String {
    s.strip_prefix('@').unwrap_or(s).trim().to_lowercase()
}

pub fn split_args(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// `/^\/(\S+)\s*([\s\S]*)$/`: (command lowercased, rest trimmed).
pub fn parse_command(raw: &str) -> Option<(String, String)> {
    let t = raw.trim();
    let body = t.strip_prefix('/')?;
    let cmd_end = body.find(char::is_whitespace).unwrap_or(body.len());
    if cmd_end == 0 {
        return None;
    }
    Some((body[..cmd_end].to_lowercase(), body[cmd_end..].trim().to_string()))
}

/// `/^\/me(\s|$)/i`
pub fn is_me(t: &str) -> bool {
    let l = t.to_lowercase();
    l == "/me" || l.starts_with("/me ") || l.starts_with("/me\t") || l.starts_with("/me\n")
}

/// What a command does to the relay.
#[derive(Clone, Debug, PartialEq)]
pub struct SlashPlan {
    pub path: &'static str,
    pub method: &'static str,
    pub body: Option<Value>,
    pub ok: String,
}

/// Helix wants absent fields absent (JS `undefined` is dropped by JSON.stringify).
fn obj(pairs: Vec<(&str, Option<Value>)>) -> Value {
    let mut m = Map::new();
    for (k, v) in pairs {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    }
    Value::Object(m)
}

fn nonempty(s: String) -> Option<Value> {
    (!s.is_empty()).then(|| json!(s))
}

fn mod_action(body: Value, ok: String) -> SlashPlan {
    SlashPlan { path: "/api/mod/action", method: "POST", body: Some(body), ok }
}
fn modes(body: Value, ok: &str) -> SlashPlan {
    SlashPlan { path: "/api/chat/settings", method: "POST", body: Some(body), ok: ok.to_string() }
}

pub enum Planned {
    Plan(SlashPlan),
    /// not a command Twitch still takes: toast this (warn) and report false
    Unsupported(String),
}

/// Parse a command into the relay call it makes, EXCEPT /clear and /raid, which need a confirm step handled by
/// the caller (the send box) before they run. `Err` is the JS' thrown Error (shown as a warn toast).
pub fn plan_slash(raw: &str, last_message_id: Option<&str>, pinned_id: Option<&str>) -> Result<Option<Planned>, String> {
    let Some((cmd, rest)) = parse_command(raw) else { return Ok(None) };
    let rest = rest.as_str();
    let plan = match cmd.as_str() {
        "ban" => {
            let parts = split_args(rest);
            let user = parse_user_arg(parts.first().copied().unwrap_or(""));
            if user.is_empty() {
                return Err("Usage: /ban <user> [reason]".into());
            }
            mod_action(obj(vec![("action", Some(json!("ban"))), ("userLogin", Some(json!(user))), ("reason", nonempty(parts[1..].join(" ")))]), format!("banned {user}"))
        }
        "timeout" => {
            let parts = split_args(rest);
            let user = parse_user_arg(parts.first().copied().unwrap_or(""));
            let dur = parts.get(1).and_then(|p| parse_duration(p));
            let (true, Some(dur)) = (!user.is_empty(), dur) else { return Err("Usage: /timeout <user> <duration> [reason]".into()) };
            let ok = format!("timed out {user} ({})", parts[1]);
            mod_action(obj(vec![("action", Some(json!("timeout"))), ("userLogin", Some(json!(user))), ("duration", Some(json!(dur))), ("reason", nonempty(parts[2..].join(" ")))]), ok)
        }
        "unban" | "untimeout" => {
            let user = parse_user_arg(split_args(rest).first().copied().unwrap_or(""));
            if user.is_empty() {
                return Err(format!("Usage: /{cmd} <user>"));
            }
            let ok = format!("{} {user}", if cmd == "unban" { "unbanned" } else { "untimed out" });
            mod_action(json!({"action": "unban", "userLogin": user}), ok)
        }
        "warn" => {
            let parts = split_args(rest);
            let user = parse_user_arg(parts.first().copied().unwrap_or(""));
            let reason = parts.get(1..).map(|p| p.join(" ")).unwrap_or_default().trim().to_string();
            if user.is_empty() || reason.is_empty() {
                return Err("Usage: /warn <user> <reason>".into());
            }
            mod_action(json!({"action": "warn", "userLogin": user, "reason": reason}), format!("warned {user}"))
        }
        "slow" => {
            let Some(secs) = parse_duration(if rest.is_empty() { "30" } else { rest }) else { return Err("Usage: /slow <seconds>".into()) };
            let clamped = secs.clamp(1, 1800);
            modes(json!({"slow_mode": true, "slow_mode_wait_time": clamped}), &format!("slow mode {clamped}s"))
        }
        "slowoff" => modes(json!({"slow_mode": false}), "slow mode off"),
        "followers" => {
            let Some(mins) = parse_follower_minutes(rest) else { return Err("Usage: /followers [10m|30m|1h|1d|1w|1mo]".into()) };
            modes(json!({"follower_mode": true, "follower_mode_duration": mins}), &if mins != 0 { format!("followers-only ({rest})") } else { "followers-only (any)".to_string() })
        }
        "followersoff" => modes(json!({"follower_mode": false}), "followers-only off"),
        "emoteonly" => modes(json!({"emote_mode": true}), "emote-only on"),
        "emoteonlyoff" => modes(json!({"emote_mode": false}), "emote-only off"),
        "subscribers" => modes(json!({"subscriber_mode": true}), "subscribers-only on"),
        "subscribersoff" => modes(json!({"subscriber_mode": false}), "subscribers-only off"),
        "uniquechat" => modes(json!({"unique_chat_mode": true}), "unique chat on"),
        "uniquechatoff" => modes(json!({"unique_chat_mode": false}), "unique chat off"),
        "announce" => {
            let parts = split_args(rest);
            let mut color: Option<String> = None;
            let mut text = rest.to_string();
            if let Some(w) = parts.first().filter(|w| ANNOUNCE_COLOR_WORDS.contains(&w.to_lowercase().as_str())) {
                color = Some(w.to_lowercase());
                text = parts[1..].join(" ");
            }
            let text = text.trim().to_string();
            if text.is_empty() {
                return Err("Usage: /announce [blue|green|orange|purple] <text>".into());
            }
            let text: String = text.chars().take(500).collect();
            SlashPlan { path: "/api/chat/announce", method: "POST", body: Some(obj(vec![("text", Some(json!(text))), ("color", color.map(|c| json!(c)))])), ok: "announced".into() }
        }
        "shoutout" => {
            let user = parse_user_arg(split_args(rest).first().copied().unwrap_or(""));
            if user.is_empty() {
                return Err("Usage: /shoutout <user>".into());
            }
            SlashPlan { path: "/api/chat/shoutout", method: "POST", body: Some(json!({"login": user})), ok: format!("shoutout @{user}") }
        }
        "shield" => {
            let v = rest.to_lowercase();
            if v != "on" && v != "off" {
                return Err("Usage: /shield on|off".into());
            }
            SlashPlan { path: "/api/chat/shield", method: "PUT", body: Some(json!({"is_active": v == "on"})), ok: format!("shield mode {v}") }
        }
        "unraid" => SlashPlan { path: "/api/chat/raid", method: "DELETE", body: None, ok: "raid cancelled".into() },
        "delete" => {
            let id = split_args(rest).first().copied().unwrap_or("").to_string();
            if id.is_empty() {
                return Err("Usage: /delete <messageId>".into());
            }
            mod_action(json!({"action": "delete", "messageId": id}), "deleted".into())
        }
        "pin" => {
            let id = split_args(rest).first().map(|s| s.to_string()).or_else(|| last_message_id.map(|s| s.to_string())).unwrap_or_default();
            if id.is_empty() {
                return Err("Nothing to pin yet".into());
            }
            mod_action(json!({"action": "pin", "messageId": id, "durationSeconds": 1200}), "Pinned".into())
        }
        "unpin" => {
            let Some(id) = pinned_id.filter(|s| !s.is_empty()) else { return Err("Nothing is pinned".into()) };
            mod_action(json!({"action": "unpin", "messageId": id}), "unpinned".into())
        }
        _ => return Ok(Some(Planned::Unsupported(format!("Twitch removed chat text commands for third-party apps in 2023 - only /me still sends. \"/{cmd}\" isn't supported.")))),
    };
    Ok(Some(Planned::Plan(plan)))
}

/// `postJson` failing: the relay's `{error}` or the text, with the relogin hint.
pub fn post_json_error(r: &RelayResponse) -> String {
    let msg = match &r.body {
        Value::Object(o) => o.get("error").map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.to_string())).unwrap_or_else(|| r.text()),
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if r.status == 403 && msg == "relogin" {
        "Log in again to grant this permission".into()
    } else if msg.is_empty() {
        "action failed".into()
    } else {
        msg
    }
}

/// `postJson`: the response, or the thrown Error's message.
pub async fn post_json(bridge: &Arc<dyn AguBridge>, source: &SourceRef, path: &str, body: Option<Value>, method: &str) -> Result<RelayResponse, String> {
    let r = relay_api(bridge, source, path, Some(method), body).await;
    if r.ok {
        Ok(r)
    } else {
        Err(post_json_error(&r))
    }
}

/// Dispatches every slash command EXCEPT /clear and /raid (confirm step first) and /me (falls through to the
/// normal send path). Ok(true) on success (caller clears the input); Err (shown as a warn toast) on a bad or
/// incomplete command or a relay error; Ok(false) + the "not supported" toast for anything unrecognized.
pub async fn run_slash_command(bridge: &Arc<dyn AguBridge>, source: &SourceRef, raw: &str, last_message_id: Option<String>, pinned_id: Option<String>) -> Result<(bool, ToastMsg), String> {
    match plan_slash(raw, last_message_id.as_deref(), pinned_id.as_deref())? {
        None => Ok((false, None)),
        Some(Planned::Unsupported(t)) => Ok((false, Some((t, true)))),
        Some(Planned::Plan(p)) => {
            post_json(bridge, source, p.path, p.body, p.method).await?;
            Ok((true, Some((p.ok, false))))
        }
    }
}

/// /clear and /raid, run after the confirm step.
#[derive(Clone, Debug, PartialEq)]
pub enum Confirmed {
    Clear,
    Raid { login: String },
}

pub async fn confirmed_command(bridge: &Arc<dyn AguBridge>, source: &SourceRef, c: &Confirmed) -> Result<ToastMsg, String> {
    match c {
        Confirmed::Clear => {
            post_json(bridge, source, "/api/mod/action", Some(json!({"action": "clear"})), "POST").await?;
            Ok(Some(("chat cleared".into(), false)))
        }
        Confirmed::Raid { login } => {
            post_json(bridge, source, "/api/chat/raid", Some(json!({"login": login})), "POST").await?;
            Ok(Some((format!("raiding @{login}"), false)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(raw: &str) -> SlashPlan {
        match plan_slash(raw, Some("last"), Some("pinned")).unwrap().unwrap() {
            Planned::Plan(p) => p,
            Planned::Unsupported(t) => panic!("unsupported: {t}"),
        }
    }
    fn err(raw: &str) -> String {
        plan_slash(raw, None, None).err().expect("an error")
    }

    #[test]
    fn follower_minutes() {
        assert_eq!(parse_follower_minutes(""), Some(0));
        assert_eq!(parse_follower_minutes("30"), Some(30));
        assert_eq!(parse_follower_minutes("10m"), Some(10));
        assert_eq!(parse_follower_minutes("2h"), Some(120));
        assert_eq!(parse_follower_minutes("1d"), Some(1440));
        assert_eq!(parse_follower_minutes("1w"), Some(10080));
        assert_eq!(parse_follower_minutes("1mo"), Some(43200));
        assert_eq!(parse_follower_minutes("3MO"), Some(129600));
        assert_eq!(parse_follower_minutes("x"), None);
        assert_eq!(parse_follower_minutes("5y"), None);
    }

    #[test]
    fn user_args_and_splitting() {
        assert_eq!(parse_user_arg("@Bob"), "bob");
        assert_eq!(parse_user_arg("  Bob "), "bob");
        assert_eq!(split_args("  a   b\tc "), vec!["a", "b", "c"]);
    }

    #[test]
    fn command_parse() {
        assert_eq!(parse_command("/Ban  bob  spam "), Some(("ban".into(), "bob  spam".into())));
        assert_eq!(parse_command("/slowoff"), Some(("slowoff".into(), "".into())));
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/"), None);
        assert!(is_me("/me waves") && is_me("/ME") && !is_me("/mention x"));
    }

    #[test]
    fn ban_and_timeout() {
        let p = plan("/ban @Bob being rude");
        assert_eq!(p.path, "/api/mod/action");
        assert_eq!(p.body.unwrap(), json!({"action": "ban", "userLogin": "bob", "reason": "being rude"}));
        assert_eq!(p.ok, "banned bob");
        // no reason: the key is absent, like JSON.stringify drops `undefined`
        assert_eq!(plan("/ban bob").body.unwrap(), json!({"action": "ban", "userLogin": "bob"}));
        let t = plan("/timeout bob 10m spam");
        assert_eq!(t.body.clone().unwrap(), json!({"action": "timeout", "userLogin": "bob", "duration": 600, "reason": "spam"}));
        assert_eq!(t.ok, "timed out bob (10m)");
        assert_eq!(err("/timeout bob"), "Usage: /timeout <user> <duration> [reason]");
        assert_eq!(err("/ban"), "Usage: /ban <user> [reason]");
        assert_eq!(plan("/untimeout Bob").ok, "untimed out bob");
        assert_eq!(plan("/unban Bob").ok, "unbanned bob");
    }

    #[test]
    fn warn_needs_a_reason() {
        assert_eq!(err("/warn bob"), "Usage: /warn <user> <reason>");
        assert_eq!(plan("/warn bob stop it").body.unwrap(), json!({"action": "warn", "userLogin": "bob", "reason": "stop it"}));
    }

    #[test]
    fn room_modes() {
        let s = plan("/slow");
        assert_eq!(s.path, "/api/chat/settings");
        assert_eq!(s.body.unwrap(), json!({"slow_mode": true, "slow_mode_wait_time": 30}));
        assert_eq!(plan("/slow 5000").ok, "slow mode 1800s");
        assert_eq!(err("/slow abc"), "Usage: /slow <seconds>");
        assert_eq!(plan("/followers 1h").body.unwrap(), json!({"follower_mode": true, "follower_mode_duration": 60}));
        assert_eq!(plan("/followers").ok, "followers-only (any)");
        assert_eq!(plan("/emoteonlyoff").body.unwrap(), json!({"emote_mode": false}));
        assert_eq!(plan("/uniquechat").body.unwrap(), json!({"unique_chat_mode": true}));
    }

    #[test]
    fn announce_shoutout_shield_raid() {
        assert_eq!(plan("/announce green hello all").body.unwrap(), json!({"text": "hello all", "color": "green"}));
        assert_eq!(plan("/announce hello all").body.unwrap(), json!({"text": "hello all"}));
        assert_eq!(err("/announce purple"), "Usage: /announce [blue|green|orange|purple] <text>");
        assert_eq!(plan("/shoutout @Bob").body.unwrap(), json!({"login": "bob"}));
        let sh = plan("/shield ON");
        assert_eq!((sh.method, sh.body.unwrap()), ("PUT", json!({"is_active": true})));
        assert_eq!(err("/shield maybe"), "Usage: /shield on|off");
        let un = plan("/unraid");
        assert_eq!((un.method, un.body), ("DELETE", None));
    }

    #[test]
    fn delete_pin_unpin() {
        assert_eq!(plan("/delete abc").body.unwrap(), json!({"action": "delete", "messageId": "abc"}));
        assert_eq!(plan("/pin").body.unwrap(), json!({"action": "pin", "messageId": "last", "durationSeconds": 1200}));
        assert_eq!(plan("/pin xyz").body.unwrap()["messageId"], "xyz");
        assert_eq!(err("/pin"), "Nothing to pin yet");
        assert_eq!(plan("/unpin").body.unwrap(), json!({"action": "unpin", "messageId": "pinned"}));
        assert_eq!(err("/unpin"), "Nothing is pinned");
    }

    #[test]
    fn unknown_commands_are_unsupported() {
        match plan_slash("/vip bob", None, None).unwrap().unwrap() {
            Planned::Unsupported(t) => assert!(t.contains("\"/vip\" isn't supported")),
            _ => panic!(),
        }
    }

    #[test]
    fn cap_lookup_covers_every_command() {
        let all = ModCaps { pin: true, ban: true, warn: true, delete: true, send: true, modes: true, announce: true, shoutout: true, shield: true, terms: true, raid: true, automod: true, ..Default::default() };
        for c in SLASH_COMMANDS {
            assert!(cap_value(&all, c.cap_key.unwrap()), "{}", c.name);
            assert!(!cap_value(&ModCaps::default(), c.cap_key.unwrap()), "{}", c.name);
        }
    }

    #[test]
    fn relay_errors() {
        let r = |status, body| RelayResponse { ok: false, status, body };
        assert_eq!(post_json_error(&r(403, json!({"error": "relogin"}))), "Log in again to grant this permission");
        assert_eq!(post_json_error(&r(400, json!({"error": "nope"}))), "nope");
        assert_eq!(post_json_error(&r(500, Value::Null)), "action failed");
    }
}
