#![windows_subsystem = "windows"]
//! Port of src/renderer/main.tsx, plus the window setup from src/main/main.js (`createWindow`): a frameless
//! 1280x800 window (min 280x300) that restores `window.json`, whose title bar is the notebook's tab bar.
//! The backend is the real `agu_chat_core::Backend`; `AGU_MOCK=1` selects the mock bridge (port of
//! layout/devMock.ts) instead. A debug build refuses to start the real backend without `AGU_DATA_DIR`, so a
//! development run can never touch the real `%APPDATA%\Agu Chat` folder.
#![allow(dead_code)] // Ports of pieces later rounds use (theme tokens, icons, Toggle, hotkey helpers) stay in.
mod assets;
mod chat;
mod icons;
mod layout;
#[cfg(target_os = "macos")]
mod mac_menu;
mod text_input;
mod theme;
mod window_state;

use gpui::{prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions};

use crate::assets::Assets;
use crate::layout::app::AguApp;

/// `AGU_LOG=1`: the app is a windows-subsystem exe in every build, so stderr goes nowhere; with the switch on, what
/// would be printed (the UI's `eprintln!`s, panics) is appended to `agu-chat.log` in the data dir, next to the
/// backend's own lines.
#[cfg(not(windows))]
fn route_stderr_to_log() {}

#[cfg(windows)]
fn route_stderr_to_log() {
    if !std::env::var("AGU_LOG").map(|v| !v.is_empty() && v != "0").unwrap_or(false) {
        return;
    }
    use std::os::windows::io::IntoRawHandle;
    let path = agu_chat_core::default_data_dir().join("agu-chat.log");
    let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let handle = windows::Win32::Foundation::HANDLE(file.into_raw_handle() as _);
    unsafe {
        let _ = windows::Win32::System::Console::SetStdHandle(windows::Win32::System::Console::STD_ERROR_HANDLE, handle);
        let _ = windows::Win32::System::Console::SetStdHandle(windows::Win32::System::Console::STD_OUTPUT_HANDLE, handle);
    }
    eprintln!("[ui] agu-chat-gpui {} started, stderr routed here", env!("CARGO_PKG_VERSION"));
}

/// Every panic's message and place go to `agu-chat.log` as well: one inside a window callback aborts the app
/// (it cannot unwind into Windows), and the windows-subsystem exe has no stderr to show it.
fn log_panics() {
    let path = agu_chat_core::default_data_dir().join("agu-chat.log");
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let at = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
            let bt = std::backtrace::Backtrace::force_capture();
            let _ = writeln!(f, "{at} [ui] PANIC: {info}\n{bt}");
        }
        prev(info);
    }));
}

#[cfg(feature = "alloc-stats")]
mod alloc_stats;
#[cfg(feature = "alloc-stats")]
#[global_allocator]
static GLOBAL: alloc_stats::Counting = alloc_stats::Counting;

fn main() {
    route_stderr_to_log();
    log_panics();
    Application::new().with_assets(Assets).run(|cx: &mut App| {
        text_input::bind_keys(cx);
        #[cfg(target_os = "macos")]
        mac_menu::install(cx);

        let mock = std::env::var("AGU_MOCK").map(|v| v == "1").unwrap_or(false);
        let (bridge, data_dir): (std::sync::Arc<dyn agu_chat_core::protocol::AguBridge>, std::path::PathBuf) = if mock {
            (layout::dev_mock::MockBridge::new(), window_state::data_dir().unwrap_or_default())
        } else {
            if cfg!(debug_assertions) && std::env::var_os("AGU_DATA_DIR").filter(|d| !d.is_empty()).is_none() {
                eprintln!("agu-chat-gpui: refusing to start a debug build without AGU_DATA_DIR (it would use the real %APPDATA%\\Agu Chat folder)");
                std::process::exit(2);
            }
            let dir = agu_chat_core::default_data_dir();
            window_state::use_real_backend();
            let backend = std::sync::Arc::new(agu_chat_core::Backend::start(dir.clone()));
            // "Restart and update": once the installer is running the backend asks the UI to quit (after the
            // layout is flushed), see updater.rs `NativeInstaller::install`.
            let (qtx, mut qrx) = futures::channel::mpsc::unbounded::<()>();
            backend.set_quit_hook(std::sync::Arc::new(move || {
                let _ = qtx.unbounded_send(());
            }));
            cx.spawn(async move |cx| {
                use futures::StreamExt;
                if qrx.next().await.is_some() {
                    let _ = cx.update(|cx| {
                        for w in cx.windows() {
                            if let Some(h) = w.downcast::<AguApp>() {
                                let _ = h.update(cx, |app, _, _| app.flush_layout());
                            }
                        }
                        cx.quit();
                    });
                }
            })
            .detach();
            (backend, dir)
        };
        if !mock {
            // X login: the core asks for the sign-in window through this hook
            #[cfg(any(windows, target_os = "macos"))]
            {
                layout::x_login::install(cx);
                layout::youtube_login::install(cx);
                layout::twitch_login::install(cx);
            }
        }
        chat::services::init(cx, bridge.clone(), data_dir);

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
        // GPUI places a window on the primary display unless told which one: bounds that sit on another monitor
        // (a restored window.json, AGU_WIN_POS) need that display's id, or they fall back to a centred default.
        let display_id = at.and_then(|r| {
            let (cx0, cy0) = (r.x + r.width / 2.0, r.y + r.height / 2.0);
            cx.displays().iter().find(|d| {
                let b = d.bounds();
                let (x, y, w, h): (f32, f32, f32, f32) = (b.origin.x.into(), b.origin.y.into(), b.size.width.into(), b.size.height.into());
                cx0 >= x && cx0 < x + w && cy0 >= y && cy0 < y + h
            }).map(|d| d.id())
        });
        let window_bounds = if saved.map(|s| s.maximized).unwrap_or(false) { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) };

        let main = cx.open_window(
            WindowOptions {
                window_bounds: Some(window_bounds),
                display_id,
                // frame: false -> no OS title bar; the tab bar is the title bar.
                titlebar: Some(TitlebarOptions { title: Some("Agu Chat".into()), appears_transparent: true, traffic_light_position: layout::macwin::traffic_light_position(28.0) }),
                window_min_size: Some(size(px(window_state::MIN_W), px(window_state::MIN_H))),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| AguApp::new(bridge, window, cx)),
        )
        .unwrap();

        // User cards are windows of their own: they go with the main window (Electron owned them by it).
        let _ = main.update(cx, |_, window, _| chat::popout::set_owner(window));
        let main_id = main.window_id();
        cx.on_window_closed(move |cx| {
            if !cx.windows().iter().any(|w| w.window_id() == main_id) {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
