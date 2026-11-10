//! Keeps every chat of the active tab at least `MIN_W` wide: the window grows when a split is added (toward the
//! side the split went) and its minimum size follows the layout, so it cannot be dragged narrower than the splits
//! need. gpui 0.2.2 fixes the minimum size at creation, so on Windows the main window is subclassed and
//! `WM_GETMINMAXINFO` is raised after gpui has answered it.
use agu_chat_core::protocol::SplitNode;

use crate::layout::split_view::MIN_H as LEAF_MIN_H;
use crate::layout::tree::Edge;
use crate::window_state::{Rect, MIN_W};

/// the line between two splits
const DIVIDER: f32 = 1.0;
/// a split's share never counts as less than this, so a corrupt size cannot demand an absurd window
const MIN_FRACTION: f32 = 0.05;

/// The smallest (width, height), in logical px, the content area needs so that no split is narrower than `MIN_W`
/// or shorter than a chat's minimum height. A side-by-side container needs its children's widths side by side
/// (each scaled up by its share, so an uneven split still keeps its small side wide enough) plus the dividers;
/// a stacked one the widest child and the heights stacked.
pub fn tree_min_size(n: &SplitNode) -> (f32, f32) {
    match n {
        SplitNode::Leaf { .. } => (MIN_W, LEAF_MIN_H),
        SplitNode::Row { children, sizes, .. } => {
            let kids: Vec<(f32, f32)> = children.iter().map(tree_min_size).collect();
            let fr = fractions(sizes, kids.len());
            let w = kids.iter().zip(&fr).map(|(k, f)| k.0 / f).fold(0.0, f32::max).max(kids.iter().map(|k| k.0).sum());
            (w + DIVIDER * kids.len().saturating_sub(1) as f32, kids.iter().map(|k| k.1).fold(0.0, f32::max))
        }
        SplitNode::Col { children, sizes, .. } => {
            let kids: Vec<(f32, f32)> = children.iter().map(tree_min_size).collect();
            let fr = fractions(sizes, kids.len());
            let h = kids.iter().zip(&fr).map(|(k, f)| k.1 / f).fold(0.0, f32::max).max(kids.iter().map(|k| k.1).sum());
            (kids.iter().map(|k| k.0).fold(0.0, f32::max), h + DIVIDER * kids.len().saturating_sub(1) as f32)
        }
    }
}

fn fractions(sizes: &[f32], n: usize) -> Vec<f32> {
    let raw: Vec<f32> = (0..n).map(|i| sizes.get(i).copied().filter(|v| v.is_finite() && *v > 0.0).unwrap_or(1.0 / n.max(1) as f32)).collect();
    let total: f32 = raw.iter().sum();
    raw.iter().map(|v| (v / total.max(f32::EPSILON)).max(MIN_FRACTION)).collect()
}

/// Where the window goes when it has to be at least `need_w` x `need_h` (all in the same unit, window outer
/// rect): it grows toward the side the new split went (`Left` / `Top` move the origin back by the added size;
/// anything else grows right / down), is pulled back inside `work` on an axis it grew on, and is capped at the
/// work area's size when it cannot fit. A window already big enough is returned as it is.
pub fn grown_rect(cur: Rect, need_w: f32, need_h: f32, edge: Option<Edge>, work: Rect) -> Rect {
    let width = if need_w > cur.width { need_w.min(work.width).max(cur.width.min(work.width)) } else { cur.width };
    let height = if need_h > cur.height { need_h.min(work.height).max(cur.height.min(work.height)) } else { cur.height };
    let mut x = cur.x;
    let mut y = cur.y;
    if width > cur.width {
        if edge == Some(Edge::Left) {
            x -= width - cur.width;
        }
        x = x.min(work.x + work.width - width).max(work.x);
    }
    if height > cur.height {
        if edge == Some(Edge::Top) {
            y -= height - cur.height;
        }
        y = y.min(work.y + work.height - height).max(work.y);
    }
    Rect { x, y, width, height }
}

#[cfg(windows)]
mod win {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
    use windows::Win32::UI::HiDpi::GetDpiForWindow;
    use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, IsZoomed, SetWindowPos, MINMAXINFO, SWP_NOACTIVATE, SWP_NOZORDER, WM_GETMINMAXINFO, WM_NCDESTROY,
    };

    use super::*;

    const SUBCLASS_ID: usize = 0xA60F17;
    /// per window: the client size (logical px) its layout needs
    static NEEDED: Mutex<Option<HashMap<isize, (f32, f32)>>> = Mutex::new(None);

    fn hwnd_of(window: &Window) -> Option<HWND> {
        let h = HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::Win32(w) = h.as_raw() else { return None };
        Some(HWND(w.hwnd.get() as *mut core::ffi::c_void))
    }

    fn scale_of(hwnd: HWND) -> f32 {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi == 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }

    /// (window width - client width, same for height) in device px: the frame around the content.
    fn frame_of(hwnd: HWND) -> (f32, f32) {
        let (mut w, mut c) = (RECT::default(), RECT::default());
        unsafe {
            if GetWindowRect(hwnd, &mut w).is_err() || GetClientRect(hwnd, &mut c).is_err() {
                return (0.0, 0.0);
            }
        }
        (((w.right - w.left) - (c.right - c.left)).max(0) as f32, ((w.bottom - w.top) - (c.bottom - c.top)).max(0) as f32)
    }

    fn work_area(hwnd: HWND) -> Option<Rect> {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            if !GetMonitorInfoW(mon, &mut mi).as_bool() {
                return None;
            }
        }
        let r = mi.rcWork;
        Some(Rect { x: r.left as f32, y: r.top as f32, width: (r.right - r.left) as f32, height: (r.bottom - r.top) as f32 })
    }

    unsafe extern "system" fn subclass_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _id: usize, _data: usize) -> LRESULT {
        // gpui answers first (its own minimum), then the layout's minimum is raised over it.
        let res = DefSubclassProc(hwnd, msg, wparam, lparam);
        if msg == WM_GETMINMAXINFO && lparam.0 != 0 {
            let need = NEEDED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&(hwnd.0 as isize)).copied());
            if let Some((w, h)) = need {
                let scale = scale_of(hwnd);
                let (fw, fh) = frame_of(hwnd);
                let (mut mw, mut mh) = (w * scale + fw, h * scale + fh);
                if let Some(wa) = work_area(hwnd) {
                    mw = mw.min(wa.width);
                    mh = mh.min(wa.height);
                }
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize.x = info.ptMinTrackSize.x.max(mw.ceil() as i32);
                info.ptMinTrackSize.y = info.ptMinTrackSize.y.max(mh.ceil() as i32);
            }
        } else if msg == WM_NCDESTROY {
            if let Some(m) = NEEDED.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                m.remove(&(hwnd.0 as isize));
            }
            let _ = RemoveWindowSubclass(hwnd, Some(subclass_proc), SUBCLASS_ID);
        }
        res
    }

    /// Installs the minimum-size hook on this window (once; installing again is harmless).
    pub fn install(window: &Window) {
        if let Some(h) = hwnd_of(window) {
            unsafe {
                let _ = SetWindowSubclass(h, Some(subclass_proc), SUBCLASS_ID, 0);
            }
        }
    }

    /// The layout needs a client area of at least `w` x `h` logical px.
    pub fn set_min_client(window: &Window, w: f32, h: f32) {
        if let Some(hwnd) = hwnd_of(window) {
            NEEDED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(hwnd.0 as isize, (w, h));
        }
    }

    /// Grows the window so its client area is at least `w` x `h` logical px, toward `edge`. Does nothing when it is
    /// maximized or already big enough.
    pub fn grow_to_fit(window: &Window, w: f32, h: f32, edge: Option<Edge>) {
        let Some(hwnd) = hwnd_of(window) else { return };
        unsafe {
            if IsZoomed(hwnd).as_bool() || window.is_fullscreen() {
                return;
            }
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return;
            }
            let scale = scale_of(hwnd);
            let (fw, fh) = frame_of(hwnd);
            let Some(work) = work_area(hwnd) else { return };
            let cur = Rect { x: wr.left as f32, y: wr.top as f32, width: (wr.right - wr.left) as f32, height: (wr.bottom - wr.top) as f32 };
            let to = grown_rect(cur, (w * scale + fw).ceil(), (h * scale + fh).ceil(), edge, work);
            if to == cur {
                return;
            }
            let _ = SetWindowPos(hwnd, None, to.x as i32, to.y as i32, to.width as i32, to.height as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

#[cfg(windows)]
pub use win::{grow_to_fit, install, set_min_client};

#[cfg(not(windows))]
pub fn install(_: &gpui::Window) {}
#[cfg(not(windows))]
pub fn set_min_client(_: &gpui::Window, _: f32, _: f32) {}
#[cfg(not(windows))]
pub fn grow_to_fit(_: &gpui::Window, _: f32, _: f32, _: Option<Edge>) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(id: &str) -> SplitNode {
        SplitNode::Leaf { id: id.into(), sources: vec![], mod_mode: false }
    }
    fn row(kids: Vec<SplitNode>, sizes: Vec<f32>) -> SplitNode {
        SplitNode::Row { id: "r".into(), children: kids, sizes }
    }
    fn col(kids: Vec<SplitNode>, sizes: Vec<f32>) -> SplitNode {
        SplitNode::Col { id: "c".into(), children: kids, sizes }
    }
    fn wa() -> Rect {
        Rect { x: 0.0, y: 0.0, width: 1920.0, height: 1040.0 }
    }
    fn win(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    #[test]
    fn a_leaf_needs_the_chat_minimum() {
        assert_eq!(tree_min_size(&leaf("a")), (MIN_W, LEAF_MIN_H));
    }

    #[test]
    fn side_by_side_adds_widths_and_dividers() {
        let t = row(vec![leaf("a"), leaf("b"), leaf("c")], vec![1.0 / 3.0; 3]);
        let (w, h) = tree_min_size(&t);
        assert!((w - (3.0 * MIN_W + 2.0)).abs() < 0.01);
        assert_eq!(h, LEAF_MIN_H);
    }

    #[test]
    fn stacked_adds_heights_and_takes_the_widest() {
        let t = col(vec![leaf("a"), row(vec![leaf("b"), leaf("c")], vec![0.5, 0.5])], vec![0.5, 0.5]);
        let (w, h) = tree_min_size(&t);
        assert!((w - (2.0 * MIN_W + 1.0)).abs() < 0.01);
        assert!((h - (2.0 * LEAF_MIN_H + 1.0)).abs() < 0.01);
    }

    #[test]
    fn an_uneven_split_keeps_its_small_side_wide_enough() {
        let (w, _) = tree_min_size(&row(vec![leaf("a"), leaf("b")], vec![0.25, 0.75]));
        assert!(w >= MIN_W * 4.0);
    }

    #[test]
    fn already_big_enough_is_untouched() {
        let cur = win(100.0, 100.0, 1000.0, 700.0);
        assert_eq!(grown_rect(cur, 800.0, 400.0, Some(Edge::Right), wa()), cur);
    }

    #[test]
    fn right_grows_right_and_keeps_x() {
        assert_eq!(grown_rect(win(100.0, 100.0, 700.0, 700.0), 1000.0, 300.0, Some(Edge::Right), wa()), win(100.0, 100.0, 1000.0, 700.0));
    }

    #[test]
    fn left_grows_left() {
        assert_eq!(grown_rect(win(500.0, 100.0, 700.0, 700.0), 1000.0, 300.0, Some(Edge::Left), wa()), win(200.0, 100.0, 1000.0, 700.0));
    }

    #[test]
    fn bottom_grows_down_and_top_grows_up() {
        assert_eq!(grown_rect(win(0.0, 100.0, 700.0, 300.0), 100.0, 600.0, Some(Edge::Bottom), wa()), win(0.0, 100.0, 700.0, 600.0));
        assert_eq!(grown_rect(win(0.0, 400.0, 700.0, 300.0), 100.0, 600.0, Some(Edge::Top), wa()), win(0.0, 100.0, 700.0, 600.0));
    }

    #[test]
    fn no_edge_grows_right_and_down() {
        assert_eq!(grown_rect(win(10.0, 10.0, 400.0, 300.0), 600.0, 500.0, None, wa()), win(10.0, 10.0, 600.0, 500.0));
    }

    #[test]
    fn growth_is_pulled_back_onto_the_work_area() {
        // grows right past the edge: shifts left
        assert_eq!(grown_rect(win(1500.0, 0.0, 400.0, 300.0), 800.0, 300.0, Some(Edge::Right), wa()), win(1120.0, 0.0, 800.0, 300.0));
        // grows left past the edge: stops at it
        assert_eq!(grown_rect(win(100.0, 0.0, 400.0, 300.0), 800.0, 300.0, Some(Edge::Left), wa()), win(0.0, 0.0, 800.0, 300.0));
        // down past the bottom: shifts up
        assert_eq!(grown_rect(win(0.0, 900.0, 400.0, 100.0), 400.0, 500.0, Some(Edge::Bottom), wa()), win(0.0, 540.0, 400.0, 500.0));
    }

    #[test]
    fn what_cannot_fit_is_capped_at_the_work_area() {
        assert_eq!(grown_rect(win(300.0, 50.0, 700.0, 700.0), 5000.0, 5000.0, Some(Edge::Right), wa()), win(0.0, 0.0, 1920.0, 1040.0));
    }
}
