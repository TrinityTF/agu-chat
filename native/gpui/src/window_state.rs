//! Port of src/main/windowState.js and src/main/windowPlace.js (the UI owns its window here).
//! The main window's position, size and maximized state, saved in `window.json` and restored on start. A saved
//! spot that is no longer on any screen (a monitor unplugged, resolution changed) is pulled back: the window
//! always opens with its title bar on a screen, where it can be grabbed.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The data folder: env `AGU_DATA_DIR`, else `%APPDATA%\Agu Chat` (Electron's userData for this app).
pub fn data_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("AGU_DATA_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Agu Chat"))
}

/// Files are written only into an explicitly given data dir while the UI runs on the mock bridge, so a run
/// can never overwrite a real install's `layout.json` / `window.json`.
pub fn writable() -> bool {
    std::env::var_os("AGU_DATA_DIR").map(|d| !d.is_empty()).unwrap_or(false)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
}

const FILE: &str = "window.json";

pub fn load() -> Option<WindowState> {
    let bytes = std::fs::read(data_dir()?.join(FILE)).ok()?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    serde_json::from_slice(bytes).ok()
}

/// Saves the window's normal (un-maximized) bounds plus whether it was maximized.
pub fn save(s: &WindowState) {
    if !writable() {
        return;
    }
    if let Some(dir) = data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(j) = serde_json::to_string_pretty(s) {
            let _ = std::fs::write(dir.join(FILE), j);
        }
    }
}

// ---- windowPlace.js ---------------------------------------------------------------------

const TITLE_BAR: f32 = 28.0; // the tab bar, which is also the drag area
const GRAB_W: f32 = 120.0; // this much of the title bar must be on a screen to count as reachable
pub const MIN_W: f32 = 280.0;
pub const MIN_H: f32 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

fn overlap(a: Rect, b: Rect) -> (f32, f32) {
    let w = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let h = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    (w.max(0.0), h.max(0.0))
}

/// Where to open, given the saved bounds and the screens' work areas (primary first).
/// Returns `None` to let the window take its default centered spot.
pub fn place_bounds(saved: Option<&WindowState>, work_areas: &[Rect]) -> Option<Rect> {
    let saved = saved?;
    if ![saved.x, saved.y, saved.width, saved.height].iter().all(|v| v.is_finite()) || work_areas.is_empty() {
        return None;
    }
    let bar = Rect { x: saved.x, y: saved.y, width: saved.width, height: TITLE_BAR };
    let mut best: Option<(Rect, f32)> = None;
    for wa in work_areas {
        let (w, h) = overlap(bar, *wa);
        if w >= GRAB_W.min(saved.width) && h >= TITLE_BAR / 2.0 && best.map(|(_, area)| w * h > area).unwrap_or(true) {
            best = Some((*wa, w * h));
        }
    }
    let wa = best.map(|(wa, _)| wa).unwrap_or(work_areas[0]);
    let width = saved.width.min(wa.width).max(MIN_W).round();
    let height = saved.height.min(wa.height).max(MIN_H).round();
    if best.is_none() {
        // Nowhere visible: same size, centered on the primary screen.
        return Some(Rect { x: (wa.x + (wa.width - width) / 2.0).round(), y: (wa.y + (wa.height - height) / 2.0).round(), width, height });
    }
    // Reachable, but keep the whole window on that screen where it fits.
    let x = saved.x.min(wa.x + wa.width - width).max(wa.x).round();
    let y = saved.y.min(wa.y + wa.height - height).max(wa.y).round();
    Some(Rect { x, y, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(x: f32, y: f32, w: f32, h: f32) -> WindowState {
        WindowState { x, y, width: w, height: h, maximized: false }
    }
    fn wa(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn a_saved_spot_on_a_screen_is_kept() {
        let r = place_bounds(Some(&st(100.0, 100.0, 800.0, 600.0)), &[wa(0.0, 0.0, 1920.0, 1040.0)]).unwrap();
        assert_eq!(r, wa(100.0, 100.0, 800.0, 600.0));
    }

    #[test]
    fn a_spot_on_an_unplugged_monitor_is_centered_on_the_primary_screen() {
        let r = place_bounds(Some(&st(-3000.0, 100.0, 800.0, 600.0)), &[wa(0.0, 0.0, 1920.0, 1040.0)]).unwrap();
        assert_eq!(r, wa(560.0, 220.0, 800.0, 600.0));
    }

    #[test]
    fn nothing_saved_means_the_default_spot() {
        assert_eq!(place_bounds(None, &[wa(0.0, 0.0, 1920.0, 1040.0)]), None);
    }

    #[test]
    fn an_oversized_window_is_shrunk_to_the_screen() {
        let r = place_bounds(Some(&st(0.0, 0.0, 5000.0, 4000.0)), &[wa(0.0, 0.0, 1920.0, 1040.0)]).unwrap();
        assert_eq!(r, wa(0.0, 0.0, 1920.0, 1040.0));
    }
}
