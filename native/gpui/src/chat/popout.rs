//! Port of src/renderer/chat/PopoutWindow.tsx (and the `agu-popout:` handling of src/main/main.js): a user card
//! draws into its own OS window, so it can be dragged anywhere on screen, outside the app.
//!
//! Frameless, no taskbar button (`WindowKind::PopUp`), opened beside the clicked name and clamped to the screen
//! that holds it; its height follows its content (`fit_height`). The window is resizable; once the user drags an edge the card fills it (`UserCardView::user_sized`, Electron's
//! `PopoutFill`). Difference: the window is not owned by the main window, so the app closes the pop-outs itself when its main
//! window goes (`main.rs`) and the pane closes its own with it.
use gpui::{point, px, size, AnyWindowHandle, App, Bounds, Entity, Render, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions};

/// `PopoutWindow` initial height (`height=200`) and the minimum the main process gave them.
pub const POPOUT_H0: f32 = 200.0;
pub const POPOUT_MIN_H: f32 = 120.0;

/// Clamp a screen position so the window sits on the display that holds the point (`availLeft`..).
pub fn clamp_to_screen(cx: &App, x: f32, y: f32, width: f32) -> (f32, f32, Option<gpui::DisplayId>) {
    let displays = cx.displays();
    let hit = displays.iter().find(|d| {
        let b = d.bounds();
        x >= f32::from(b.origin.x) && x < f32::from(b.origin.x + b.size.width) && y >= f32::from(b.origin.y) && y < f32::from(b.origin.y + b.size.height)
    });
    let Some(d) = hit.cloned().or_else(|| cx.primary_display()) else { return (x, y, None) };
    let b = d.bounds();
    let (l, t) = (f32::from(b.origin.x), f32::from(b.origin.y));
    let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
    (x.clamp(l, (l + w - width).max(l)), y.clamp(t, (t + h - 200.0).max(t)), Some(d.id()))
}

/// Open the window; `None` when the platform refused (the caller drops the card, like `w.open` returning null).
pub fn open_popout<V: Render + 'static>(cx: &mut App, title: &str, x: f32, y: f32, width: f32, build: impl FnOnce(&mut Window, &mut App) -> Entity<V>) -> Option<AnyWindowHandle> {
    // the display is named: the platform only accepts bounds that lie on the display it opens the window on
    let (x, y, display_id) = clamp_to_screen(cx, x, y, width);
    let opts = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(x), px(y)), size(px(width), px(POPOUT_H0))))),
        titlebar: Some(TitlebarOptions { title: Some(title.to_string().into()), appears_transparent: true, traffic_light_position: None }),
        kind: WindowKind::PopUp,
        display_id,
        is_resizable: true,
        is_minimizable: false,
        window_min_size: Some(size(px(240.), px(POPOUT_MIN_H))),
        ..Default::default()
    };
    cx.open_window(opts, build).ok().map(|h| h.into())
}

/// `fitPopout`: the window's height follows its content, at least 120 and at most the screen less 40.
pub fn fit_height(window: &mut Window, cx: &mut App, content_h: f32) {
    let max = window.display(cx).map(|d| f32::from(d.bounds().size.height) - 40.0).unwrap_or(1000.0);
    let h = content_h.round().clamp(POPOUT_MIN_H, max);
    let cur = window.bounds().size;
    if (f32::from(cur.height) - h).abs() >= 1.0 {
        window.resize(size(cur.width, px(h)));
    }
}
