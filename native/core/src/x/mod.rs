//! Stub for src/main/x/index.js. The X backend is ported in a later round; until then an X source only
//! reports itself unavailable. `normalize_channel` is the broadcast-link half of the JS one; a username is
//! lowercased with its @ dropped (the real one validates it with normalizeHandle from vendor/xLive.js).
use std::sync::Arc;

use crate::platforms::{Ctx, Emit, PlatformModule, Source, UnavailableSource};
use crate::protocol::Platform;
use crate::util::js_trim;

pub struct X;

fn is_id(s: &str) -> bool {
    (13..=14).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `(?:broadcasts?|i/broadcasts)/([A-Za-z0-9]{13,14})(?![A-Za-z0-9])`
fn broadcast_from_url(raw: &str) -> Option<String> {
    for marker in ["broadcasts/", "broadcast/"] {
        let mut from = 0;
        while let Some(i) = raw[from..].find(marker) {
            let start = from + i + marker.len();
            let id: String = raw[start..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
            if is_id(&id) {
                return Some(id);
            }
            from = start;
        }
    }
    None
}

impl PlatformModule for X {
    fn platform(&self) -> Platform {
        Platform::X
    }

    /// "broadcast:<id>" for a broadcast link/id, otherwise the lowercased X username.
    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        let raw = js_trim(input);
        if raw.is_empty() {
            return Err("Enter an X username or a broadcast link.".into());
        }
        if let Some(id) = raw.strip_prefix("broadcast:").filter(|id| is_id(id)) {
            return Ok(format!("broadcast:{id}"));
        }
        // Only a URL counts as a broadcast: a bare 13-character word is far likelier a username.
        if let Some(id) = broadcast_from_url(raw) {
            return Ok(format!("broadcast:{id}"));
        }
        let handle = raw.trim_start_matches('@').to_lowercase();
        if handle.is_empty() || !handle.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || handle.len() > 15 {
            return Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into());
        }
        Ok(handle)
    }

    fn create_source(&self, _channel: String, emit: Emit, _ctx: Arc<Ctx>) -> Arc<dyn Source> {
        UnavailableSource::new(emit)
    }
}
