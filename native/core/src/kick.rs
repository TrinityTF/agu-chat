//! Stub for src/main/kick/index.js. The Kick backend is ported in a later round; until then a Kick source
//! only reports itself unavailable (`{type:"conn", state:"unavailable", detail:"Coming in the native port"}`).
//! `normalize_channel` is the simple part of parseKickSource (a slug or a kick.com link).
use std::sync::Arc;

use crate::platforms::{Ctx, Emit, PlatformModule, Source, UnavailableSource};
use crate::protocol::Platform;
use crate::util::js_trim;

pub struct Kick;

/// A slug check close to the JS one (the exact SLUG_RE comes with the real port).
fn is_slug(s: &str) -> bool {
    (1..=50).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl PlatformModule for Kick {
    fn platform(&self) -> Platform {
        Platform::Kick
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        let err = || "Enter a Kick channel name or a kick.com link.".to_string();
        let raw = js_trim(input);
        let lower = raw.to_lowercase();
        let mut slug = raw.to_string();
        if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("kick.com") || lower.starts_with("www.kick.com") {
            let with_scheme = if lower.starts_with("http") { raw.to_string() } else { format!("https://{raw}") };
            let u = url::Url::parse(&with_scheme).map_err(|_| err())?;
            let host = u.host_str().unwrap_or("").to_lowercase();
            if host != "kick.com" && !host.ends_with(".kick.com") {
                return Err(err());
            }
            slug = u.path_segments().and_then(|mut p| p.find(|s| !s.is_empty())).unwrap_or("").trim().to_string();
        }
        let slug = slug.strip_prefix('@').unwrap_or(&slug).to_string();
        if is_slug(&slug) {
            Ok(slug.to_lowercase().replace('_', "-"))
        } else {
            Err(err())
        }
    }

    fn create_source(&self, _channel: String, emit: Emit, _ctx: Arc<Ctx>) -> Arc<dyn Source> {
        UnavailableSource::new(emit)
    }
}
