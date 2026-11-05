//! Port of src/main/store.js: settings (plain JSON) and secrets (encrypted, see safe_storage.rs) in the data
//! dir (`%APPDATA%\Agu Chat` by default, the same folder the Electron app uses), plus `layout.json` and
//! `window.json`. Secrets never leave the backend: the UI only ever sees account state.
//!
//! Settings are kept as JSON maps in the order of DEFAULT_SETTINGS (so settings.json comes out the way the
//! Electron app writes it) and converted to the typed `protocol::Settings` for callers.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use crate::highlight_rules::sanitize_highlight_rules;
use crate::log::Logger;
use crate::protocol::Settings;
use crate::safe_storage::SafeStorage;
use crate::settings_sanitize::{sanitize_ignored_phrases, sanitize_ignored_users, sanitize_setting};
use crate::util::{js_string, len_utf16, slice_utf16, truthy, Listeners};

/// Keep in sync with Settings in protocol.rs (a test checks it equals `Settings::default()`).
pub fn default_settings() -> Map<String, Value> {
    let v = json!({
        "twitchClientId": "",
        "alwaysOnTop": false,
        "updateChannel": "beta", // "beta" = GitHub prereleases too, "stable" = full releases only
        "timestamps": false,
        "timestampSeconds": false,
        "alternateRows": true,
        "fontSize": 13,
        "emoteSize": "medium", // small | medium | large
        "showBadges": true,
        "readableColors": true,
        "sevenTv": true,
        "bttv": true,
        "ffz": true,
        "sevenTvCosmetics": true,
        "animatedEmotes": true, // off: emotes and GIFs stay still, which takes most of the GPU load away
        "deletedMessages": "strike", // strike | hide | show
        "showModButtons": true,
        "showModActions": true, // the mod log lines in chat (bans, timeouts, mode changes)
        "showAutomod": true, // AutoMod's held messages, with Allow / Deny
        "pauseChat": "scroll", // scroll | hover | alt: what holds the chat still, besides scrolling up
        "showPlatformMarks": true,
        "hotkeys": {}, // action id -> ["Ctrl+T", ...]; only the actions the user changed
        // Inline mod-mode buttons, in order. Same defaults as DEFAULT_MOD_BUTTONS in src/renderer/chat/modButtons.ts.
        "modButtons": [
            { "id": "ban", "action": "ban", "icon": "ban", "label": "", "seconds": 0, "command": "" },
            { "id": "timeout", "action": "timeout", "icon": "clock", "label": "", "seconds": 600, "command": "" },
            { "id": "warn", "action": "warn", "icon": "warning", "label": "", "seconds": 0, "command": "" },
            { "id": "delete", "action": "delete", "icon": "trash", "label": "", "seconds": 0, "command": "" },
        ],
        "highlightRules": [], // see highlight_rules.rs
        "mentionSound": false,
        "mentionFlash": true,
        "highlightSoundFile": "", // a file in <data dir>/sounds, "" = the built-in ping
        "highlightVolume": 70, // 0-100
        "highlightSoundFocused": true,
        "ignoredUsers": [], // [{ platform, login }] (see settings_sanitize.rs)
        "ignoredPhrases": [], // [{ pattern, regex, caseSensitive, action: "hide" | "replace", replacement }]
    });
    match v {
        Value::Object(m) => m,
        _ => unreachable!(),
    }
}

const SETTINGS_VERSION: i64 = 3;

fn str_max(v: Option<&Value>, max: usize) -> String {
    match v {
        Some(Value::String(s)) => slice_utf16(s, max),
        _ => String::new(),
    }
}

// Keys whose value is not a plain string/number/boolean: each gets its own check. A sanitizer returns the
// cleaned value, or None to reject the patch for that key.
// The renderer validates the meaning of structured values on read; these only check their shape.
fn sanitize_hotkeys(v: &Value) -> Option<Value> {
    let o = v.as_object()?;
    let mut out = Map::new();
    for (k, list) in o.iter().take(100) {
        let Some(list) = list.as_array() else { continue };
        if len_utf16(k) > 64 {
            continue;
        }
        let kept: Vec<Value> = list.iter().filter(|b| b.as_str().map(|s| len_utf16(s) <= 40).unwrap_or(false)).take(8).cloned().collect();
        out.insert(k.clone(), Value::Array(kept));
    }
    Some(Value::Object(out))
}

fn sanitize_mod_buttons(v: &Value) -> Option<Value> {
    let list = v.as_array()?;
    Some(Value::Array(
        list.iter()
            .filter(|b| b.is_object())
            .take(12)
            .map(|b| {
                let seconds = match b.get("seconds") {
                    Some(n @ Value::Number(_)) => n.clone(),
                    _ => json!(0),
                };
                json!({
                    "id": str_max(b.get("id"), 40),
                    "action": str_max(b.get("action"), 16),
                    "icon": str_max(b.get("icon"), 16),
                    "label": str_max(b.get("label"), 16),
                    "seconds": seconds,
                    "command": str_max(b.get("command"), 500),
                })
            })
            .collect(),
    ))
}

fn sanitize_highlight_volume(v: &Value) -> Option<Value> {
    let n = v.as_f64()?;
    // Math.round: halves go up.
    Some(json!((n + 0.5).floor().clamp(0.0, 100.0) as i64))
}

fn clean(k: &str, v: &Value, defaults: &Map<String, Value>) -> Option<Value> {
    match k {
        "hotkeys" => sanitize_hotkeys(v),
        "modButtons" => sanitize_mod_buttons(v),
        "highlightRules" => sanitize_highlight_rules(v),
        "ignoredUsers" => sanitize_ignored_users(v),
        "ignoredPhrases" => sanitize_ignored_phrases(v),
        "highlightVolume" => sanitize_highlight_volume(v),
        _ => sanitize_setting(k, v, defaults), // scalars: must have the default's type
    }
}

/// The typed view of a settings map. A value that does not fit the typed struct (protocol.rs is stricter
/// than JS in places, e.g. `ModButton.seconds` is a u32 where JS allows any number) is adjusted or skipped,
/// so one odd key never costs the caller the rest. The stored JSON keeps the raw value.
pub fn to_typed(map: &Map<String, Value>) -> Settings {
    let mut acc = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
    for (k, v) in map {
        let mut v = v.clone();
        if k == "modButtons" {
            if let Some(list) = v.as_array_mut() {
                for b in list {
                    if let Some(n) = b.get("seconds").and_then(Value::as_f64) {
                        b["seconds"] = json!(n.round().clamp(0.0, u32::MAX as f64) as u64);
                    }
                }
            }
        }
        let prev = acc[k.as_str()].clone();
        acc[k.as_str()] = v;
        if serde_json::from_value::<Settings>(acc.clone()).is_err() {
            acc[k.as_str()] = prev;
        }
    }
    serde_json::from_value(acc).unwrap_or_default()
}

/// The data dir's files. `read_json` gives None on any error, like store.js's readJson with a null fallback.
pub struct Files {
    dir: PathBuf,
}

impl Files {
    pub fn new(dir: PathBuf) -> Self {
        Files { dir }
    }
    pub fn dir(&self) -> &Path {
        &self.dir
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    pub fn read_json(&self, name: &str) -> Option<Value> {
        let text = std::fs::read_to_string(self.path(name)).ok()?;
        serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
    }
    pub fn write_json(&self, name: &str, value: &Value) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let target = self.path(name);
        let tmp = self.path(&format!("{name}.tmp"));
        std::fs::write(&tmp, serde_json::to_string_pretty(value).map_err(std::io::Error::other)?)?;
        std::fs::rename(&tmp, &target)
    }
}

pub struct SettingsStore {
    files: Arc<Files>,
    log: Arc<Logger>,
    defaults: Map<String, Value>,
    settings: Mutex<Option<Map<String, Value>>>,
    pub events: Listeners<Settings>,
}

impl SettingsStore {
    pub fn new(files: Arc<Files>, log: Arc<Logger>) -> Self {
        SettingsStore { files, log, defaults: default_settings(), settings: Mutex::new(None), events: Listeners::new() }
    }

    fn load(&self) -> Map<String, Value> {
        let mut settings = self.defaults.clone();
        let mut saved = self.files.read_json("settings.json").unwrap_or_else(|| json!({}));
        if let Some(o) = saved.as_object_mut() {
            let v = o.get("v").and_then(Value::as_f64).unwrap_or(1.0);
            // v2 turned timestamps off by default; older files saved every key, so their `true` was just the old default.
            if v < 2.0 {
                o.remove("timestamps");
            }
            // v3 added warn to the default mod buttons: a list still exactly the old three defaults gets the new set.
            if v < 3.0 {
                if let Some(list) = o.get("modButtons").and_then(Value::as_array) {
                    // JS: `${b?.id}:${b?.action}:${b?.seconds}:${b?.label || ""}:${b?.command || ""}`
                    let part = |b: &Value, k: &str| b.get(k).map(js_string).unwrap_or_else(|| "undefined".into());
                    let or_empty = |b: &Value, k: &str| match b.get(k) {
                        Some(x) if truthy(x) => js_string(x),
                        _ => String::new(),
                    };
                    let ids: Vec<String> = list
                        .iter()
                        .map(|b| format!("{}:{}:{}:{}:{}", part(b, "id"), part(b, "action"), part(b, "seconds"), or_empty(b, "label"), or_empty(b, "command")))
                        .collect();
                    if ids.join(",") == "ban:ban:0::,timeout:timeout:600::,delete:delete:0::" {
                        o.remove("modButtons");
                    }
                }
            }
            for (k, v) in o.iter() {
                if !self.defaults.contains_key(k) {
                    continue;
                }
                if let Some(c) = clean(k, v, &self.defaults) {
                    settings.insert(k.clone(), c);
                }
            }
        }
        settings
    }

    /// A copy: callers may mutate it; the live map is never handed out.
    pub fn get(&self) -> Map<String, Value> {
        let mut guard = self.settings.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.load());
        }
        guard.as_ref().unwrap().clone()
    }

    pub fn get_typed(&self) -> Settings {
        to_typed(&self.get())
    }

    /// Applies a partial settings object (unknown keys and invalid values are ignored), saves, and notifies.
    /// Unlike the JS version a failed write is logged instead of thrown: the new settings are in effect
    /// either way (JS also kept them in memory before writing).
    pub fn set(&self, patch: &Value) -> Settings {
        let mut next = self.get();
        if let Some(p) = patch.as_object() {
            for (k, v) in p {
                if !self.defaults.contains_key(k) {
                    continue;
                }
                if let Some(c) = clean(k, v, &self.defaults) {
                    next.insert(k.clone(), c);
                }
            }
        }
        *self.settings.lock().unwrap() = Some(next.clone());
        let mut out = next;
        out.insert("v".into(), json!(SETTINGS_VERSION));
        if let Err(e) = self.files.write_json("settings.json", &Value::Object(out)) {
            self.log.log(&format!("[store] could not write settings.json: {e}"));
        }
        let typed = self.get_typed();
        self.events.emit(&typed);
        typed
    }
}

/// All secrets live in one encrypted blob (a JSON object), `secrets.bin`.
pub struct SecretStore {
    files: Arc<Files>,
    log: Arc<Logger>,
    safe: SafeStorage,
    secrets: Mutex<Option<Map<String, Value>>>,
}

impl SecretStore {
    pub fn new(files: Arc<Files>, log: Arc<Logger>) -> Self {
        let safe = SafeStorage::new(files.dir());
        SecretStore { files, log, safe, secrets: Mutex::new(None) }
    }

    fn load<'a>(&self, guard: &'a mut Option<Map<String, Value>>) -> &'a mut Map<String, Value> {
        if guard.is_none() {
            let mut secrets = Map::new();
            if let Ok(buf) = std::fs::read(self.files.path("secrets.bin")) {
                if self.safe.is_encryption_available() {
                    match self.safe.decrypt_string(&buf).and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())) {
                        Ok(Value::Object(m)) => secrets = m,
                        Ok(_) => self.log.log("[store] secrets.bin does not hold an object"),
                        Err(e) => self.log.log(&format!("[store] could not read secrets.bin: {e}")),
                    }
                }
            }
            *guard = Some(secrets);
        }
        guard.as_mut().unwrap()
    }

    fn save(&self, secrets: &Map<String, Value>) -> Result<(), String> {
        if !self.safe.is_encryption_available() {
            return Err("Secure storage is not available on this system.".into());
        }
        std::fs::create_dir_all(self.files.dir()).map_err(|e| e.to_string())?;
        let enc = self.safe.encrypt_string(&serde_json::to_string(secrets).map_err(|e| e.to_string())?)?;
        std::fs::write(self.files.path("secrets.bin"), enc).map_err(|e| e.to_string())
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        let mut guard = self.secrets.lock().unwrap();
        self.load(&mut guard).get(key).cloned()
    }

    pub fn set(&self, key: &str, value: Value) -> Result<(), String> {
        let mut guard = self.secrets.lock().unwrap();
        let m = self.load(&mut guard);
        m.insert(key.to_string(), value);
        self.save(m)
    }

    pub fn delete(&self, key: &str) -> Result<(), String> {
        let mut guard = self.secrets.lock().unwrap();
        let m = self.load(&mut guard);
        m.remove(key);
        self.save(m)
    }

    /// Key names only, for diagnostics (never the values).
    pub fn keys(&self) -> Vec<String> {
        let mut guard = self.secrets.lock().unwrap();
        self.load(&mut guard).keys().cloned().collect()
    }
}

/// `layout.json`: the notebook (tabs and split trees). Returned as stored; protocol.rs types it.
pub struct LayoutStore {
    files: Arc<Files>,
}

impl LayoutStore {
    pub fn new(files: Arc<Files>) -> Self {
        LayoutStore { files }
    }
    pub fn get(&self) -> Option<Value> {
        self.files.read_json("layout.json").filter(|v| !v.is_null())
    }
    pub fn set(&self, layout: &Value) -> std::io::Result<()> {
        self.files.write_json("layout.json", layout)
    }
}

/// `window.json` (windowState.js reads and writes it through store.js's readJson/writeJson): the main
/// window's x, y, width, height and maximized. The UI owns the window, so it calls these.
pub struct WindowStore {
    files: Arc<Files>,
}

impl WindowStore {
    pub fn new(files: Arc<Files>) -> Self {
        WindowStore { files }
    }
    pub fn load(&self) -> Option<Value> {
        self.files.read_json("window.json").filter(|v| v.is_object())
    }
    pub fn save(&self, state: &Value) -> std::io::Result<()> {
        self.files.write_json("window.json", state)
    }
}

/// Everything under one data dir.
pub struct Store {
    pub files: Arc<Files>,
    pub settings: Arc<SettingsStore>,
    pub secrets: Arc<SecretStore>,
    pub layout: LayoutStore,
    pub window: WindowStore,
}

impl Store {
    pub fn open(dir: PathBuf, log: Arc<Logger>) -> Store {
        let files = Arc::new(Files::new(dir));
        Store {
            settings: Arc::new(SettingsStore::new(files.clone(), log.clone())),
            secrets: Arc::new(SecretStore::new(files.clone(), log)),
            layout: LayoutStore::new(files.clone()),
            window: WindowStore::new(files.clone()),
            files,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("agu-store-{}", crate::util::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = Arc::new(Logger::new(&dir, true));
        (Store::open(dir.clone(), log), dir)
    }

    #[test]
    fn defaults_match_the_protocol_struct() {
        let typed: Settings = serde_json::from_value(Value::Object(default_settings())).unwrap();
        assert_eq!(typed, Settings::default());
        assert_eq!(to_typed(&default_settings()), Settings::default());
        // and no key is missing on either side
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(keys(&Value::Object(default_settings())), keys(&serde_json::to_value(Settings::default()).unwrap()));
    }

    #[test]
    fn patch_is_validated_saved_and_announced() {
        let (s, dir) = temp_store();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        s.settings.events.on(Arc::new(move |st: &Settings| seen2.lock().unwrap().push(st.font_size)));
        let out = s.settings.set(&json!({"fontSize": 16, "timestamps": "yes", "nope": 1, "highlightVolume": 150, "updateChannel": " stable "}));
        assert_eq!(out.font_size, 16.0);
        assert!(!out.timestamps);
        assert_eq!(out.highlight_volume, 100);
        assert_eq!(out.update_channel, "stable");
        assert_eq!(*seen.lock().unwrap(), vec![16.0]);
        let file = s.files.read_json("settings.json").unwrap();
        assert_eq!(file["v"], json!(3));
        assert_eq!(file["fontSize"], json!(16));
        assert!(file.get("nope").is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn migrations_by_version() {
        let (s, dir) = temp_store();
        s.files
            .write_json(
                "settings.json",
                &json!({"v":1,"timestamps":true,"fontSize":15,"modButtons":[
                    {"id":"ban","action":"ban","icon":"ban","label":"","seconds":0,"command":""},
                    {"id":"timeout","action":"timeout","icon":"clock","label":"","seconds":600,"command":""},
                    {"id":"delete","action":"delete","icon":"trash","label":"","seconds":0,"command":""}]}),
            )
            .unwrap();
        let st = s.settings.get_typed();
        assert!(!st.timestamps); // v1 `true` was just the old default
        assert_eq!(st.font_size, 15.0);
        assert_eq!(st.mod_buttons.len(), 4); // the old three defaults became the new four
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn odd_values_do_not_break_the_typed_view() {
        let m = json!({"modButtons":[{"id":"x","action":"timeout","icon":"","label":"","seconds":-5.4,"command":""}],"fontSize":14});
        let mut full = default_settings();
        full.extend(m.as_object().unwrap().clone());
        let t = to_typed(&full);
        assert_eq!(t.mod_buttons[0].seconds, 0);
        assert_eq!(t.font_size, 14.0);
    }

    #[cfg(windows)]
    #[test]
    fn secrets_round_trip() {
        let (s, dir) = temp_store();
        s.secrets.set("twitch", json!({"accessToken": "x"})).unwrap();
        let again = Store::open(dir.clone(), Arc::new(Logger::new(&dir, true)));
        assert_eq!(again.secrets.get("twitch"), Some(json!({"accessToken": "x"})));
        again.secrets.delete("twitch").unwrap();
        assert_eq!(again.secrets.get("twitch"), None);
        std::fs::remove_dir_all(dir).ok();
    }
}
