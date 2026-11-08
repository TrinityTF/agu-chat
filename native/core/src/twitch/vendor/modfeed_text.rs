//! Port of src/main/twitch/vendor/modfeedText.js (vendored from modtools.agu.lol render/lib/ircRelay.js, render
//! commit 7704aec): the module-level text/shape builders at the bottom of that file.
use serde_json::{json, Value};

use crate::util::{js_str_or_empty, js_string, js_to_number, now_ms, parse_rfc3339_ms, truthy};

// ROOMSTATE mode-change text builders (CHAT v5 item 4).
pub fn followers_mode_text(val: &str) -> String {
    let n = crate::util::js_number(val);
    if !n.is_finite() || n < 0.0 {
        return "Followers-only chat disabled".into();
    }
    if n == 0.0 {
        return "Followers-only chat enabled (any duration)".into();
    }
    format!("Followers-only chat enabled ({} min)", fmt_num(n))
}

pub fn slow_mode_text(val: &str) -> String {
    let n = crate::util::js_number(val);
    if !n.is_finite() || n <= 0.0 {
        return "Slow mode off".into();
    }
    format!("Slow mode set to {}s", fmt_num(n))
}

pub fn toggle_mode_text(label: &str, on: bool) -> String {
    format!("{} {}", label, if on { "enabled" } else { "disabled" })
}

/// A number the way JS prints it in a template string (no trailing ".0").
fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// A Helix chat message (`{text, fragments}`) -> its text plus an IRC-style emotes tag ("id:start-end/..."),
/// counted in code points like IRC's.
pub fn text_from_fragments(message: &Value) -> (String, String) {
    let Some(frags) = message.get("fragments").and_then(Value::as_array) else {
        let text = match message.get("text") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        return (text, String::new());
    };
    let mut text = String::new();
    let mut pos = 0usize;
    let mut by_id: Vec<(String, Vec<String>)> = Vec::new();
    for f in frags {
        let t = match f.get("text") {
            Some(Value::String(s)) => s.as_str(),
            _ => "",
        };
        let len = t.chars().count();
        if f.get("type").and_then(Value::as_str) == Some("emote") && len > 0 {
            if let Some(id) = f.get("emote").and_then(|e| e.get("id")).filter(|i| truthy(i)).map(js_string) {
                let span = format!("{}-{}", pos, pos + len - 1);
                match by_id.iter_mut().find(|(i, _)| *i == id) {
                    Some((_, spans)) => spans.push(span),
                    None => by_id.push((id, vec![span])),
                }
            }
        }
        text.push_str(t);
        pos += len;
    }
    let emotes = by_id.iter().map(|(id, spans)| format!("{}:{}", id, spans.join(","))).collect::<Vec<_>>().join("/");
    (text, emotes)
}

// ---------- channel.moderate v2 -> named feed text (CHAT v5b) ----------

fn target_name(sub: Option<&Value>) -> String {
    let Some(sub) = sub else { return "someone".into() };
    let a = js_str_or_empty(sub.get("user_name"));
    if !a.is_empty() {
        return a;
    }
    let b = js_str_or_empty(sub.get("user_login"));
    if !b.is_empty() {
        return b;
    }
    "someone".into()
}

// Humanizes a whole-second duration (90 -> "1m 30s", 600 -> "10m", 3600 -> "1h", 86400 -> "1d", 604800 -> "1w"):
// largest unit that fits, plus a remainder in the NEXT smaller unit only (never more than two units).
fn humanize_duration(total_seconds: f64) -> String {
    let n = if total_seconds.is_nan() { 0.0 } else { total_seconds };
    let s = (n + 0.5).floor().max(0.0) as i64; // Math.max(0, Math.round(Number(x) || 0))
    if s <= 0 {
        return "0s".into();
    }
    const UNITS: [(&str, i64); 5] = [("w", 604_800), ("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)];
    for (i, (label, secs)) in UNITS.iter().enumerate() {
        if s < *secs {
            continue;
        }
        let count = s / secs;
        let rem = s - count * secs;
        if let Some((nl, ns)) = UNITS.get(i + 1) {
            if rem >= *ns {
                return format!("{count}{label} {}{nl}", rem / ns);
            }
        }
        return format!("{count}{label}");
    }
    format!("{s}s")
}

// timeout's duration comes from `expires_at` (RFC3339): "now" relative to when the moderate event arrived.
fn seconds_until(expires_at: &str) -> f64 {
    match parse_rfc3339_ms(expires_at) {
        Some(t) => (((t - now_ms()) as f64) / 1000.0 + 0.5).floor().max(0.0),
        None => 0.0,
    }
}

const SHARED_CHAT_ACTIONS: [&str; 5] = ["shared_chat_ban", "shared_chat_timeout", "shared_chat_unban", "shared_chat_untimeout", "shared_chat_delete"];

pub struct ModEntry {
    pub kind: &'static str,
    pub text: String,
}

/// `{kind, text}` for a channel.moderate event, or None.
pub fn build_moderate_entry(action: &str, actor: &str, event: &Value) -> Option<ModEntry> {
    let is_shared = SHARED_CHAT_ACTIONS.contains(&action);
    let base_action = if is_shared { &action["shared_chat_".len()..] } else { action };
    let sub = event.get(action).filter(|v| truthy(v)); // sub-object key always matches the action name verbatim
    let base = build_base_moderate_text(base_action, actor, event, sub)?;
    Some(if is_shared { ModEntry { kind: base.kind, text: format!("{} (shared chat)", base.text) } } else { base })
}

fn mode(text: String) -> Option<ModEntry> {
    Some(ModEntry { kind: "mode", text })
}

fn build_base_moderate_text(action: &str, actor: &str, event: &Value, sub: Option<&Value>) -> Option<ModEntry> {
    let reason = sub.map(|s| js_str_or_empty(s.get("reason"))).unwrap_or_default();
    match action {
        "ban" => Some(ModEntry {
            kind: "ban",
            text: format!("{actor} banned {}", target_name(sub)) + &if reason.is_empty() { String::new() } else { format!(": {reason}") },
        }),
        "timeout" => {
            let dur = match sub.and_then(|s| s.get("expires_at")).filter(|v| truthy(v)) {
                Some(e) => humanize_duration(seconds_until(&js_string(e))),
                None => String::new(),
            };
            Some(ModEntry {
                kind: "timeout",
                text: format!("{actor} timed out {}", target_name(sub)) + &if dur.is_empty() { String::new() } else { format!(" ({dur})") } + &if reason.is_empty() { String::new() } else { format!(": {reason}") },
            })
        }
        "unban" => Some(ModEntry { kind: "ban", text: format!("{actor} unbanned {}", target_name(sub)) }),
        "untimeout" => Some(ModEntry { kind: "timeout", text: format!("{actor} removed the timeout on {}", target_name(sub)) }),
        "clear" => Some(ModEntry { kind: "clear", text: format!("{actor} cleared chat") }),
        "delete" => Some(ModEntry { kind: "delete", text: format!("{actor} deleted a message from {}", target_name(sub)) }),
        "warn" => {
            let mut parts: Vec<String> = Vec::new();
            if !reason.is_empty() {
                parts.push(reason);
            }
            if let Some(rules) = sub.and_then(|s| s.get("chat_rules_cited")).and_then(Value::as_array) {
                if !rules.is_empty() {
                    parts.push(rules.iter().map(|r| if r.is_null() { String::new() } else { js_string(r) }).collect::<Vec<_>>().join(", "));
                }
            }
            Some(ModEntry { kind: "warn", text: format!("{actor} warned {}", target_name(sub)) + &if parts.is_empty() { String::new() } else { format!(": {}", parts.join("; ")) } })
        }
        "raid" => mode(format!("{actor} started a raid to {}", target_name(sub))),
        "unraid" => mode(format!("{actor} canceled the raid to {}", target_name(sub))),
        "slow" => {
            let wait = sub.and_then(|s| s.get("wait_time_seconds")).filter(|v| !v.is_null());
            mode(format!("{actor} set slow mode") + &match wait {
                Some(w) => format!(" ({})", humanize_duration(js_to_number(Some(w)))),
                None => String::new(),
            })
        }
        "slowoff" => mode(format!("{actor} turned off slow mode")),
        "followers" => {
            let mins = match sub {
                Some(s) => js_to_number(s.get("follow_duration_minutes")),
                None => 0.0,
            };
            let dur_text = if mins > 0.0 { format!("({})", humanize_duration(mins * 60.0)) } else { "(any follower)".into() };
            mode(format!("{actor} set followers-only chat {dur_text}"))
        }
        "followersoff" => mode(format!("{actor} turned off followers-only chat")),
        "emoteonly" => mode(format!("{actor} turned on emote-only chat")),
        "emoteonlyoff" => mode(format!("{actor} turned off emote-only chat")),
        "uniquechat" => mode(format!("{actor} turned on unique chat")),
        "uniquechatoff" => mode(format!("{actor} turned off unique chat")),
        "subscribers" => mode(format!("{actor} turned on subscribers-only chat")),
        "subscribersoff" => mode(format!("{actor} turned off subscribers-only chat")),
        "mod" => mode(format!("{actor} modded {}", target_name(sub))),
        "unmod" => mode(format!("{actor} unmodded {}", target_name(sub))),
        "vip" => mode(format!("{actor} added {} as a VIP", target_name(sub))),
        "unvip" => mode(format!("{actor} removed {} as a VIP", target_name(sub))),
        "add_blocked_term" | "remove_blocked_term" | "add_permitted_term" | "remove_permitted_term" => {
            let terms = event.get("automod_terms").filter(|v| truthy(v));
            let list: Vec<Value> = terms.and_then(|t| t.get("terms")).and_then(Value::as_array).cloned().unwrap_or_default();
            let verb = if action.starts_with("add") { "added" } else { "removed" };
            let kind_word = if action.contains("permitted") { "permitted" } else { "blocked" };
            let plural = if list.len() == 1 { "" } else { "s" };
            let quoted = list.iter().map(|t| format!("\"{}\"", js_string(t))).collect::<Vec<_>>().join(", ");
            let from_automod = if terms.and_then(|t| t.get("from_automod")).map(truthy).unwrap_or(false) { " (from AutoMod)" } else { "" };
            Some(ModEntry { kind: "terms", text: format!("{actor} {verb} {kind_word} term{plural} {quoted}{from_automod}") })
        }
        "approve_unban_request" => Some(ModEntry { kind: "ban", text: format!("{actor} approved {}'s unban request", target_name(event.get("unban_request").filter(|v| truthy(v)))) }),
        "deny_unban_request" => Some(ModEntry { kind: "ban", text: format!("{actor} denied {}'s unban request", target_name(event.get("unban_request").filter(|v| truthy(v)))) }),
        _ => mode(format!("{actor} used {action}")), // future-proof, unknown action
    }
}

/// The structured twin of `build_moderate_entry` for the one-user actions the user card keeps a history of
/// (ban, timeout, unban, untimeout, warn). Shared-chat actions happened in another channel, so they are skipped.
pub fn moderate_record(action: &str, actor: &str, event: &Value) -> Option<Value> {
    if !["ban", "timeout", "unban", "untimeout", "warn"].contains(&action) {
        return None;
    }
    let sub = event.get(action).filter(|v| truthy(v))?;
    let user_id = js_str_or_empty(sub.get("user_id"));
    let login = js_str_or_empty(sub.get("user_login"));
    if user_id.is_empty() && login.is_empty() {
        return None;
    }
    let secs = match (action, sub.get("expires_at").filter(|v| truthy(v))) {
        ("timeout", Some(e)) => Some(seconds_until(&js_string(e)) as i64),
        _ => None,
    };
    let mut reason = js_str_or_empty(sub.get("reason"));
    if action == "warn" {
        if let Some(rules) = sub.get("chat_rules_cited").and_then(Value::as_array).filter(|r| !r.is_empty()) {
            let cited = rules.iter().map(|r| if r.is_null() { String::new() } else { js_string(r) }).collect::<Vec<_>>().join(", ");
            reason = if reason.is_empty() { cited } else { format!("{reason}; {cited}") };
        }
    }
    Some(json!({"action": action, "by": actor, "userId": user_id, "login": login, "secs": secs, "reason": reason}))
}

// ---------- AutoMod hold/update -> HeldMsg (CHAT v5b) ----------

pub fn map_automod_status(status: &str) -> &'static str {
    match status {
        "Approved" => "approved",
        "Denied" => "denied",
        _ => "expired", // "Expired" or any other/unrecognized value
    }
}

/// Shared by handleAutomodHold (status "held", resolvedBy null) and handleAutomodUpdate's synthesize-if-absent
/// branch (a mod may resolve a message that was held before this session connected).
pub fn build_held_msg(event: &Value, status: &str, resolved_by: Option<&str>) -> Value {
    let is_blocked_term = event.get("reason").and_then(Value::as_str) == Some("blocked_term");
    let s = |k: &str| js_str_or_empty(event.get(k));
    let or_empty = |a: String, b: String| if a.is_empty() { b } else { a };
    let automod = event.get("automod").filter(|v| truthy(v));
    json!({
        "id": event.get("message_id").cloned().unwrap_or(Value::Null),
        "ts": event.get("held_at").and_then(Value::as_str).and_then(parse_rfc3339_ms).filter(|t| *t != 0).unwrap_or_else(now_ms),
        "user": { "id": s("user_id"), "login": s("user_login"), "name": or_empty(s("user_name"), s("user_login")) },
        "text": event.get("message").map(|m| js_str_or_empty(m.get("text"))).unwrap_or_default(),
        "reason": if is_blocked_term { "blocked_term" } else { "automod" },
        "category": if is_blocked_term { Value::Null } else { automod.and_then(|a| a.get("category")).filter(|v| truthy(v)).cloned().unwrap_or(Value::Null) },
        "level": if is_blocked_term { Value::Null } else { automod.and_then(|a| a.get("level")).filter(|v| v.is_number()).cloned().unwrap_or(Value::Null) },
        "status": status,
        "resolvedBy": resolved_by,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_texts() {
        assert_eq!(followers_mode_text("-1"), "Followers-only chat disabled");
        assert_eq!(followers_mode_text("0"), "Followers-only chat enabled (any duration)");
        assert_eq!(followers_mode_text("30"), "Followers-only chat enabled (30 min)");
        assert_eq!(slow_mode_text("0"), "Slow mode off");
        assert_eq!(slow_mode_text("5"), "Slow mode set to 5s");
        assert_eq!(toggle_mode_text("Emote-only mode", true), "Emote-only mode enabled");
    }

    #[test]
    fn durations() {
        assert_eq!(humanize_duration(90.0), "1m 30s");
        assert_eq!(humanize_duration(600.0), "10m");
        assert_eq!(humanize_duration(3600.0), "1h");
        assert_eq!(humanize_duration(86400.0), "1d");
        assert_eq!(humanize_duration(604800.0), "1w");
        assert_eq!(humanize_duration(0.0), "0s");
    }

    #[test]
    fn moderate_entries() {
        let e = json!({"ban": {"user_name": "Troll", "reason": "spam"}});
        let m = build_moderate_entry("ban", "Mod", &e).unwrap();
        assert_eq!((m.kind, m.text.as_str()), ("ban", "Mod banned Troll: spam"));
        let e = json!({"shared_chat_delete": {"user_login": "x"}});
        assert_eq!(build_moderate_entry("shared_chat_delete", "Mod", &e).unwrap().text, "Mod deleted a message from x (shared chat)");
        let e = json!({"automod_terms": {"terms": ["a", "b"], "from_automod": true}});
        assert_eq!(build_moderate_entry("add_blocked_term", "Mod", &e).unwrap().text, "Mod added blocked terms \"a\", \"b\" (from AutoMod)");
        assert_eq!(build_moderate_entry("weird", "Mod", &json!({})).unwrap().text, "Mod used weird");
    }

    #[test]
    fn moderate_records() {
        let e = json!({"ban": {"user_id": "7", "user_login": "troll", "reason": "spam"}});
        let r = moderate_record("ban", "Mod", &e).unwrap();
        assert_eq!((r["userId"].as_str(), r["by"].as_str(), r["reason"].as_str()), (Some("7"), Some("Mod"), Some("spam")));
        let e = json!({"warn": {"user_id": "7", "user_login": "troll", "reason": "hey", "chat_rules_cited": ["No spam"]}});
        assert_eq!(moderate_record("warn", "Mod", &e).unwrap()["reason"], "hey; No spam");
        assert!(moderate_record("slow", "Mod", &json!({})).is_none());
        assert!(moderate_record("shared_chat_ban", "Mod", &json!({"shared_chat_ban": {"user_id": "7"}})).is_none());
    }

    #[test]
    fn fragments() {
        let m = json!({"text": "hi Kappa Kappa", "fragments": [{"type":"text","text":"hi "},{"type":"emote","text":"Kappa","emote":{"id":"25"}},{"type":"text","text":" "},{"type":"emote","text":"Kappa","emote":{"id":"25"}}]});
        assert_eq!(text_from_fragments(&m), ("hi Kappa Kappa".to_string(), "25:3-7,9-13".to_string()));
    }

    #[test]
    fn held() {
        let e = json!({"message_id":"m1","user_id":"1","user_login":"x","held_at":"2026-01-01T00:00:00Z","message":{"text":"bad"},"reason":"automod","automod":{"category":"aggressive","level":2}});
        let h = build_held_msg(&e, "held", None);
        assert_eq!(h["ts"], 1_767_225_600_000i64);
        assert_eq!(h["level"], 2);
        assert_eq!(h["resolvedBy"], Value::Null);
        assert_eq!(map_automod_status("Approved"), "approved");
    }
}
