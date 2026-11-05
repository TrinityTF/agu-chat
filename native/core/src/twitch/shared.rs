//! Port of src/main/twitch/shared.js: the app-wide singletons: one IRC connection and one EventSub session
//! for every Twitch source. Created on first use (inside the tokio runtime); `reset` drops them so a new
//! Backend starts clean.
use std::sync::{Arc, Mutex};

use super::eventsub::EventSubManager;
use super::irc_hub::IrcHub;

static HUB: Mutex<Option<Arc<IrcHub>>> = Mutex::new(None);
static ESUB: Mutex<Option<Arc<EventSubManager>>> = Mutex::new(None);

pub fn hub() -> Arc<IrcHub> {
    HUB.lock().unwrap().get_or_insert_with(|| Arc::new(IrcHub::new())).clone()
}

pub fn esub() -> Arc<EventSubManager> {
    ESUB.lock().unwrap().get_or_insert_with(EventSubManager::new).clone()
}

/// Closes both and forgets them.
pub fn reset() {
    if let Some(h) = HUB.lock().unwrap().take() {
        h.close_all();
    }
    if let Some(e) = ESUB.lock().unwrap().take() {
        e.shutdown();
    }
}
