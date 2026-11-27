//! Port of src/renderer/chat/modButtons.ts: the mod-mode row buttons (settings.modButtons), their defaults,
//! validation, durations and the {placeholder} expansion of command buttons.
use agu_chat_core::protocol::ModButton;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

pub const MOD_BUTTON_ICONS: [&str; 10] = ["ban", "clock", "trash", "shield", "warning", "flag", "eyeoff", "chat", "pin", "check"];
const ACTIONS: [&str; 5] = ["ban", "timeout", "warn", "delete", "command"];

pub const MAX_MOD_BUTTONS: usize = 12;
pub const MAX_LABEL: usize = 8;
pub const MAX_COMMAND: usize = 500;
/// Twitch's timeout cap: 2 weeks.
pub const MAX_TIMEOUT: i64 = 1_209_600;

/// The default set: ban, timeout 10 minutes, warn, delete.
pub fn default_mod_buttons() -> Vec<ModButton> {
    let mb = |id: &str, action: &str, icon: &str, seconds: u32| ModButton { id: id.into(), action: action.into(), icon: icon.into(), label: String::new(), seconds, command: String::new() };
    vec![mb("ban", "ban", "ban", 0), mb("timeout", "timeout", "clock", 600), mb("warn", "warn", "warning", 0), mb("delete", "delete", "trash", 0)]
}

/// Plain seconds, or a number with an s/m/h/d/w suffix, clamped to 1s..2 weeks. None if it does not parse.
pub fn parse_duration(s: &str) -> Option<i64> {
    let t = s.trim();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let unit = t[digits.len()..].trim().to_lowercase();
    let mult = match unit.as_str() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        "w" => 604800,
        _ => return None,
    };
    let n: f64 = digits.parse().ok()?;
    Some(((n * mult as f64).round() as i64).clamp(1, MAX_TIMEOUT))
}

/// 600 -> "10m", 90 -> "90s", 86400 -> "1d": the largest unit that divides it evenly.
pub fn format_duration(seconds: i64) -> String {
    let s = seconds.max(1);
    for (unit, n) in [("w", 604800), ("d", 86400), ("h", 3600), ("m", 60)] {
        if s % n == 0 {
            return format!("{}{unit}", s / n);
        }
    }
    format!("{s}s")
}

static SEQ: AtomicU64 = AtomicU64::new(0);
pub fn new_mod_button_id() -> String {
    let n = SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    format!("b{:x}{:x}", t, n)
}

pub fn default_icon(action: &str) -> &'static str {
    match action {
        "ban" => "ban",
        "timeout" => "clock",
        "warn" => "warning",
        "delete" => "trash",
        _ => "chat",
    }
}

/// One stored entry -> a valid button, or None if it can't be one. Fills missing fields with sane values.
pub fn normalize_mod_button(v: &Value) -> Option<ModButton> {
    let o = v.as_object()?;
    let action = o.get("action")?.as_str()?;
    if !ACTIONS.contains(&action) {
        return None;
    }
    let icon = o.get("icon").and_then(|x| x.as_str()).filter(|i| MOD_BUTTON_ICONS.contains(i)).unwrap_or("").to_string();
    let raw_label: String = o.get("label").and_then(|x| x.as_str()).map(|s| s.chars().take(MAX_LABEL).collect()).unwrap_or_default();
    let label = if raw_label.trim().is_empty() { String::new() } else { raw_label };
    let raw_secs = o.get("seconds").and_then(|x| x.as_f64()).filter(|x| x.is_finite()).map(|x| x.round() as i64).unwrap_or(0);
    let seconds = if action == "timeout" { (if raw_secs == 0 { 600 } else { raw_secs }).clamp(1, MAX_TIMEOUT) as u32 } else { 0 };
    let command = if action == "command" { o.get("command").and_then(|x| x.as_str()).map(|s| s.trim().chars().take(MAX_COMMAND).collect::<String>()).unwrap_or_default() } else { String::new() };
    if action == "command" && command.is_empty() {
        return None;
    }
    let id = o.get("id").and_then(|x| x.as_str()).filter(|s| !s.is_empty()).map(|s| s.chars().take(40).collect()).unwrap_or_else(new_mod_button_id);
    let icon = if !icon.is_empty() {
        icon
    } else if !label.is_empty() {
        String::new()
    } else {
        default_icon(action).to_string()
    };
    Some(ModButton { id, action: action.into(), icon, label, seconds, command })
}

/// settings.modButtons -> the buttons to draw, in order. Not an array (missing / corrupt) -> the defaults.
pub fn normalize_mod_buttons(v: &Value) -> Vec<ModButton> {
    let Some(arr) = v.as_array() else { return default_mod_buttons() };
    let mut out: Vec<ModButton> = Vec::new();
    for x in arr {
        let Some(mut b) = normalize_mod_button(x) else { continue };
        if out.iter().any(|o| o.id == b.id) {
            b.id = new_mod_button_id();
        }
        out.push(b);
        if out.len() >= MAX_MOD_BUTTONS {
            break;
        }
    }
    out
}

/// The tooltip: what the button does.
pub fn describe_mod_button(b: &ModButton) -> String {
    match b.action.as_str() {
        "ban" => "Ban".into(),
        "timeout" => format!("Timeout {}", format_duration(b.seconds as i64)),
        "warn" => "Warn".into(),
        "delete" => "Delete message".into(),
        _ => b.command.clone(),
    }
}

#[derive(Default)]
pub struct TemplateVars<'a> {
    /// Login (what commands take).
    pub user: &'a str,
    /// Display name.
    pub name: Option<&'a str>,
    pub user_id: Option<&'a str>,
    pub msg_id: Option<&'a str>,
    pub channel: Option<&'a str>,
    /// The message text.
    pub msg: Option<&'a str>,
}

/// Fills {user} {name} {user-id} {msg-id} {channel} {msg} in a command template. Unknown placeholders stay as
/// typed, and values are inserted once (a value containing "{user}" is not expanded again).
pub fn expand_template(template: &str, vars: &TemplateVars) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 1..];
        let close = tail.find('}');
        let key = close.map(|c| &tail[..c]).filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic() || c == '-'));
        match (close, key) {
            (Some(c), Some(k)) => {
                let v = match k.to_lowercase().as_str() {
                    "user" => Some(vars.user),
                    "name" => Some(vars.name.unwrap_or(vars.user)),
                    "user-id" => vars.user_id,
                    "msg-id" => vars.msg_id,
                    "channel" => vars.channel,
                    "msg" => vars.msg,
                    _ => None,
                };
                match v {
                    Some(v) => out.push_str(&v.replace(['\r', '\n'], " ")),
                    None => {
                        out.push('{');
                        out.push_str(k);
                        out.push('}');
                    }
                }
                rest = &tail[c + 1..];
            }
            _ => {
                out.push('{');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars() -> TemplateVars<'static> {
        TemplateVars { user: "bob", name: Some("Bob"), user_id: Some("123"), msg_id: Some("m-1"), channel: Some("agu"), msg: Some("hello there") }
    }

    #[test]
    fn expand_fills_every_placeholder() {
        let v = vars();
        assert_eq!(expand_template("/timeout {user} 600", &v), "/timeout bob 600");
        assert_eq!(expand_template("/ban {user} spam in {channel}", &v), "/ban bob spam in agu");
        assert_eq!(expand_template("/delete {msg-id}", &v), "/delete m-1");
        assert_eq!(expand_template("@{name} ({user-id}) said: {msg}", &v), "@Bob (123) said: hello there");
        assert_eq!(expand_template("{USER} {User}", &v), "bob bob");
        assert_eq!(expand_template("{user}{user}", &v), "bobbob");
    }

    #[test]
    fn expand_leaves_unknown_alone_and_does_not_re_expand() {
        let only = TemplateVars { user: "bob", ..Default::default() };
        assert_eq!(expand_template("/warn {user} {reason}", &only), "/warn bob {reason}");
        assert_eq!(expand_template("/delete {msg-id}", &only), "/delete {msg-id}");
        assert_eq!(expand_template("{name}", &only), "bob");
        let m = TemplateVars { user: "x", msg: Some("{user}\nline2"), ..Default::default() };
        assert_eq!(expand_template("say {msg}", &m), "say {user} line2");
        assert_eq!(expand_template("{ user } {}", &only), "{ user } {}");
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("600"), Some(600));
        assert_eq!(parse_duration("10m"), Some(600));
        assert_eq!(parse_duration("1H"), Some(3600));
        assert_eq!(parse_duration("2 d"), Some(172800));
        assert_eq!(parse_duration("1w"), Some(604800));
        assert_eq!(parse_duration("99w"), Some(1_209_600));
        assert_eq!(parse_duration("0"), Some(1));
        assert_eq!(parse_duration("ten"), None);
        assert_eq!(parse_duration("1.5m"), None);
        assert_eq!(format_duration(600), "10m");
        assert_eq!(format_duration(90), "90s");
        assert_eq!(format_duration(86400), "1d");
        assert_eq!(format_duration(1_209_600), "2w");
        assert_eq!(format_duration(5400), "90m");
    }

    #[test]
    fn defaults() {
        let d = default_mod_buttons();
        assert_eq!(d.iter().map(|b| (b.action.as_str(), b.seconds)).collect::<Vec<_>>(), vec![("ban", 0), ("timeout", 600), ("warn", 0), ("delete", 0)]);
        assert_eq!(d.iter().map(describe_mod_button).collect::<Vec<_>>(), vec!["Ban", "Timeout 10m", "Warn", "Delete message"]);
        assert_eq!(normalize_mod_buttons(&Value::Null), d);
        assert_eq!(normalize_mod_buttons(&json!({})), d);
        assert_eq!(normalize_mod_buttons(&serde_json::to_value(&d).unwrap()), d);
        assert!(normalize_mod_buttons(&json!([])).is_empty());
    }

    #[test]
    fn normalize_validates_and_fills_in() {
        assert!(normalize_mod_button(&Value::Null).is_none());
        assert!(normalize_mod_button(&json!({"action": "explode"})).is_none());
        assert!(normalize_mod_button(&json!({"id": "c", "action": "command", "command": "   "})).is_none());
        let t = normalize_mod_button(&json!({"id": "t", "action": "timeout"})).unwrap();
        assert_eq!((t.icon.as_str(), t.seconds), ("clock", 600));
        assert_eq!(normalize_mod_button(&json!({"id": "t", "action": "timeout", "seconds": 9e9})).unwrap().seconds, 1_209_600);
        let b = normalize_mod_button(&json!({"id": "b", "action": "ban", "icon": "nope", "label": "PERMA-BAN!", "seconds": 5, "command": "x"})).unwrap();
        assert_eq!((b.icon.as_str(), b.label.as_str(), b.seconds, b.command.as_str()), ("", "PERMA-BA", 0, ""));
        assert_eq!(normalize_mod_button(&json!({"id": "c", "action": "command", "command": "/warn {user} rules", "icon": "warning"})).unwrap().command, "/warn {user} rules");
        assert!(!normalize_mod_button(&json!({"action": "delete"})).unwrap().id.is_empty());
    }

    #[test]
    fn normalize_list_drops_bad_fixes_ids_and_caps() {
        let list = normalize_mod_buttons(&json!([{"id": "a", "action": "ban"}, "junk", {"id": "a", "action": "delete"}, {"action": "nope"}]));
        assert_eq!(list.iter().map(|b| b.action.as_str()).collect::<Vec<_>>(), vec!["ban", "delete"]);
        assert_ne!(list[0].id, list[1].id);
        let many: Vec<Value> = (0..30).map(|i| json!({"id": format!("x{i}"), "action": "delete"})).collect();
        assert_eq!(normalize_mod_buttons(&Value::Array(many)).len(), MAX_MOD_BUTTONS);
    }
}
