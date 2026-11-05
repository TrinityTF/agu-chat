#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
//! Port of src/renderer/main.tsx, plus the window setup from src/main/main.js (`createWindow`): a frameless
//! 1280x800 window (min 280x300) that restores `window.json`, whose title bar is the notebook's tab bar.
//! The backend is the mock bridge (port of layout/devMock.ts) until the real one is wired in: `AGU_MOCK=1`
//! selects it explicitly, and for now it is also what runs without that variable.
#![allow(dead_code)] // Ports of pieces later rounds use (theme tokens, icons, Toggle, hotkey helpers) stay in.
mod assets;
mod icons;
mod layout;
mod text_input;
mod theme;
mod window_state;

use gpui::{prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions};

use crate::assets::Assets;
use crate::layout::app::AguApp;

fn main() {
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        text_input::bind_keys(cx);

        // AGU_MOCK=1 -> mock bridge; the real backend (agu-chat-core) replaces this later.
        let bridge = layout::dev_mock::MockBridge::new();

        // Where the window opens: its saved bounds, pulled back onto a screen when needed (windowState.js).
        let saved = window_state::load();
        let work: Vec<window_state::Rect> = cx
            .displays()
            .iter()
            .map(|d| {
                let b = d.bounds();
                window_state::Rect { x: b.origin.x.into(), y: b.origin.y.into(), width: b.size.width.into(), height: b.size.height.into() }
            })
            .collect();
        let at = window_state::place_bounds(saved.as_ref(), &work);
        // AGU_WIN_POS=x,y,w,h opens the window exactly there (test runs keep it on the second monitor).
        let forced = std::env::var("AGU_WIN_POS").ok().and_then(|s| {
            let v: Vec<f32> = s.split(',').filter_map(|p| p.trim().parse().ok()).collect();
            (v.len() == 4).then(|| window_state::Rect { x: v[0], y: v[1], width: v[2], height: v[3] })
        });
        let at = forced.or(at);
        let bounds = match at {
            Some(r) => Bounds::new(gpui::point(px(r.x), px(r.y)), size(px(r.width), px(r.height))),
            None => Bounds::centered(None, size(px(1280.), px(800.)), cx),
        };
        let window_bounds = if saved.map(|s| s.maximized).unwrap_or(false) { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) };

        cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                // frame: false -> no OS title bar; the tab bar is the title bar.
                titlebar: Some(TitlebarOptions { title: Some("Agu Chat".into()), appears_transparent: true, traffic_light_position: None }),
                window_min_size: Some(size(px(window_state::MIN_W), px(window_state::MIN_H))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AguApp::new(bridge, window, cx)),
        )
        .unwrap();

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
