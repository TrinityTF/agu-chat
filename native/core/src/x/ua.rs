//! Port of src/main/x/ua.js: the honest product User-Agent the vendored X code sends. Set by mod.rs from
//! ctx.user_agent. X's own site code used a browser UA here; this app never does (see docs/ARCHITECTURE.md).
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

/// What requests made with the user's own x.com session send: x.com answers a session from something that does
/// not look like a browser with anti-bot errors, and the session is a browser's, so the same string is used.
pub fn browser_user_agent() -> &'static str {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36"
}
