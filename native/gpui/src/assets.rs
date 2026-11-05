//! The icon SVGs (assets/icons/*.svg, extracted from src/renderer/icons.tsx and platformIcons.tsx),
//! embedded in the exe and served to GPUI's `svg()` element as an `AssetSource`.
use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets"]
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Self::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(Self::iter().filter(|p| p.starts_with(path)).map(|p| SharedString::from(p.to_string())).collect())
    }
}
