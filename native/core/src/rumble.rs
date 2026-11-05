//! Stub for src/main/rumble/index.js. The Rumble backend is ported in a later round; until then a Rumble
//! source only reports itself unavailable. `normalize_channel` only trims (the real one parses channel and
//! video links with parseRumbleSource).
use std::sync::Arc;

use crate::platforms::{Ctx, Emit, PlatformModule, Source, UnavailableSource};
use crate::protocol::Platform;
use crate::util::js_trim;

pub struct Rumble;

impl PlatformModule for Rumble {
    fn platform(&self) -> Platform {
        Platform::Rumble
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        let raw = js_trim(input);
        if raw.is_empty() {
            return Err("Enter a Rumble channel name, a rumble.com channel link, or a video link.".into());
        }
        Ok(raw.to_string())
    }

    fn create_source(&self, _channel: String, emit: Emit, _ctx: Arc<Ctx>) -> Arc<dyn Source> {
        UnavailableSource::new(emit)
    }
}
