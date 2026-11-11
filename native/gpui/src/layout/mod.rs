//! Port of src/renderer/layout/ (the notebook shell). One Rust file per TSX/TS file.
pub mod app;
pub mod channel_dialog;
pub mod channel_modes;

pub mod dev_mock;
pub mod highlights_section;
pub mod hotkeys;
pub mod settings_dialog;
pub mod settings_widgets;
pub mod notebook_tabs;
pub mod split_header;
pub mod split_view;
pub mod tab_window;
pub mod window_fit;
pub mod tree;
// The login windows host WebView2 in a raw Win32 window on Windows and a WKWebView in a GPUI window on macOS
// (`webview_host_mac.rs`); Linux has none yet.
#[cfg(windows)]
pub mod twitch_login;
#[cfg(windows)]
pub mod x_login;
#[cfg(windows)]
pub mod youtube_login;
#[cfg(target_os = "macos")]
pub mod webview_host_mac;
#[cfg(target_os = "macos")]
#[path = "twitch_login_mac.rs"]
pub mod twitch_login;
#[cfg(target_os = "macos")]
#[path = "x_login_mac.rs"]
pub mod x_login;
#[cfg(target_os = "macos")]
#[path = "youtube_login_mac.rs"]
pub mod youtube_login;
pub mod ui;
