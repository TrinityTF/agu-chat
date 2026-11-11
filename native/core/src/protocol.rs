//! Port of src/renderer/protocol.ts: the contract between the backend (this crate, the port of
//! src/main) and the UI (native/gpui, the port of src/renderer). Change it only when both sides change.
//! Frames, api bodies and account args stay JSON (`serde_json::Value`) exactly as in the Electron app,
//! so the ported code on both sides reads the same fields the JS did.
use std::collections::HashMap;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Twitch,
    Kick,
    Youtube,
    Rumble,
    X,
}

impl Platform {
    pub const ALL: [Platform; 5] = [Platform::Twitch, Platform::Kick, Platform::Youtube, Platform::Rumble, Platform::X];
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Twitch => "twitch",
            Platform::Kick => "kick",
            Platform::Youtube => "youtube",
            Platform::Rumble => "rumble",
            Platform::X => "x",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceRef {
    pub platform: Platform,
    pub channel: String,
}

/// Every frame is a JSON object with a `type`. `{type:"conn", ...}` is the uniform connection frame
/// (see `ConnFrame`); every other frame is the platform's own agu.lol relay frame.
pub type Frame = Value;

/// The `conn` frame's fields, for code that wants them typed. Parse with `serde_json::from_value`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnFrame {
    pub state: String, // connecting | open | offline | error | unavailable
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_send: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_reason: Option<String>,
    /// true when sending is blocked because no account is logged in for this platform
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logged_out: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiResult {
    pub status: u16,
    pub body: Value,
}

#[derive(Clone, Debug, Default)]
pub struct ApiInit {
    /// GET when None.
    pub method: Option<String>,
    pub body: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountUser {
    pub id: String,
    pub login: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevicePrompt {
    pub user_code: String,
    pub verification_uri: String,
    /// ms since epoch
    pub expires_at: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountState {
    pub connected: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unsupported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<AccountUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<DevicePrompt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub type AccountsState = HashMap<Platform, AccountState>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightRule {
    pub id: String,
    pub enabled: bool,
    /// phrase | regex | user
    pub kind: String,
    pub pattern: String,
    pub case_sensitive: bool,
    /// #rrggbb
    pub color: String,
    pub sound: bool,
    pub sound_file: String,
    pub flash: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IgnoredUser {
    pub platform: Platform,
    pub login: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IgnoredPhrase {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    /// hide | replace
    pub action: String,
    pub replacement: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModButton {
    pub id: String,
    /// ban | timeout | warn | delete | command
    pub action: String,
    /// ban | clock | trash | shield | warning | flag | eyeoff | chat | pin | check | ""
    pub icon: String,
    pub label: String,
    pub seconds: u32,
    pub command: String,
}

/// Keep in sync with DEFAULT_SETTINGS in src/main/store.js (the backend's store port owns the defaults
/// and validation; `Default` here must equal them).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub twitch_client_id: String,
    pub always_on_top: bool,
    pub start_with_windows: bool,
    /// beta | stable
    pub update_channel: String,
    pub timestamps: bool,
    pub timestamp_seconds: bool,
    pub alternate_rows: bool,
    pub font_size: f32,
    /// small | medium | large
    pub emote_size: String,
    pub show_badges: bool,
    pub readable_colors: bool,
    pub seven_tv: bool,
    pub bttv: bool,
    pub ffz: bool,
    pub seven_tv_cosmetics: bool,
    pub animated_emotes: bool,
    /// strike | hide | show
    pub deleted_messages: String,
    pub show_mod_buttons: bool,
    pub show_mod_actions: bool,
    pub show_automod: bool,
    /// scroll | hover | alt
    pub pause_chat: String,
    pub show_platform_marks: bool,
    pub hotkeys: HashMap<String, Vec<String>>,
    pub mod_buttons: Vec<ModButton>,
    pub highlight_rules: Vec<HighlightRule>,
    pub mention_sound: bool,
    pub mention_flash: bool,
    pub highlight_sound_file: String,
    pub highlight_volume: u8,
    pub highlight_sound_focused: bool,
    pub ignored_users: Vec<IgnoredUser>,
    pub ignored_phrases: Vec<IgnoredPhrase>,
}

impl Default for Settings {
    fn default() -> Self {
        let mb = |id: &str, action: &str, icon: &str, seconds: u32| ModButton {
            id: id.into(),
            action: action.into(),
            icon: icon.into(),
            label: String::new(),
            seconds,
            command: String::new(),
        };
        Settings {
            twitch_client_id: String::new(),
            always_on_top: false,
            start_with_windows: false,
            update_channel: "beta".into(),
            timestamps: false,
            timestamp_seconds: false,
            alternate_rows: true,
            font_size: 13.0,
            emote_size: "medium".into(),
            show_badges: true,
            readable_colors: true,
            seven_tv: true,
            bttv: true,
            ffz: true,
            seven_tv_cosmetics: true,
            animated_emotes: true,
            deleted_messages: "strike".into(),
            show_mod_buttons: true,
            show_mod_actions: true,
            show_automod: true,
            pause_chat: "scroll".into(),
            show_platform_marks: true,
            hotkeys: HashMap::new(),
            mod_buttons: vec![
                mb("ban", "ban", "ban", 0),
                mb("timeout", "timeout", "clock", 600),
                mb("warn", "warn", "warning", 0),
                mb("delete", "delete", "trash", 0),
            ],
            highlight_rules: Vec::new(),
            mention_sound: false,
            mention_flash: true,
            highlight_sound_file: String::new(),
            highlight_volume: 70,
            highlight_sound_focused: true,
            ignored_users: Vec::new(),
            ignored_phrases: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SplitNode {
    Leaf {
        id: String,
        sources: Vec<SourceRef>,
        #[serde(rename = "modMode", default)]
        mod_mode: bool,
    },
    Row {
        id: String,
        children: Vec<SplitNode>,
        /// fractions summing to 1
        sizes: Vec<f32>,
    },
    Col {
        id: String,
        children: Vec<SplitNode>,
        sizes: Vec<f32>,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub root: Option<SplitNode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    pub tabs: Vec<Tab>,
    pub active: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdaterState {
    /// idle | dev | checking | none | available | downloading | ready | error
    pub status: String,
    /// beta | stable
    pub channel: String,
    pub version: Option<String>,
    pub notes: Vec<String>,
    /// 0..1
    pub progress: f32,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub packaged: bool,
}

pub type SubId = String;
pub type Unsubscribe = Box<dyn FnOnce() + Send>;

/// Port of `AguBridge`. Futures are executor-agnostic (the backend runs them on its own tokio runtime and
/// completes them through channels), so the UI can `.await` them from GPUI tasks. Callbacks are invoked
/// on backend threads: the UI must hop to its own thread before touching UI state.
/// Window controls (`win.*`) are not here: the UI owns its windows.
pub trait AguBridge: Send + Sync + 'static {
    fn subscribe(&self, source: SourceRef) -> BoxFuture<'static, Result<SubId, String>>;
    fn unsubscribe(&self, sub_id: SubId) -> BoxFuture<'static, ()>;
    fn on_frame(&self, cb: Arc<dyn Fn(&SubId, &Frame) + Send + Sync>) -> Unsubscribe;
    fn api(&self, source: SourceRef, path: String, init: ApiInit) -> BoxFuture<'static, ApiResult>;

    fn accounts_get(&self) -> BoxFuture<'static, AccountsState>;
    /// Twitch resolves with the DevicePrompt as JSON; Rumble may resolve `{twoFactor: true}`.
    fn accounts_login(&self, platform: Platform, args: Value) -> BoxFuture<'static, Result<Value, String>>;
    fn accounts_cancel(&self, platform: Platform) -> BoxFuture<'static, ()>;
    fn accounts_logout(&self, platform: Platform) -> BoxFuture<'static, ()>;
    fn on_accounts(&self, cb: Arc<dyn Fn(&AccountsState) + Send + Sync>) -> Unsubscribe;

    fn settings_get(&self) -> Settings;
    /// `patch` is a partial Settings object in its JSON (camelCase) form, validated like store.js.
    fn settings_set(&self, patch: Value) -> BoxFuture<'static, Settings>;
    fn on_settings(&self, cb: Arc<dyn Fn(&Settings) + Send + Sync>) -> Unsubscribe;

    fn layout_get(&self) -> Option<Layout>;
    fn layout_set(&self, layout: Layout);

    fn updater_get(&self) -> UpdaterState;
    fn updater_check(&self) -> BoxFuture<'static, ()>;
    fn updater_install(&self) -> BoxFuture<'static, ()>;
    fn on_updater(&self, cb: Arc<dyn Fn(&UpdaterState) + Send + Sync>) -> Unsubscribe;

    fn app_info(&self) -> AppInfo;
    fn open_external(&self, url: String);

    /// Copies a chosen audio file into the sounds folder; the UI shows the file dialog and passes the path.
    fn sounds_import(&self, path: std::path::PathBuf) -> BoxFuture<'static, Result<String, String>>;
    fn sounds_read(&self, file: String) -> BoxFuture<'static, Result<Vec<u8>, String>>;
}
