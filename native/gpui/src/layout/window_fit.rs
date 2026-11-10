//! Keeps every chat of the active tab at least `MIN_W` wide: the window grows when a split is added (toward the
//! side the split went) and its minimum size follows the layout, so it cannot be dragged narrower than the splits
//! need. gpui 0.2.2 fixes the minimum size at creation, so on Windows the main window is subclassed and
//! `WM_GETMINMAXINFO` is raised after gpui has answered it.
use std::collections::{HashMap, HashSet};

use agu_chat_core::protocol::SplitNode;

use crate::layout::split_view::{compute_layout, MIN_H as LEAF_MIN_H};
use crate::layout::tree::Edge;
use crate::window_state::{Rect, MIN_W};

/// the line between two splits
const DIVIDER: f32 = 1.0;

/// The smallest (width, height), in logical px, the content area needs so that no split is narrower than `MIN_W`
/// or shorter than a chat's minimum height. A side-by-side container needs its children's widths side by side plus
/// the dividers and the tallest child; a stacked one the transpose.
pub fn tree_min_size(n: &SplitNode) -> (f32, f32) {
    match n {
        SplitNode::Leaf { .. } => (MIN_W, LEAF_MIN_H),
        SplitNode::Row { children, .. } => {
            let kids: Vec<(f32, f32)> = children.iter().map(tree_min_size).collect();
            (kids.iter().map(|k| k.0).sum::<f32>() + DIVIDER * kids.len().saturating_sub(1) as f32, kids.iter().map(|k| k.1).fold(0.0, f32::max))
        }
        SplitNode::Col { children, .. } => {
            let kids: Vec<(f32, f32)> = children.iter().map(tree_min_size).collect();
            (kids.iter().map(|k| k.0).fold(0.0, f32::max), kids.iter().map(|k| k.1).sum::<f32>() + DIVIDER * kids.len().saturating_sub(1) as f32)
        }
    }
}

/// The structure of a tree (node kinds, ids, child order) without its sizes: it changes when a split is added,
/// removed or moved, not when a divider is dragged.
pub fn shape(n: &SplitNode) -> String {
    match n {
        SplitNode::Leaf { id, .. } => format!("l{id}"),
        SplitNode::Row { id, children, .. } => format!("r{id}({})", children.iter().map(shape).collect::<Vec<_>>().join(",")),
        SplitNode::Col { id, children, .. } => format!("c{id}({})", children.iter().map(shape).collect::<Vec<_>>().join(",")),
    }
}

/// Every leaf's size in px when the tree fills a `w` x `h` area.
pub fn leaf_px(root: &SplitNode, w: f32, h: f32) -> HashMap<String, (f32, f32)> {
    compute_layout(Some(root)).0.into_iter().map(|(l, r)| (l.id, (r.w * w, r.h * h))).collect()
}

/// (children, is a row) of a container; a leaf has none.
fn kids_of(n: &SplitNode) -> (&[SplitNode], bool) {
    match n {
        SplitNode::Row { children, .. } => (children, true),
        SplitNode::Col { children, .. } => (children, false),
        SplitNode::Leaf { .. } => (&[], true),
    }
}

fn with_sizes(n: &SplitNode, children: Vec<SplitNode>, sizes: Vec<f32>) -> SplitNode {
    match n {
        SplitNode::Row { id, .. } => SplitNode::Row { id: id.clone(), children, sizes },
        SplitNode::Col { id, .. } => SplitNode::Col { id: id.clone(), children, sizes },
        SplitNode::Leaf { .. } => n.clone(),
    }
}

/// The tree after its structure changed, with sizes that keep every chat that existed at its old pixel size
/// (`old_px`, by leaf id) and give new ones the average of their siblings: the new tree and the content size
/// (w, h) in px it asks for. Every chat is at least the minimum size. A tree with no known chat is returned as
/// it is, with its minimum size.
pub fn replan(new: &SplitNode, old_px: &HashMap<String, (f32, f32)>) -> (SplitNode, f32, f32) {
    /// (node with fixed sizes, its px size when any leaf in it is known)
    fn go(n: &SplitNode, old: &HashMap<String, (f32, f32)>) -> (SplitNode, Option<(f32, f32)>) {
        if let SplitNode::Leaf { id, .. } = n {
            return (n.clone(), old.get(id).map(|(w, h)| (w.max(MIN_W), h.max(LEAF_MIN_H))));
        }
        let (children, row) = kids_of(n);
        let kids: Vec<(SplitNode, Option<(f32, f32)>)> = children.iter().map(|c| go(c, old)).collect();
        let known: Vec<(f32, f32)> = kids.iter().filter_map(|k| k.1).collect();
        if known.is_empty() {
            return (n.clone(), None);
        }
        let main = |s: (f32, f32)| if row { s.0 } else { s.1 };
        let cross = |s: (f32, f32)| if row { s.1 } else { s.0 };
        let mean = known.iter().map(|s| main(*s)).sum::<f32>() / known.len() as f32;
        let wide = known.iter().map(|s| cross(*s)).fold(0.0, f32::max);
        let mut nodes = Vec::new();
        let mut sizes = Vec::new();
        for (node, size) in kids {
            let s = size.unwrap_or_else(|| {
                let min = tree_min_size(&node);
                let (m, x) = (mean.max(if row { min.0 } else { min.1 }), wide.max(if row { min.1 } else { min.0 }));
                if row {
                    (m, x)
                } else {
                    (x, m)
                }
            });
            nodes.push(node);
            sizes.push(s);
        }
        let total: f32 = sizes.iter().map(|s| main(*s)).sum();
        let fr: Vec<f32> = sizes.iter().map(|s| main(*s) / total.max(f32::EPSILON)).collect();
        let size = if row { (total, sizes.iter().map(|s| s.1).fold(0.0, f32::max)) } else { (sizes.iter().map(|s| s.0).fold(0.0, f32::max), total) };
        (with_sizes(n, nodes, fr), Some(size))
    }
    match go(new, old_px) {
        (t, Some((w, h))) => (t, w, h),
        (t, None) => {
            let (w, h) = tree_min_size(&t);
            (t, w, h)
        }
    }
}

/// `n` with every split brought up to its minimum size inside a `w` x `h` px content area: a split below its
/// minimum is raised to it and the space comes from its siblings with room to spare, in proportion to it. None
/// when nothing had to change.
pub fn enforce_min(n: &SplitNode, w: f32, h: f32) -> Option<SplitNode> {
    fn go(n: &SplitNode, w: f32, h: f32, changed: &mut bool) -> SplitNode {
        let (children, row) = kids_of(n);
        let (SplitNode::Row { sizes, .. } | SplitNode::Col { sizes, .. }) = n else { return n.clone() };
        let p = if row { w } else { h };
        if children.is_empty() || p <= 0.0 || !p.is_finite() {
            return n.clone();
        }
        let m: Vec<f32> = children.iter().map(|c| if row { tree_min_size(c).0 } else { tree_min_size(c).1 }).collect();
        let f: Vec<f32> = (0..children.len()).map(|i| sizes.get(i).copied().unwrap_or(1.0 / children.len() as f32)).collect();
        let cur: Vec<f32> = f.iter().map(|f| f * p).collect();
        let mut next = cur.clone();
        if m.iter().sum::<f32>() >= p {
            next = m.clone();
        } else if cur.iter().zip(&m).any(|(c, m)| *c < m - 0.5) {
            let deficit: f32 = cur.iter().zip(&m).filter(|(c, m)| **c < **m - 0.5).map(|(c, m)| m - c).sum();
            let slack: f32 = cur.iter().zip(&m).map(|(c, m)| (c - m).max(0.0)).sum();
            for i in 0..next.len() {
                if cur[i] < m[i] - 0.5 {
                    next[i] = m[i];
                } else if slack > 0.0 {
                    next[i] = cur[i] - deficit * (cur[i] - m[i]).max(0.0) / slack;
                }
            }
        }
        let total: f32 = next.iter().sum();
        let fr: Vec<f32> = if next == cur { f.clone() } else { next.iter().map(|v| v / total.max(f32::EPSILON)).collect() };
        if fr.iter().zip(&f).any(|(a, b)| (a - b).abs() > 1e-4) {
            *changed = true;
        }
        let kids = children.iter().zip(&fr).map(|(c, f)| if row { go(c, f * p, h, changed) } else { go(c, w, f * p, changed) }).collect();
        with_sizes(n, kids, fr)
    }
    let mut changed = false;
    let t = go(n, w, h, &mut changed);
    changed.then_some(t)
}

/// (from_left, from_top): whether the chats `new` no longer has sat at the left / top edge of `old` (and did not
/// span the whole width / height), i.e. which side the window should give the space back on.
pub fn removed_side(old: &SplitNode, new: &SplitNode) -> (bool, bool) {
    let kept: HashSet<String> = compute_layout(Some(new)).0.into_iter().map(|(l, _)| l.id).collect();
    let (mut left, mut top) = (false, false);
    for (l, r) in compute_layout(Some(old)).0 {
        if kept.contains(&l.id) {
            continue;
        }
        left |= r.x <= 0.001 && r.w < 0.999;
        top |= r.y <= 0.001 && r.h < 0.999;
    }
    (left, top)
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

/// The window outer rect for an exact size of `w` x `h` (it grows or shrinks, capped at the work area): the change
/// is taken at the left / top edge when `from_left` / `from_top`, else at the right / bottom, and the rect is
/// pulled back inside `work` on an axis whose size changed.
pub fn resized_rect(cur: Rect, w: f32, h: f32, from_left: bool, from_top: bool, work: Rect) -> Rect {
    let width = w.min(work.width);
    let height = h.min(work.height);
    let mut x = cur.x;
    let mut y = cur.y;
    if width != cur.width {
        if from_left {
            x += cur.width - width;
        }
        x = x.min(work.x + work.width - width).max(work.x);
    }
    if height != cur.height {
        if from_top {
            y += cur.height - height;
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

    /// Where the window has to go so its client area is at least `w` x `h` logical px, grown toward `edge`: the
    /// HWND and the new outer rect, or None when it is maximized or already big enough. Apply it with
    /// `apply_grow` OUTSIDE any gpui update of this window: SetWindowPos delivers WM_SIZE at once, and gpui
    /// drops a size change for a window that is being updated (the grown part then stays blank).
    pub fn plan_grow(window: &Window, w: f32, h: f32, edge: Option<Edge>) -> Option<(isize, Rect)> {
        let hwnd = hwnd_of(window)?;
        unsafe {
            if IsZoomed(hwnd).as_bool() || window.is_fullscreen() {
                return None;
            }
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return None;
            }
            let scale = scale_of(hwnd);
            let (fw, fh) = frame_of(hwnd);
            let work = work_area(hwnd)?;
            let cur = Rect { x: wr.left as f32, y: wr.top as f32, width: (wr.right - wr.left) as f32, height: (wr.bottom - wr.top) as f32 };
            let to = grown_rect(cur, (w * scale + fw).ceil(), (h * scale + fh).ceil(), edge, work);
            (to != cur).then_some((hwnd.0 as isize, to))
        }
    }

    /// Where the window has to go so its client area is exactly `w` x `h` logical px (it grows or shrinks, the
    /// change taken at the left / top edge when asked): None when maximized, fullscreen or already that size.
    /// Apply it with `apply_grow` outside any gpui update, like `plan_grow`.
    pub fn plan_resize(window: &Window, w: f32, h: f32, from_left: bool, from_top: bool) -> Option<(isize, Rect)> {
        let hwnd = hwnd_of(window)?;
        unsafe {
            if IsZoomed(hwnd).as_bool() || window.is_fullscreen() {
                return None;
            }
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return None;
            }
            let scale = scale_of(hwnd);
            let (fw, fh) = frame_of(hwnd);
            let work = work_area(hwnd)?;
            let cur = Rect { x: wr.left as f32, y: wr.top as f32, width: (wr.right - wr.left) as f32, height: (wr.bottom - wr.top) as f32 };
            let to = resized_rect(cur, (w * scale + fw).ceil(), (h * scale + fh).ceil(), from_left, from_top, work);
            (to != cur).then_some((hwnd.0 as isize, to))
        }
    }

    /// The biggest client area (logical px) this window can have on its screen: the work area less the frame.
    pub fn max_client(window: &Window) -> Option<(f32, f32)> {
        let hwnd = hwnd_of(window)?;
        let wa = work_area(hwnd)?;
        let scale = scale_of(hwnd);
        let (fw, fh) = frame_of(hwnd);
        Some(((wa.width - fw).max(0.0) / scale, (wa.height - fh).max(0.0) / scale))
    }

    pub fn apply_grow(hwnd: isize, to: Rect) {
        unsafe {
            let _ = SetWindowPos(HWND(hwnd as *mut core::ffi::c_void), None, to.x as i32, to.y as i32, to.width as i32, to.height as i32, SWP_NOZORDER | SWP_NOACTIVATE);
        }
    }
}

#[cfg(windows)]
pub use win::{apply_grow, install, max_client, plan_grow, plan_resize, set_min_client};

#[cfg(not(windows))]
pub fn install(_: &gpui::Window) {}
#[cfg(not(windows))]
pub fn set_min_client(_: &gpui::Window, _: f32, _: f32) {}
#[cfg(not(windows))]
pub fn plan_grow(_: &gpui::Window, _: f32, _: f32, _: Option<Edge>) -> Option<(isize, Rect)> {
    None
}
#[cfg(not(windows))]
pub fn plan_resize(_: &gpui::Window, _: f32, _: f32, _: bool, _: bool) -> Option<(isize, Rect)> {
    None
}
#[cfg(not(windows))]
pub fn apply_grow(_: isize, _: Rect) {}
#[cfg(not(windows))]
pub fn max_client(_: &gpui::Window) -> Option<(f32, f32)> {
    None
}

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
    fn an_uneven_split_needs_the_same_as_an_even_one() {
        let (a, _) = tree_min_size(&row(vec![leaf("a"), leaf("b")], vec![0.25, 0.75]));
        let (b, _) = tree_min_size(&row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5]));
        assert_eq!(a, b);
        assert!((a - (2.0 * MIN_W + 1.0)).abs() < 0.01);
    }

    fn rowid(id: &str, kids: Vec<SplitNode>, sizes: Vec<f32>) -> SplitNode {
        SplitNode::Row { id: id.into(), children: kids, sizes }
    }
    fn px(wh: &[(&str, f32, f32)]) -> HashMap<String, (f32, f32)> {
        wh.iter().map(|(i, w, h)| (i.to_string(), (*w, *h))).collect()
    }
    fn widths(t: &SplitNode, w: f32, h: f32) -> HashMap<String, f32> {
        leaf_px(t, w, h).into_iter().map(|(k, v)| (k, v.0)).collect()
    }

    #[test]
    fn shape_ignores_sizes() {
        assert_eq!(shape(&row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5])), shape(&row(vec![leaf("a"), leaf("b")], vec![0.2, 0.8])));
        assert_ne!(shape(&row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5])), shape(&row(vec![leaf("b"), leaf("a")], vec![0.5, 0.5])));
    }

    #[test]
    fn replan_keeps_old_chats_and_sizes_the_new_one_like_its_neighbours() {
        let old = px(&[("a", 400.0, 500.0), ("b", 400.0, 500.0)]);
        let flat = rowid("r", vec![leaf("a"), leaf("n"), leaf("b")], vec![1.0 / 3.0; 3]);
        let (t, w, h) = replan(&flat, &old);
        assert!((w - 1200.0).abs() < 0.01 && (h - 500.0).abs() < 0.01);
        for (_, v) in widths(&t, w, h) {
            assert!((v - 400.0).abs() < 0.01);
        }
        let nested = rowid("r", vec![rowid("i", vec![leaf("a"), leaf("n")], vec![0.5, 0.5]), leaf("b")], vec![0.5, 0.5]);
        let (t, w, h) = replan(&nested, &old);
        assert!((w - 1200.0).abs() < 0.01);
        for (_, v) in widths(&t, w, h) {
            assert!((v - 400.0).abs() < 0.01);
        }
    }

    #[test]
    fn replan_after_a_removal_gives_the_space_back() {
        let old = px(&[("a", 400.0, 500.0), ("n", 400.0, 500.0), ("b", 400.0, 500.0)]);
        let two = rowid("r", vec![leaf("a"), leaf("b")], vec![0.25, 0.75]);
        let (t, w, _) = replan(&two, &old);
        assert!((w - 800.0).abs() < 0.01);
        assert!(matches!(&t, SplitNode::Row { sizes, .. } if (sizes[0] - 0.5).abs() < 1e-4));
    }

    #[test]
    fn replan_stacks_a_new_chat_in_a_column() {
        let old = px(&[("a", 400.0, 300.0)]);
        let t = col(vec![leaf("a"), leaf("n")], vec![0.5, 0.5]);
        let (_, w, h) = replan(&t, &old);
        assert!((w - 400.0).abs() < 0.01 && (h - 600.0).abs() < 0.01);
    }

    #[test]
    fn replan_of_an_unknown_tree_asks_for_its_minimum() {
        let t = row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5]);
        let (n, w, h) = replan(&t, &HashMap::new());
        assert_eq!(n, t);
        assert_eq!((w, h), tree_min_size(&t));
    }

    #[test]
    fn enforce_min_raises_a_narrow_chat() {
        let t = row(vec![leaf("a"), leaf("b")], vec![0.25, 0.75]);
        let fixed = enforce_min(&t, 700.0, 500.0).expect("a is 175px wide");
        let w = widths(&fixed, 700.0, 500.0);
        assert!(w["a"] >= MIN_W - 0.5 && w["b"] >= MIN_W - 0.5);
        assert!(enforce_min(&fixed, 700.0, 500.0).is_none());
    }

    #[test]
    fn enforce_min_returns_none_when_nothing_changes() {
        assert!(enforce_min(&row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5]), 900.0, 500.0).is_none());
        assert!(enforce_min(&leaf("a"), 100.0, 10.0).is_none());
    }

    #[test]
    fn removed_side_finds_where_the_chat_left_from() {
        let old = row(vec![leaf("a"), leaf("b")], vec![0.5, 0.5]);
        assert_eq!(removed_side(&old, &leaf("b")), (true, false));
        assert_eq!(removed_side(&old, &leaf("a")), (false, false));
        assert_eq!(removed_side(&old, &old), (false, false));
        let stacked = col(vec![leaf("a"), leaf("b")], vec![0.5, 0.5]);
        assert_eq!(removed_side(&stacked, &leaf("b")), (false, true));
    }

    #[test]
    fn a_resize_shrinks_from_the_right_or_the_left() {
        assert_eq!(resized_rect(win(100.0, 50.0, 1200.0, 700.0), 800.0, 700.0, false, false, wa()), win(100.0, 50.0, 800.0, 700.0));
        assert_eq!(resized_rect(win(100.0, 50.0, 1200.0, 700.0), 800.0, 700.0, true, false, wa()), win(500.0, 50.0, 800.0, 700.0));
        assert_eq!(resized_rect(win(100.0, 300.0, 700.0, 600.0), 700.0, 400.0, false, true, wa()), win(100.0, 500.0, 700.0, 400.0));
    }

    #[test]
    fn a_resize_grows_from_the_left_and_is_capped() {
        assert_eq!(resized_rect(win(500.0, 50.0, 700.0, 700.0), 1000.0, 700.0, true, false, wa()), win(200.0, 50.0, 1000.0, 700.0));
        assert_eq!(resized_rect(win(300.0, 50.0, 700.0, 700.0), 5000.0, 5000.0, false, false, wa()), win(0.0, 0.0, 1920.0, 1040.0));
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
