//! Port of src/main/highlightRules.js: validation for the `highlightRules` setting (Settings > Highlights).
//! Keep the shape in sync with `HighlightRule` in protocol.rs.
use serde_json::{json, Value};

use crate::util::slice_utf16;

pub const MAX_RULES: usize = 200;
pub const MAX_PATTERN: usize = 500;
const KINDS: [&str; 3] = ["phrase", "regex", "user"];

/// `/^#[0-9a-f]{6}$/i`
fn is_color(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 7 && b[0] == b'#' && b[1..].iter().all(|c| c.is_ascii_hexdigit())
}

/// `/^[A-Za-z0-9._ -]{1,120}$/`: a sound file is a bare name inside the sounds folder (see sounds.rs), never a path.
fn matches_sound_file(s: &str) -> bool {
    let n = s.chars().count();
    (1..=120).contains(&n) && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ' ' | '-'))
}

/// One rule cleaned up, or None when it is not a rule at all.
pub fn sanitize_highlight_rule(raw: &Value) -> Option<Value> {
    let o = raw.as_object()?;
    let id = match o.get("id") {
        Some(Value::String(s)) => slice_utf16(s, 64),
        _ => String::new(),
    };
    if id.is_empty() {
        return None;
    }
    let s = |k: &str| o.get(k).and_then(Value::as_str);
    let sound_file = match s("soundFile") {
        Some(f) if matches_sound_file(f) && !f.starts_with('.') => f.to_string(),
        _ => String::new(),
    };
    let flag = |k: &str| o.get(k) == Some(&Value::Bool(true));
    Some(json!({
        "id": id,
        "enabled": o.get("enabled") != Some(&Value::Bool(false)),
        "kind": match s("kind") { Some(k) if KINDS.contains(&k) => k, _ => "phrase" },
        "pattern": match s("pattern") { Some(p) => slice_utf16(p, MAX_PATTERN), None => String::new() },
        "caseSensitive": flag("caseSensitive"),
        "color": match s("color") { Some(c) if is_color(c) => c.to_lowercase(), _ => "#9147ff".to_string() },
        "sound": flag("sound"),
        "soundFile": sound_file,
        "flash": flag("flash"),
    }))
}

/// The whole list cleaned up (bad entries and duplicate ids dropped), or None when it is not a list.
pub fn sanitize_highlight_rules(value: &Value) -> Option<Value> {
    let list = value.as_array()?;
    let mut out: Vec<Value> = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for raw in list {
        let Some(rule) = sanitize_highlight_rule(raw) else { continue };
        let id = rule["id"].as_str().unwrap_or("").to_string();
        if !ids.insert(id) {
            continue;
        }
        out.push(rule);
        if out.len() >= MAX_RULES {
            break;
        }
    }
    Some(Value::Array(out))
}

pub fn is_sound_file_name(name: &str) -> bool {
    matches_sound_file(name) && !name.starts_with('.')
}


#[cfg(test)]
mod tests {
    //! Port of test/highlightRules.test.mjs
    use super::*;

    #[test]
    fn a_list_is_required() {
        assert_eq!(sanitize_highlight_rules(&json!("nope")), None);
        assert_eq!(sanitize_highlight_rules(&json!({})), None);
        assert_eq!(sanitize_highlight_rules(&Value::Null), None);
        assert_eq!(sanitize_highlight_rules(&json!([])), Some(json!([])));
    }

    #[test]
    fn a_good_rule_passes_through_unchanged() {
        let r = json!({"id":"a","enabled":true,"kind":"regex","pattern":"^hi","caseSensitive":true,"color":"#ff9f1a","sound":true,"soundFile":"ping-1a2b3c4d.mp3","flash":true});
        assert_eq!(sanitize_highlight_rule(&r), Some(r));
    }

    #[test]
    fn bad_fields_get_safe_defaults() {
        let r = sanitize_highlight_rule(&json!({"id":"b","kind":"eval","pattern":5,"color":"red","soundFile":"../../secrets.bin","sound":"yes","enabled":0}));
        assert_eq!(r, Some(json!({"id":"b","enabled":true,"kind":"phrase","pattern":"","caseSensitive":false,"color":"#9147ff","sound":false,"soundFile":"","flash":false})));
        assert_eq!(sanitize_highlight_rule(&json!({"id":"c","enabled":false})).unwrap()["enabled"], json!(false));
        assert_eq!(sanitize_highlight_rule(&json!({"id":"d","color":"#ABCDEF"})).unwrap()["color"], json!("#abcdef"));
        let long = "x".repeat(MAX_PATTERN + 10);
        assert_eq!(sanitize_highlight_rule(&json!({"id":"e","pattern":long})).unwrap()["pattern"].as_str().unwrap().len(), MAX_PATTERN);
    }

    #[test]
    fn entries_without_an_id_non_objects_and_duplicate_ids_are_dropped() {
        let out = sanitize_highlight_rules(&json!([{"id":"a"},null,"x",[],{"pattern":"no id"},{"id":"a","pattern":"dupe"},{"id":"b"}])).unwrap();
        let ids: Vec<&str> = out.as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn the_list_is_capped() {
        let many: Vec<Value> = (0..MAX_RULES + 5).map(|i| json!({"id": i.to_string()})).collect();
        assert_eq!(sanitize_highlight_rules(&Value::Array(many)).unwrap().as_array().unwrap().len(), MAX_RULES);
    }

    #[test]
    fn sound_file_names_are_bare_names_inside_the_sounds_folder() {
        assert!(is_sound_file_name("my sound-1a2b.mp3"));
        assert!(!is_sound_file_name("../x.mp3"));
        assert!(!is_sound_file_name("a/b.mp3"));
        assert!(!is_sound_file_name(r"a\b.mp3"));
        assert!(!is_sound_file_name(".hidden.mp3"));
        assert!(!is_sound_file_name(""));
    }
}
