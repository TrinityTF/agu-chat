//! Port of src/main/settingsSanitize.js: validation for settings values that are not plain scalars (lists of
//! ignore rules). Pure. The renderer sends whole lists; anything malformed is dropped entry by entry rather
//! than rejecting the patch, and a settings.json edited by hand is cleaned on load.
use std::collections::HashSet;

use serde_json::{json, Map, Value};

use crate::util::{js_trim, slice_utf16};

const PLATFORMS: [&str; 5] = ["twitch", "kick", "youtube", "rumble", "x"];
const MAX_ENTRIES: usize = 1000;
const MAX_LEN: usize = 500;

fn str_of(v: Option<&Value>, max: usize) -> String {
    match v {
        Some(Value::String(s)) => slice_utf16(s, max),
        _ => String::new(),
    }
}

/// `[{ platform, login }]`: login lowercased, leading @ dropped, duplicates removed.
pub fn sanitize_ignored_users(list: &Value) -> Option<Value> {
    let list = list.as_array()?;
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for e in list {
        let Some(o) = e.as_object() else { continue };
        let Some(platform) = o.get("platform").and_then(Value::as_str).filter(|p| PLATFORMS.contains(p)) else { continue };
        let raw = str_of(o.get("login"), 100);
        let trimmed = js_trim(&raw);
        let login = trimmed.strip_prefix('@').unwrap_or(trimmed).to_lowercase();
        if login.is_empty() {
            continue;
        }
        if !seen.insert(format!("{platform}:{login}")) {
            continue;
        }
        out.push(json!({"platform": platform, "login": login}));
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    Some(Value::Array(out))
}

/// `[{ pattern, regex, caseSensitive, action: "hide" | "replace", replacement }]`. Empty patterns are dropped.
pub fn sanitize_ignored_phrases(list: &Value) -> Option<Value> {
    let list = list.as_array()?;
    let mut out = Vec::new();
    for e in list {
        let Some(o) = e.as_object() else { continue };
        let pattern = str_of(o.get("pattern"), MAX_LEN);
        if js_trim(&pattern).is_empty() {
            continue;
        }
        out.push(json!({
            "pattern": pattern,
            "regex": o.get("regex") == Some(&Value::Bool(true)),
            "caseSensitive": o.get("caseSensitive") == Some(&Value::Bool(true)),
            "action": if o.get("action").and_then(Value::as_str) == Some("replace") { "replace" } else { "hide" },
            "replacement": str_of(o.get("replacement"), MAX_LEN),
        }));
        if out.len() >= MAX_ENTRIES {
            break;
        }
    }
    Some(Value::Array(out))
}

fn list_sanitizer(key: &str) -> Option<fn(&Value) -> Option<Value>> {
    match key {
        "ignoredUsers" => Some(sanitize_ignored_users),
        "ignoredPhrases" => Some(sanitize_ignored_phrases),
        _ => None,
    }
}

/// `typeof`, as far as JSON values go.
fn type_of(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        _ => 4, // object, array and null are all "object"
    }
}

/// The value to store for `key`, or None to ignore it. Scalars must have the default's type (strings are
/// trimmed); arrays need a sanitizer for that key.
pub fn sanitize_setting(key: &str, value: &Value, defaults: &Map<String, Value>) -> Option<Value> {
    let def = defaults.get(key)?;
    if def.is_array() {
        return list_sanitizer(key).and_then(|f| f(value));
    }
    if value.is_null() || type_of(value) != type_of(def) || value.is_array() {
        return None;
    }
    Some(match value {
        Value::String(s) => Value::String(js_trim(s).to_string()),
        v => v.clone(),
    })
}

/// Defaults overlaid with whatever valid keys a stored settings object has.
pub fn load_settings(stored: &Value, defaults: &Map<String, Value>) -> Map<String, Value> {
    let mut out = defaults.clone();
    let Some(o) = stored.as_object() else { return out };
    for (k, v) in o {
        if let Some(s) = sanitize_setting(k, v, defaults) {
            out.insert(k.clone(), s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    //! Port of test/settingsSanitize.test.js
    use super::*;

    fn defaults() -> Map<String, Value> {
        json!({"fontSize":13,"updateChannel":"beta","timestamps":true,"ignoredUsers":[],"ignoredPhrases":[]}).as_object().unwrap().clone()
    }

    #[test]
    fn scalars_keep_the_defaults_type_strings_are_trimmed() {
        let d = defaults();
        assert_eq!(sanitize_setting("fontSize", &json!(15), &d), Some(json!(15)));
        assert_eq!(sanitize_setting("fontSize", &json!("15"), &d), None);
        assert_eq!(sanitize_setting("updateChannel", &json!(" stable "), &d), Some(json!("stable")));
        assert_eq!(sanitize_setting("nope", &json!(1), &d), None);
        assert_eq!(sanitize_setting("timestamps", &Value::Null, &d), None);
    }

    #[test]
    fn ignored_users_arrays_only_entries_normalized_and_de_duplicated() {
        let d = defaults();
        assert_eq!(sanitize_setting("ignoredUsers", &json!({"platform":"twitch","login":"a"}), &d), None);
        assert_eq!(sanitize_setting("ignoredUsers", &json!("a"), &d), None);
        assert_eq!(
            sanitize_setting(
                "ignoredUsers",
                &json!([{"platform":"twitch","login":" @Alice "},{"platform":"twitch","login":"alice"},{"platform":"nope","login":"x"},{"platform":"kick","login":""},null,"x",{"platform":"kick","login":"Bob"}]),
                &d
            ),
            Some(json!([{"platform":"twitch","login":"alice"},{"platform":"kick","login":"bob"}]))
        );
    }

    #[test]
    fn ignored_phrases_entries_get_every_field_with_a_safe_value() {
        let d = defaults();
        assert_eq!(
            sanitize_setting(
                "ignoredPhrases",
                &json!([{"pattern":"spam"},{"pattern":"  "},{"pattern":"a+","regex":true,"caseSensitive":true,"action":"replace","replacement":"b","extra":1},{"pattern":5}]),
                &d
            ),
            Some(json!([
                {"pattern":"spam","regex":false,"caseSensitive":false,"action":"hide","replacement":""},
                {"pattern":"a+","regex":true,"caseSensitive":true,"action":"replace","replacement":"b"}
            ]))
        );
    }

    #[test]
    fn a_pattern_keeps_its_spaces() {
        let d = defaults();
        assert_eq!(sanitize_setting("ignoredPhrases", &json!([{"pattern":" hi "}]), &d).unwrap()[0]["pattern"], json!(" hi "));
    }

    #[test]
    fn load_settings_overlays_valid_stored_keys_on_the_defaults() {
        let d = defaults();
        let s = load_settings(&json!({"fontSize":16,"timestamps":"yes","ignoredUsers":"bad","ignoredPhrases":[{"pattern":"x"}],"junk":1}), &d);
        assert_eq!(s["fontSize"], json!(16));
        assert_eq!(s["timestamps"], json!(true));
        assert_eq!(s["ignoredUsers"], json!([]));
        assert_eq!(s["ignoredPhrases"].as_array().unwrap().len(), 1);
        assert!(!s.contains_key("junk"));
        assert_eq!(load_settings(&Value::Null, &d), d);
    }
}
