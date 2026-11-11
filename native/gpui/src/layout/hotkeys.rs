//! Port of src/renderer/layout/hotkeys.ts.
//! Keyboard shortcuts: the action list with default bindings, binding parsing/formatting, and the merge of
//! the user's overrides (settings.hotkeys) over the defaults. Pure (no GPUI), so `cargo test` loads it directly.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    NewTab,
    CloseSplit,
    CloseTab,
    NextTab,
    PrevTab,
    /// selectTab1..selectTab8 (the number is 1-based)
    SelectTab(u8),
    SelectLastTab,
    OpenSettings,
}

impl HotkeyAction {
    /// The id used in settings.hotkeys ("newTab", "selectTab3", ...).
    pub fn id(self) -> String {
        match self {
            HotkeyAction::NewTab => "newTab".into(),
            HotkeyAction::CloseSplit => "closeSplit".into(),
            HotkeyAction::CloseTab => "closeTab".into(),
            HotkeyAction::NextTab => "nextTab".into(),
            HotkeyAction::PrevTab => "prevTab".into(),
            HotkeyAction::SelectTab(n) => format!("selectTab{n}"),
            HotkeyAction::SelectLastTab => "selectLastTab".into(),
            HotkeyAction::OpenSettings => "openSettings".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct HotkeyDef {
    pub id: HotkeyAction,
    pub label: String,
    pub group: &'static str,
    pub defaults: Vec<String>,
}

fn def(id: HotkeyAction, label: &str, group: &'static str, defaults: &[&str]) -> HotkeyDef {
    HotkeyDef { id, label: label.into(), group, defaults: defaults.iter().map(|s| s.to_string()).collect() }
}

const MAC: bool = cfg!(target_os = "macos");

/// HOTKEY_ACTIONS
pub fn hotkey_actions() -> &'static Vec<HotkeyDef> {
    static CELL: OnceLock<Vec<HotkeyDef>> = OnceLock::new();
    CELL.get_or_init(|| build_actions(MAC))
}

/// The action list with the defaults of one platform: macOS uses Cmd where Windows uses Ctrl (Chrome's Mac
/// shortcuts for switching tabs), keeps Ctrl+Tab, and adds the Cmd+, Settings shortcut every Mac app has.
fn build_actions(mac: bool) -> Vec<HotkeyDef> {
    let p = if mac { "Cmd" } else { "Ctrl" };
    let next: Vec<&str> = if mac { vec!["Ctrl+Tab", "Cmd+Shift+]"] } else { vec!["Ctrl+Tab", "Ctrl+PageDown"] };
    let prev: Vec<&str> = if mac { vec!["Ctrl+Shift+Tab", "Cmd+Shift+["] } else { vec!["Ctrl+Shift+Tab", "Ctrl+PageUp"] };
    let settings: Vec<&str> = if mac { vec!["Cmd+P", "Cmd+,"] } else { vec!["Ctrl+P"] };
    let mut v = vec![
        def(HotkeyAction::NewTab, "New tab", "Tabs", &[&format!("{p}+T")]),
        def(HotkeyAction::CloseTab, "Close tab", "Tabs", &[&format!("{p}+Shift+W")]),
        def(HotkeyAction::NextTab, "Next tab", "Tabs", &next),
        def(HotkeyAction::PrevTab, "Previous tab", "Tabs", &prev),
    ];
    for n in 1..=8u8 {
        v.push(def(HotkeyAction::SelectTab(n), &format!("Select tab {n}"), "Tabs", &[&format!("{p}+{n}")]));
    }
    v.push(def(HotkeyAction::SelectLastTab, "Select last tab", "Tabs", &[&format!("{p}+9")]));
    v.push(def(HotkeyAction::CloseSplit, "Close split", "Splits", &[&format!("{p}+W")]));
    v.push(def(HotkeyAction::OpenSettings, "Open / close settings", "Window", &settings));
    v
}

/// What settings.hotkeys holds: only the actions the user changed (an empty list = unbound).
pub type HotkeyOverrides = BTreeMap<String, Vec<String>>;
pub type HotkeyTable = HashMap<HotkeyAction, Vec<String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub ctrl: bool,
    /// The Command key (macOS only; elsewhere "Cmd" in a saved binding reads as Ctrl).
    pub meta: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

const NAMED_KEYS: [&str; 15] = [
    "Tab", "Enter", "Space", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown", "ArrowUp", "ArrowDown",
    "ArrowLeft", "ArrowRight", "Escape",
];
const KEY_ALIASES: [(&str, &str); 11] = [
    ("esc", "Escape"),
    ("del", "Delete"),
    ("ins", "Insert"),
    ("pgup", "PageUp"),
    ("pgdn", "PageDown"),
    ("pagedn", "PageDown"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("return", "Enter"),
];
const PUNCT: [&str; 11] = ["-", "=", "[", "]", ";", "'", ",", ".", "/", "\\", "`"];

fn norm_key(raw: &str) -> Option<String> {
    let k = raw.trim();
    if k.is_empty() {
        return None;
    }
    let mut chars = k.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_ascii_alphanumeric() {
            return Some(c.to_ascii_uppercase().to_string());
        }
    }
    if PUNCT.contains(&k) {
        return Some(k.to_string());
    }
    let lower = k.to_lowercase();
    if let Some(rest) = lower.strip_prefix('f') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) && rest == n.to_string() {
                return Some(format!("F{n}"));
            }
        }
    }
    if let Some(n) = NAMED_KEYS.iter().find(|n| n.to_lowercase() == lower) {
        return Some((*n).to_string());
    }
    KEY_ALIASES.iter().find(|(a, _)| *a == lower).map(|(_, v)| (*v).to_string())
}

/// "ctrl+shift+w" -> {ctrl, shift, key: "W"}; None for anything that is not a single valid shortcut.
pub fn parse_binding(s: &str) -> Option<Binding> {
    parse_binding_for(s, MAC)
}

fn parse_binding_for(s: &str, mac: bool) -> Option<Binding> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // The key itself may be "+" only as "Plus"; split on "+" that separates parts.
    let parts: Vec<&str> = t.split('+').map(|p| p.trim()).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut b = Binding { ctrl: false, meta: false, alt: false, shift: false, key: String::new() };
    for (i, part) in parts.iter().enumerate() {
        let p = part.to_lowercase();
        let last = i == parts.len() - 1;
        if !last {
            match p.as_str() {
                "ctrl" | "control" => {
                    if b.ctrl {
                        return None;
                    }
                    b.ctrl = true;
                }
                "cmd" | "meta" | "command" | "super" => {
                    // Windows / Linux have no Cmd: a saved "Cmd+T" means the same as "Ctrl+T" there.
                    let slot = if mac { &mut b.meta } else { &mut b.ctrl };
                    if *slot {
                        return None;
                    }
                    *slot = true;
                }
                "alt" | "option" => {
                    if b.alt {
                        return None;
                    }
                    b.alt = true;
                }
                "shift" => {
                    if b.shift {
                        return None;
                    }
                    b.shift = true;
                }
                _ => return None,
            }
        } else {
            b.key = norm_key(part)?;
        }
    }
    Some(b)
}

pub fn format_binding(b: &Binding) -> String {
    let mut out: Vec<&str> = Vec::new();
    if b.ctrl {
        out.push("Ctrl");
    }
    if b.meta {
        out.push("Cmd");
    }
    if b.alt {
        out.push("Alt");
    }
    if b.shift {
        out.push("Shift");
    }
    if !b.key.is_empty() {
        out.push(&b.key);
    }
    out.join("+")
}

/// Canonical form ("ctrl+shift+w" -> "Ctrl+Shift+W"), or None when it does not parse.
pub fn normalize_binding(s: &str) -> Option<String> {
    parse_binding(s).map(|b| format_binding(&b))
}

fn is_fn_key(key: &str) -> bool {
    key.strip_prefix('F').map(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())).unwrap_or(false)
}

/// A binding as the user reads it: macOS calls Alt "Option" ("Cmd+Alt+T" -> "Cmd+Option+T"); elsewhere unchanged.
pub fn display_binding(s: &str) -> String {
    display_binding_for(s, MAC)
}

fn display_binding_for(s: &str, mac: bool) -> String {
    if !mac {
        return s.to_string();
    }
    s.split('+').map(|p| if p == "Alt" { "Option" } else { p }).collect::<Vec<_>>().join("+")
}

/// A binding we let the user assign: needs Ctrl, Cmd or Alt, except the function keys (a bare letter would eat typing).
pub fn is_usable_binding(s: &str) -> bool {
    match parse_binding(s) {
        Some(b) => b.ctrl || b.meta || b.alt || is_fn_key(&b.key),
        None => false,
    }
}

fn code_punct(code: &str) -> Option<&'static str> {
    Some(match code {
        "Minus" => "-",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Semicolon" => ";",
        "Quote" => "'",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "Backslash" => "\\",
        "Backquote" => "`",
        _ => return None,
    })
}

/// A DOM-style key event (`KeyboardEvent`). The GPUI adapter in `app.rs` builds one from a `Keystroke`.
#[derive(Clone, Debug, Default)]
pub struct KeyLike {
    pub key: String,
    pub code: String,
    pub ctrl_key: bool,
    pub alt_key: bool,
    pub shift_key: bool,
    pub meta_key: bool,
}

/// The binding a keydown event produces, or None for a lone modifier / an unknown key. On Windows / Linux Ctrl and
/// Cmd are the same; on macOS they are separate keys.
pub fn event_to_binding(e: &KeyLike) -> Option<String> {
    event_to_binding_for(e, MAC)
}

fn event_to_binding_for(e: &KeyLike, mac: bool) -> Option<String> {
    if ["Control", "Shift", "Alt", "Meta", "AltGraph", "CapsLock", "OS"].contains(&e.key.as_str()) {
        return None;
    }
    let code = e.code.as_str();
    // Physical letter/digit keys, so Shift+1 is "Shift+1" and not "!".
    let key: Option<String> = if let Some(l) = code.strip_prefix("Key").filter(|l| l.len() == 1 && l.chars().all(|c| c.is_ascii_uppercase())) {
        Some(l.to_string())
    } else if let Some(d) = code
        .strip_prefix("Digit")
        .or_else(|| code.strip_prefix("Numpad"))
        .filter(|d| d.len() == 1 && d.chars().all(|c| c.is_ascii_digit()))
    {
        Some(d.to_string())
    } else if let Some(p) = code_punct(code) {
        Some(p.to_string())
    } else {
        norm_key(if e.key == " " { "Space" } else { &e.key })
    };
    let key = key?;
    Some(format_binding(&Binding { ctrl: e.ctrl_key || (!mac && e.meta_key), meta: mac && e.meta_key, alt: e.alt_key, shift: e.shift_key, key }))
}

/// Defaults with the user's overrides applied. Bad or unusable entries and unknown actions are dropped.
pub fn resolve_hotkeys(overrides: &Value) -> HotkeyTable {
    let empty = serde_json::Map::new();
    let o = overrides.as_object().unwrap_or(&empty);
    let mut out = HotkeyTable::new();
    for a in hotkey_actions() {
        let list: Vec<Value> = match o.get(&a.id.id()) {
            Some(Value::Array(v)) => v.clone(),
            _ => a.defaults.iter().map(|s| Value::String(s.clone())).collect(),
        };
        let mut seen = HashSet::new();
        let mut v = Vec::new();
        for s in list {
            let n = s.as_str().and_then(normalize_binding);
            if let Some(n) = n {
                if is_usable_binding(&n) && seen.insert(n.clone()) {
                    v.push(n);
                }
            }
        }
        out.insert(a.id, v);
    }
    out
}

/// binding -> action, for the key handler. When two actions share a binding the first in HOTKEY_ACTIONS wins.
pub fn binding_map(table: &HotkeyTable) -> HashMap<String, HotkeyAction> {
    let mut m = HashMap::new();
    for a in hotkey_actions() {
        for b in table.get(&a.id).into_iter().flatten() {
            m.entry(b.clone()).or_insert(a.id);
        }
    }
    m
}

/// Bindings used by more than one action: binding -> the actions that share it.
pub fn find_conflicts(table: &HotkeyTable) -> HashMap<String, Vec<HotkeyAction>> {
    let mut users: HashMap<String, Vec<HotkeyAction>> = HashMap::new();
    for a in hotkey_actions() {
        for b in table.get(&a.id).into_iter().flatten() {
            users.entry(b.clone()).or_default().push(a.id);
        }
    }
    users.retain(|_, l| l.len() >= 2);
    users
}

/// The overrides that store `table`: actions equal to their defaults are left out.
pub fn to_overrides(table: &HotkeyTable) -> HotkeyOverrides {
    let mut out = HotkeyOverrides::new();
    for a in hotkey_actions() {
        let cur = table.get(&a.id).cloned().unwrap_or_default();
        if cur != a.defaults {
            out.insert(a.id.id(), cur);
        }
    }
    out
}

pub fn action_label(id: HotkeyAction) -> String {
    hotkey_actions().iter().find(|a| a.id == id).map(|a| a.label.clone()).unwrap_or_else(|| id.id())
}

/// The first binding of an action ("Ctrl+T"), for a menu hint or tooltip (port of `useHotkeyHint`); None when unbound.
pub fn hotkey_hint(table: &HotkeyTable, id: HotkeyAction) -> Option<String> {
    table.get(&id).and_then(|v| v.first()).map(|b| display_binding(b))
}

// While the settings page records a new shortcut, the global key handlers (shortcuts, Esc-closes-dialog)
// stand aside so the pressed keys reach the recorder.
static RECORDING: AtomicBool = AtomicBool::new(false);
pub struct HotkeyRecording;
impl HotkeyRecording {
    pub fn active(&self) -> bool {
        RECORDING.load(Ordering::Relaxed)
    }
    pub fn set(&self, v: bool) {
        RECORDING.store(v, Ordering::Relaxed);
    }
}
pub const HOTKEY_RECORDING: HotkeyRecording = HotkeyRecording;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(key: &str, code: &str, ctrl: bool, alt: bool, shift: bool, meta: bool) -> KeyLike {
        KeyLike { key: key.into(), code: code.into(), ctrl_key: ctrl, alt_key: alt, shift_key: shift, meta_key: meta }
    }
    fn b(ctrl: bool, alt: bool, shift: bool, key: &str) -> Option<Binding> {
        Some(Binding { ctrl, meta: false, alt, shift, key: key.into() })
    }
    fn v(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    // Port of test/hotkeys.test.ts

    #[test]
    fn parse_binding_reads_modifiers_and_keys_in_any_case_and_order() {
        assert_eq!(parse_binding("ctrl+shift+w"), b(true, false, true, "W"));
        assert_eq!(parse_binding("Shift+Ctrl+W"), b(true, false, true, "W"));
        assert_eq!(parse_binding("Alt+F4"), b(false, true, false, "F4"));
        assert_eq!(parse_binding("Ctrl+pageup"), b(true, false, false, "PageUp"));
        assert_eq!(parse_binding("Cmd+1"), b(true, false, false, "1"));
        assert_eq!(parse_binding(" Ctrl + Tab "), b(true, false, false, "Tab"));
        assert_eq!(parse_binding("Ctrl+Esc"), b(true, false, false, "Escape"));
    }

    #[test]
    fn parse_binding_rejects_malformed_bindings() {
        for bad in ["", "Ctrl+", "+W", "Ctrl+Ctrl+W", "Hyper+W", "Ctrl+Shift", "Ctrl+WW", "Ctrl+F25", "Ctrl++"] {
            assert_eq!(parse_binding(bad), None, "{bad}");
        }
    }

    #[test]
    fn normalize_binding_gives_one_canonical_spelling() {
        assert_eq!(normalize_binding("shift+alt+ctrl+k").as_deref(), Some("Ctrl+Alt+Shift+K"));
        assert_eq!(normalize_binding("ctrl+pgdn").as_deref(), Some("Ctrl+PageDown"));
        assert_eq!(normalize_binding("f5").as_deref(), Some("F5"));
        assert_eq!(normalize_binding("nope+x"), None);
    }

    #[test]
    fn is_usable_binding_needs_ctrl_or_alt_unless_it_is_a_function_key() {
        assert!(is_usable_binding("Ctrl+T"));
        assert!(is_usable_binding("Alt+1"));
        assert!(is_usable_binding("F2"));
        assert!(!is_usable_binding("T"));
        assert!(!is_usable_binding("Shift+T"));
        assert!(!is_usable_binding("Tab"));
    }

    #[test]
    fn event_to_binding_uses_physical_keys_and_treats_cmd_as_ctrl() {
        assert_eq!(event_to_binding(&ev("w", "KeyW", true, false, false, false)).as_deref(), Some("Ctrl+W"));
        assert_eq!(event_to_binding(&ev("W", "KeyW", true, false, true, false)).as_deref(), Some("Ctrl+Shift+W"));
        assert_eq!(event_to_binding(&ev("!", "Digit1", true, false, true, false)).as_deref(), Some("Ctrl+Shift+1"));
        assert_eq!(event_to_binding(&ev("Tab", "Tab", true, false, true, false)).as_deref(), Some("Ctrl+Shift+Tab"));
        assert_eq!(event_to_binding(&ev("PageDown", "PageDown", true, false, false, false)).as_deref(), Some("Ctrl+PageDown"));
        assert_eq!(event_to_binding(&ev("t", "KeyT", false, false, false, true)).as_deref(), Some("Ctrl+T"));
        assert_eq!(event_to_binding(&ev("_", "Minus", true, false, true, false)).as_deref(), Some("Ctrl+Shift+-"));
        assert_eq!(event_to_binding(&ev(" ", "Space", false, true, false, false)).as_deref(), Some("Alt+Space"));
        assert_eq!(event_to_binding(&ev("Control", "ControlLeft", true, false, false, false)), None);
        assert_eq!(event_to_binding(&ev("Shift", "ShiftLeft", false, false, true, false)), None);
    }

    #[test]
    fn cmd_is_its_own_modifier_on_mac_and_ctrl_elsewhere() {
        let mac = parse_binding_for("cmd+shift+t", true).unwrap();
        assert!(mac.meta && !mac.ctrl);
        assert_eq!(format_binding(&mac), "Cmd+Shift+T");
        assert_eq!(format_binding(&parse_binding_for("Ctrl+Cmd+Alt+K", true).unwrap()), "Ctrl+Cmd+Alt+K");
        assert_eq!(parse_binding_for("Cmd+Cmd+T", true), None);
        let win = parse_binding_for("Cmd+T", false).unwrap();
        assert!(win.ctrl && !win.meta);
        assert_eq!(parse_binding_for("Ctrl+Cmd+T", false), None); // both fold into Ctrl: a repeat
    }

    #[test]
    fn events_keep_cmd_apart_from_ctrl_on_mac_only() {
        let cmd_t = ev("t", "KeyT", false, false, false, true);
        assert_eq!(event_to_binding_for(&cmd_t, true).as_deref(), Some("Cmd+T"));
        assert_eq!(event_to_binding_for(&cmd_t, false).as_deref(), Some("Ctrl+T"));
        let ctrl_tab = ev("Tab", "Tab", true, false, false, false);
        assert_eq!(event_to_binding_for(&ctrl_tab, true).as_deref(), Some("Ctrl+Tab"));
        let cmd_comma = ev(",", "Comma", false, false, false, true);
        assert_eq!(event_to_binding_for(&cmd_comma, true).as_deref(), Some("Cmd+,"));
    }

    #[test]
    fn mac_defaults_use_cmd_and_parse_back() {
        let acts = build_actions(true);
        let get = |a: HotkeyAction| acts.iter().find(|d| d.id == a).unwrap().defaults.clone();
        assert_eq!(get(HotkeyAction::NewTab), v(&["Cmd+T"]));
        assert_eq!(get(HotkeyAction::CloseSplit), v(&["Cmd+W"]));
        assert_eq!(get(HotkeyAction::OpenSettings), v(&["Cmd+P", "Cmd+,"]));
        assert_eq!(get(HotkeyAction::NextTab), v(&["Ctrl+Tab", "Cmd+Shift+]"]));
        let mut seen = HashSet::new();
        for d in &acts {
            for s in &d.defaults {
                let p = parse_binding_for(s, true).unwrap_or_else(|| panic!("{s}"));
                assert_eq!(&format_binding(&p), s);
                assert!(p.ctrl || p.meta);
                assert!(seen.insert(s.clone()), "duplicate default {s}");
            }
        }
    }

    #[test]
    fn display_binding_says_option_on_mac() {
        assert_eq!(display_binding_for("Cmd+Alt+T", true), "Cmd+Option+T");
        assert_eq!(display_binding_for("Cmd+Alt+T", false), "Cmd+Alt+T");
        assert_eq!(display_binding_for("Ctrl+Tab", true), "Ctrl+Tab");
    }

    #[test]
    fn defaults_match_the_original_shortcuts() {
        let t = resolve_hotkeys(&Value::Null);
        assert_eq!(t[&HotkeyAction::NewTab], v(&["Ctrl+T"]));
        assert_eq!(t[&HotkeyAction::CloseSplit], v(&["Ctrl+W"]));
        assert_eq!(t[&HotkeyAction::CloseTab], v(&["Ctrl+Shift+W"]));
        assert_eq!(t[&HotkeyAction::NextTab], v(&["Ctrl+Tab", "Ctrl+PageDown"]));
        assert_eq!(t[&HotkeyAction::PrevTab], v(&["Ctrl+Shift+Tab", "Ctrl+PageUp"]));
        assert_eq!(t[&HotkeyAction::SelectTab(1)], v(&["Ctrl+1"]));
        assert_eq!(t[&HotkeyAction::SelectLastTab], v(&["Ctrl+9"]));
        assert_eq!(t[&HotkeyAction::OpenSettings], v(&["Ctrl+P"]));
        assert_eq!(find_conflicts(&t).len(), 0);
    }

    #[test]
    fn resolve_hotkeys_applies_overrides_and_drops_junk() {
        let t = resolve_hotkeys(&json!({
            "newTab": ["ctrl+n", "ctrl+N", "Q", 7, "bogus+x"],
            "closeSplit": [],
            "madeUp": ["Ctrl+M"],
            "prevTab": "Ctrl+Q",
        }));
        assert_eq!(t[&HotkeyAction::NewTab], v(&["Ctrl+N"])); // normalized, deduped, unusable/invalid dropped
        assert_eq!(t[&HotkeyAction::CloseSplit], Vec::<String>::new()); // explicitly unbound
        assert_eq!(t[&HotkeyAction::PrevTab], v(&["Ctrl+Shift+Tab", "Ctrl+PageUp"])); // not a list: default
        assert_eq!(t.len(), hotkey_actions().len()); // "madeUp" is not an action
        assert_eq!(resolve_hotkeys(&json!([]))[&HotkeyAction::NewTab], v(&["Ctrl+T"]));
        assert_eq!(resolve_hotkeys(&json!("x"))[&HotkeyAction::NewTab], v(&["Ctrl+T"]));
    }

    #[test]
    fn to_overrides_keeps_only_the_changed_actions_and_round_trips() {
        let t = resolve_hotkeys(&json!({}));
        assert!(to_overrides(&t).is_empty());
        let mut changed = t.clone();
        changed.insert(HotkeyAction::NewTab, v(&["Ctrl+N"]));
        changed.insert(HotkeyAction::CloseSplit, vec![]);
        let o = to_overrides(&changed);
        let want: HotkeyOverrides = [("newTab".to_string(), v(&["Ctrl+N"])), ("closeSplit".to_string(), vec![])].into_iter().collect();
        assert_eq!(o, want);
        assert_eq!(resolve_hotkeys(&serde_json::to_value(&o).unwrap()), changed);
    }

    #[test]
    fn binding_map_and_find_conflicts() {
        let t = resolve_hotkeys(&json!({ "newTab": ["Ctrl+W"] }));
        let m = binding_map(&t);
        assert_eq!(m["Ctrl+Tab"], HotkeyAction::NextTab);
        assert_eq!(m["Ctrl+9"], HotkeyAction::SelectLastTab);
        // The earlier action in the list wins a shared binding.
        let acts = hotkey_actions();
        let idx = |a: HotkeyAction| acts.iter().position(|d| d.id == a).unwrap();
        let first = if idx(HotkeyAction::NewTab) < idx(HotkeyAction::CloseSplit) { HotkeyAction::NewTab } else { HotkeyAction::CloseSplit };
        assert_eq!(m["Ctrl+W"], first);
        let mut c = find_conflicts(&t)["Ctrl+W"].clone();
        c.sort_by_key(|a| a.id());
        assert_eq!(c, vec![HotkeyAction::CloseSplit, HotkeyAction::NewTab]);
    }
}
