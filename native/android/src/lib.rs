//! Agu Chat for Android, the Rust half: the same core and chat logic as the desktop app, behind a JSON bridge.
//! `session` is the bridge (testable on desktop); `jni_glue` exposes it to Kotlin as `lol.agu.chat.Core`.
pub mod alerts;
pub mod card;
pub mod compose;
pub mod hub;
pub mod login;
pub mod session;
pub mod settings_api;
pub mod view;

#[cfg(target_os = "android")]
mod jni_glue;
