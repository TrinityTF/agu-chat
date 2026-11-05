//! Stub for src/main/youtube/index.js. The YouTube backend is ported in a later round; until then a YouTube
//! source only reports itself unavailable. `normalize_channel` only trims (the real one parses handles,
//! channel ids and stream links with parseYouTubeSource).
use std::sync::Arc;

use crate::platforms::{Ctx, Emit, PlatformModule, Source, UnavailableSource};
use crate::protocol::Platform;
use crate::util::js_trim;

pub struct Youtube;

impl PlatformModule for Youtube {
    fn platform(&self) -> Platform {
        Platform::Youtube
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        let raw = js_trim(input);
        if raw.is_empty() {
            return Err("Enter a YouTube @handle, channel id, or a channel / stream link.".into());
        }
        Ok(raw.to_string())
    }

    fn create_source(&self, _channel: String, emit: Emit, _ctx: Arc<Ctx>) -> Arc<dyn Source> {
        UnavailableSource::new(emit)
    }
}
