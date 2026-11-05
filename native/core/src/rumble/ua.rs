//! Port of src/main/rumble/ua.js: the honest product User-Agent the vendored Rumble code sends. Set once by
//! mod.rs from ctx.user_agent. Rumble's own site code used a browser UA here; this app never does (see
//! docs/ARCHITECTURE.md).
use std::sync::Mutex;

static USER_AGENT: Mutex<String> = Mutex::new(String::new());

pub fn set_user_agent(ua: &str) {
    if !ua.is_empty() {
        *USER_AGENT.lock().unwrap() = ua.to_string();
    }
}

pub fn get_user_agent() -> String {
    let g = USER_AGENT.lock().unwrap();
    if g.is_empty() { "AguChat".to_string() } else { g.clone() }
}
