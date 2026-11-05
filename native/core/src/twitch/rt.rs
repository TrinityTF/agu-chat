//! Port of src/main/twitch/rt.js: the shared handle to the app context. (JS modules must not import
//! "electron"; here nothing in twitch/ knows the backend either: `init` hands the ctx over.)
use std::sync::{Arc, Mutex};

use crate::platforms::Ctx;

static CTX: Mutex<Option<Arc<Ctx>>> = Mutex::new(None);

pub fn set_ctx(ctx: Arc<Ctx>) {
    *CTX.lock().unwrap() = Some(ctx);
}

pub fn ctx() -> Option<Arc<Ctx>> {
    CTX.lock().unwrap().clone()
}

/// `ctx.log("[twitch]", ...)`
pub fn log(msg: &str) {
    if let Some(c) = ctx() {
        c.log(&format!("[twitch] {msg}"));
    }
}

pub fn user_agent() -> String {
    ctx().map(|c| c.user_agent.clone()).unwrap_or_else(|| "AguChat/0 (+https://github.com/TrinityTF/agu-chat)".to_string())
}
