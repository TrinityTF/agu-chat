//! macOS window behaviour: the AppKit calls behind window fitting, pop-out ownership, the Dock bounce and the
//! title-bar drag. The AppKit part is compiled only on macOS; elsewhere the few cross-platform items are no-ops,
//! so callers need no cfg of their own. gpui 0.2.2 resizes content only (`Window::resize`), has no minimum-size
//! setter after creation and no screen work area, hence the direct AppKit calls (through `objc2`).
use gpui::{MouseDownEvent, Window};

/// Left padding that keeps tabs and card headers clear of the native traffic lights.
pub const TRAFFIC_INSET: f32 = 78.0;

/// Where gpui puts the traffic lights (x, y of the buttons' top-left) so they sit centred in a bar `bar_h` tall.
pub fn traffic_light_position(bar_h: f32) -> Option<gpui::Point<gpui::Pixels>> {
    if cfg!(target_os = "macos") {
        Some(gpui::point(gpui::px(12.), gpui::px(((bar_h - 14.0) / 2.0).max(0.0).round())))
    } else {
        None
    }
}

/// A press on empty title-bar space: a double click zooms (or minimizes, per the system setting), a single press
/// starts a native window drag. A no-op off macOS (Windows handles `WindowControlArea::Drag` itself).
pub fn titlebar_press(ev: &MouseDownEvent, window: &mut Window) {
    #[cfg(target_os = "macos")]
    {
        if ev.click_count >= 2 {
            window.titlebar_double_click();
        } else {
            imp::begin_drag(window);
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (ev, window);
    }
}

#[cfg(target_os = "macos")]
pub use imp::*;

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::c_void;
    use std::sync::Mutex;

    use gpui::Window;
    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use crate::layout::window_fit::{grown_rect, resized_rect};
    use crate::window_state::Rect;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSPoint {
        x: f64,
        y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSSize {
        width: f64,
        height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct NSRect {
        origin: NSPoint,
        size: NSSize,
    }
    unsafe impl Encode for NSPoint {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]);
    }
    unsafe impl Encode for NSSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]);
    }
    unsafe impl Encode for NSRect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[NSPoint::ENCODING, NSSize::ENCODING]);
    }

    extern "C" {
        static _dispatch_main_q: c_void;
        fn dispatch_async_f(queue: *const c_void, context: *mut c_void, work: extern "C" fn(*mut c_void));
    }

    /// Runs `f` on the main queue after the current event handling returns (so never inside a gpui update).
    fn on_main_later<F: FnOnce() + 'static>(f: F) {
        extern "C" fn run<F: FnOnce()>(ctx: *mut c_void) {
            let f = unsafe { Box::from_raw(ctx as *mut F) };
            f();
        }
        let ctx = Box::into_raw(Box::new(f)) as *mut c_void;
        unsafe { dispatch_async_f(std::ptr::addr_of!(_dispatch_main_q), ctx, run::<F>) };
    }

    /// The NSWindow behind a gpui window (the raw handle is its content NSView).
    pub fn ns_window(window: &Window) -> Option<*mut AnyObject> {
        let h = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(a) = h.as_raw() else { return None };
        let view = a.ns_view.as_ptr() as *mut AnyObject;
        let w: *mut AnyObject = unsafe { msg_send![view, window] };
        (!w.is_null()).then_some(w)
    }

    /// Height of the primary screen: AppKit's y axis starts at its bottom-left corner, ours at the top-left.
    fn primary_height() -> f64 {
        unsafe {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            if screens.is_null() {
                return 0.0;
            }
            let n: usize = msg_send![screens, count];
            if n == 0 {
                return 0.0;
            }
            let first: *mut AnyObject = msg_send![screens, objectAtIndex: 0usize];
            let f: NSRect = msg_send![first, frame];
            f.size.height
        }
    }

    /// A rect in top-left-origin screen coordinates from an AppKit one.
    fn to_top(r: NSRect, ph: f64) -> Rect {
        Rect { x: r.origin.x as f32, y: (ph - (r.origin.y + r.size.height)) as f32, width: r.size.width as f32, height: r.size.height as f32 }
    }

    fn frame_top(win: *mut AnyObject) -> Rect {
        let f: NSRect = unsafe { msg_send![win, frame] };
        to_top(f, primary_height())
    }

    /// (window width - content width, same for height): the frame around the content. Zero with a full-size
    /// content view, which is how these windows are made.
    fn frame_delta(win: *mut AnyObject) -> (f32, f32) {
        unsafe {
            let f: NSRect = msg_send![win, frame];
            let view: *mut AnyObject = msg_send![win, contentView];
            if view.is_null() {
                return (0.0, 0.0);
            }
            let c: NSRect = msg_send![view, frame];
            (((f.size.width - c.size.width) as f32).max(0.0), ((f.size.height - c.size.height) as f32).max(0.0))
        }
    }

    /// The screen's visible frame (menu bar and Dock excluded) that holds the window, top-left origin.
    fn visible_frame(win: *mut AnyObject) -> Option<Rect> {
        unsafe {
            let mut screen: *mut AnyObject = msg_send![win, screen];
            if screen.is_null() {
                screen = msg_send![class!(NSScreen), mainScreen];
            }
            if screen.is_null() {
                return None;
            }
            let v: NSRect = msg_send![screen, visibleFrame];
            Some(to_top(v, primary_height()))
        }
    }

    fn is_zoomed(win: *mut AnyObject) -> bool {
        let z: Bool = unsafe { msg_send![win, isZoomed] };
        z.as_bool()
    }

    /// The layout needs a content area of at least `w` x `h` points. Applied on the main queue after the current
    /// event: setting a minimum above the window's size can resize it at once, which must not happen inside a
    /// gpui update (the size change is then dropped).
    pub fn set_min_client(window: &Window, w: f32, h: f32) {
        static LAST: Mutex<Option<(usize, i32, i32)>> = Mutex::new(None);
        let Some(win) = ns_window(window) else { return };
        let (mut w, mut h) = (w as f64, h as f64);
        if let Some(v) = visible_frame(win) {
            w = w.min(v.width as f64);
            h = h.min(v.height as f64);
        }
        let key = (win as usize, w.ceil() as i32, h.ceil() as i32);
        {
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            if *last == Some(key) {
                return;
            }
            *last = Some(key);
        }
        unsafe {
            let _: *mut AnyObject = msg_send![win, retain];
        }
        let win_addr = win as usize;
        on_main_later(move || unsafe {
            let win = win_addr as *mut AnyObject;
            let _: () = msg_send![win, setContentMinSize: NSSize { width: w.ceil(), height: h.ceil() }];
            let _: () = msg_send![win, release];
        });
    }

    /// The biggest content area (points) this window can have on its screen.
    pub fn max_client(window: &Window) -> Option<(f32, f32)> {
        let win = ns_window(window)?;
        let v = visible_frame(win)?;
        let (fw, fh) = frame_delta(win);
        Some(((v.width - fw).max(0.0), (v.height - fh).max(0.0)))
    }

    /// (window, new outer rect) for a content area of at least `w` x `h`, grown toward `edge`; None when zoomed,
    /// fullscreen or already big enough. Apply with `apply_grow` outside any gpui update of the window.
    pub fn plan_grow(window: &Window, w: f32, h: f32, edge: Option<crate::layout::tree::Edge>) -> Option<(isize, Rect)> {
        let win = ns_window(window)?;
        if window.is_fullscreen() || is_zoomed(win) {
            return None;
        }
        let cur = frame_top(win);
        let (fw, fh) = frame_delta(win);
        let work = visible_frame(win)?;
        let to = grown_rect(cur, (w + fw).ceil(), (h + fh).ceil(), edge, work);
        (to != cur).then_some((win as isize, to))
    }

    /// Like `plan_grow` for an exact size (it grows or shrinks, the change taken at the left / top when asked).
    pub fn plan_resize(window: &Window, w: f32, h: f32, from_left: bool, from_top: bool) -> Option<(isize, Rect)> {
        let win = ns_window(window)?;
        if window.is_fullscreen() || is_zoomed(win) {
            return None;
        }
        let cur = frame_top(win);
        let (fw, fh) = frame_delta(win);
        let work = visible_frame(win)?;
        let to = resized_rect(cur, (w + fw).ceil(), (h + fh).ceil(), from_left, from_top, work);
        (to != cur).then_some((win as isize, to))
    }

    /// Moves / resizes the window to `to` (top-left origin). Called right after the plan, on the main thread,
    /// outside any gpui update.
    pub fn apply_grow(win: isize, to: Rect) {
        let win = win as *mut AnyObject;
        if win.is_null() {
            return;
        }
        let ph = primary_height();
        let r = NSRect {
            origin: NSPoint { x: to.x as f64, y: ph - (to.y as f64 + to.height as f64) },
            size: NSSize { width: to.width as f64, height: to.height as f64 },
        };
        unsafe {
            let _: () = msg_send![win, setFrame: r, display: Bool::YES];
        }
    }

    /// A pop-out: owned by the main window (a child window stays above it and goes with it when it is minimized,
    /// like the owned Win32 windows), no zoom button, and in front: the click that opened it went to the main
    /// window.
    pub fn tool_window(window: &Window, owner: isize) {
        let Some(win) = ns_window(window) else { return };
        unsafe {
            let owner = owner as *mut AnyObject;
            if !owner.is_null() && owner != win {
                // NSWindowAbove = 1
                let _: () = msg_send![owner, addChildWindow: win, ordered: 1isize];
            }
            // NSWindowZoomButton = 2
            let zoom: *mut AnyObject = msg_send![win, standardWindowButton: 2isize];
            if !zoom.is_null() {
                let _: () = msg_send![zoom, setEnabled: Bool::NO];
            }
            let _: () = msg_send![win, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
        }
    }

    /// Keeps the window above other apps' windows (NSFloatingWindowLevel = 3) or back at NSNormalWindowLevel (0).
    pub fn set_always_on_top(window: &Window, on: bool) {
        let Some(win) = ns_window(window) else { return };
        let level: isize = if on { 3 } else { 0 };
        unsafe {
            let _: () = msg_send![win, setLevel: level];
        }
    }

    /// Bounces the Dock icon once (informational, not critical); AppKit ignores it while the app is active.
    pub fn request_attention() {
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            // NSInformationalRequest = 10
            let _: isize = msg_send![app, requestUserAttention: 10usize];
        }
    }

    /// Hands the current mouse press to AppKit's window drag. Deferred to the main queue: the drag runs its own
    /// event loop, which must not run inside gpui's event handling. Only started when the button is still down.
    pub fn begin_drag(window: &Window) {
        let Some(win) = ns_window(window) else { return };
        unsafe {
            let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            if app.is_null() {
                return;
            }
            let ev: *mut AnyObject = msg_send![app, currentEvent];
            if ev.is_null() {
                return;
            }
            let _: *mut AnyObject = msg_send![ev, retain];
            let _: *mut AnyObject = msg_send![win, retain];
            let (win_addr, ev_addr) = (win as usize, ev as usize);
            on_main_later(move || {
                let (win, ev) = (win_addr as *mut AnyObject, ev_addr as *mut AnyObject);
                let down: usize = msg_send![class!(NSEvent), pressedMouseButtons];
                if down & 1 != 0 {
                    let _: () = msg_send![win, performWindowDragWithEvent: ev];
                }
                let _: () = msg_send![ev, release];
                let _: () = msg_send![win, release];
            });
        }
    }
}
