//! The Settings screens' helpers: the lists and limits the desktop dialog draws from (colour palette, badge ids,
//! mod button icons), validation of what is typed (a regular expression, a timeout length), the highlight tester and
//! the updater's state as the screen shows it. The settings themselves go through `settings.get` / `settings.set`
//! like on the desktop; this is only what the screen needs besides them.
use agu_chat_core::protocol::{HighlightRule, Settings, UpdaterState};
use agu_chat_model::highlights::{compile_highlights, regex_error, MatchInput, BADGES, BADGE_COLORS, RULE_COLORS};
use agu_chat_model::ignores::phrase_error;
use agu_chat_model::mod_buttons::{format_duration, parse_duration, MAX_COMMAND, MAX_LABEL, MAX_MOD_BUTTONS, MOD_BUTTON_ICONS};
use agu_chat_model::settings_text::{new_issue_url, percent_of, sound_label, updater_text, MOD_ICON_LABELS, REPO_URL, TEMPLATE_HELP};
use serde_json::{json, Value};

/// `settings.meta`
pub fn meta(version: &str) -> Value {
    json!({
        "ruleColors": RULE_COLORS,
        "badges": BADGES.iter().map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
        "badgeColors": BADGE_COLORS,
        "modIcons": MOD_BUTTON_ICONS.iter().zip(MOD_ICON_LABELS).map(|(id, label)| json!({"id": id, "label": label})).collect::<Vec<_>>(),
        "maxModButtons": MAX_MOD_BUTTONS,
        "maxLabel": MAX_LABEL,
        "maxCommand": MAX_COMMAND,
        "templateHelp": TEMPLATE_HELP,
        "repoUrl": REPO_URL,
        "issuesUrl": format!("{REPO_URL}/issues"),
        "newIssueUrl": new_issue_url(version, "android"),
    })
}

/// `settings.check`: `{what: "regex", pattern, caseSensitive}` (a highlight rule) or `{what: "phrase", pattern, regex,
/// caseSensitive}` (an ignored phrase). `{error}`: the message, or null when it is fine.
pub fn check(args: &Value) -> Value {
    let pattern = args["pattern"].as_str().unwrap_or("");
    let cs = args["caseSensitive"].as_bool().unwrap_or(false);
    let error = if pattern.trim().is_empty() {
        None
    } else if args["what"].as_str() == Some("phrase") {
        phrase_error(pattern, args["regex"].as_bool().unwrap_or(false), cs)
    } else {
        regex_error(pattern, cs)
    };
    json!({ "error": error })
}

/// `modbtn.duration`: `{text}` -> `{seconds}` (null when it is not a length), or `{seconds}` -> `{text}`.
pub fn duration(args: &Value) -> Value {
    match (args["text"].as_str(), args["seconds"].as_i64()) {
        (Some(t), _) => json!({ "seconds": parse_duration(t) }),
        (None, Some(s)) => json!({ "text": format_duration(s) }),
        _ => json!({ "seconds": null }),
    }
}

/// `highlights.test`: which rule (by position) a sample message from `user` would hit, as the desktop's "Try it".
pub fn highlight_test(s: &Settings, args: &Value) -> Value {
    let rules: Vec<HighlightRule> = s.highlight_rules.clone();
    let user = args["user"].as_str().unwrap_or("").to_string();
    let text = args["text"].as_str().unwrap_or("").to_string();
    if user.is_empty() && text.is_empty() {
        return Value::Null;
    }
    let hl = compile_highlights(&rules);
    match hl.matches(&MatchInput { text, login: user.clone(), name: user, badges: Vec::new() }) {
        Some(h) => json!({ "index": rules.iter().position(|r| r.id == h.id).unwrap_or(0), "kind": h.kind, "pattern": h.pattern, "color": h.color }),
        None => json!({ "index": null }),
    }
}

/// The updater as the screen shows it: the state, the desktop's status line, and the percentage.
pub fn updater_json(u: &UpdaterState) -> Value {
    let mut v = serde_json::to_value(u).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("text".into(), json!(updater_text(u)));
        o.insert("percent".into(), json!(percent_of(u.progress).round() as i64));
    }
    v
}

/// The shown name of a stored sound file (`sounds.label`).
pub fn sound_name(file: &str) -> String {
    sound_label(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, kind: &str, pattern: &str) -> HighlightRule {
        HighlightRule { id: id.into(), enabled: true, kind: kind.into(), pattern: pattern.into(), case_sensitive: false, color: "#ff9f1a".into(), sound: false, sound_file: String::new(), flash: false }
    }

    #[test]
    fn the_meta_carries_the_desktops_lists() {
        let m = meta("0.3.11");
        assert_eq!(m["ruleColors"].as_array().unwrap().len(), 8);
        assert_eq!(m["badges"][2], json!({"id": "lead_moderator", "label": "Lead Moderator"}));
        assert_eq!(m["modIcons"].as_array().unwrap().len(), 10);
        assert_eq!(m["modIcons"][6]["label"], "Hidden");
        assert_eq!(m["maxModButtons"], 12);
        assert!(m["newIssueUrl"].as_str().unwrap().contains("Android"));
    }

    #[test]
    fn a_bad_regular_expression_is_reported_and_an_empty_one_is_not() {
        assert!(check(&json!({"what": "regex", "pattern": "(a"}))["error"].is_string());
        assert!(check(&json!({"what": "regex", "pattern": "^!(drop|claim)"}))["error"].is_null());
        assert!(check(&json!({"what": "regex", "pattern": ""}))["error"].is_null());
        // a plain phrase is escaped, so it cannot be invalid; the same text as a regex can
        assert!(check(&json!({"what": "phrase", "pattern": "(a", "regex": false}))["error"].is_null());
        assert!(check(&json!({"what": "phrase", "pattern": "(a", "regex": true}))["error"].is_string());
    }

    #[test]
    fn timeout_lengths_round_trip() {
        assert_eq!(duration(&json!({"text": "10m"})), json!({"seconds": 600}));
        assert_eq!(duration(&json!({"text": "2w"}))["seconds"], 1_209_600);
        assert_eq!(duration(&json!({"text": "soon"})), json!({"seconds": null}));
        assert_eq!(duration(&json!({"seconds": 86400})), json!({"text": "1d"}));
    }

    #[test]
    fn the_tester_names_the_first_matching_rule() {
        let mut s = Settings::default();
        s.highlight_rules = vec![rule("a", "phrase", "giveaway"), rule("b", "user", "bob")];
        assert_eq!(highlight_test(&s, &json!({"user": "x", "text": "a giveaway!"}))["index"], 0);
        assert_eq!(highlight_test(&s, &json!({"user": "Bob", "text": "hi"}))["index"], 1);
        assert!(highlight_test(&s, &json!({"user": "x", "text": "hello"}))["index"].is_null());
        assert!(highlight_test(&s, &json!({"user": "", "text": ""})).is_null());
    }

    #[test]
    fn the_updater_comes_with_the_desktops_words() {
        let u = UpdaterState { status: "downloading".into(), channel: "beta".into(), version: Some("0.4.0".into()), notes: vec![], progress: 0.6, error: None };
        let j = updater_json(&u);
        assert_eq!(j["text"], "Downloading 0.4.0… 60%");
        assert_eq!(j["percent"], 60);
        assert_eq!(sound_name("My Ping-a9993e36.mp3"), "My Ping.mp3");
    }
}
