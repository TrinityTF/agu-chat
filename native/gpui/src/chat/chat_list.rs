//! The message list's scroll container (what `MessageList.tsx` got from `overflow-y: auto`). `gpui::list()` rounds
//! every item height to whole pixels (taffy's rounding), which makes a column of 20.84 px rows drift away from the
//! DOM's fractional layout, so this is a small purpose-built list: items have *fractional* heights and margins
//! (with the DOM's adjacent-margin collapsing), are laid out from the bottom (stick-to-bottom) or from an anchor
//! item (when the user scrolled up, so rows arriving below or trimmed above do not move what is being read), and are
//! prepainted at fractional origins. Only the items in view are rendered; heights come from the pane's model.
//!
//! Content shorter than the viewport starts at the top, like a scroll container's content does.
use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    px, relative, size, AnyElement, App, AvailableSpace, Bounds, ContentMask, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollWheelEvent, Style, Window,
};

use super::PxF;

/// The padding of the DOM list's inner `py-1`.
pub const PAD: f32 = 4.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ItemMetrics {
    pub h: f32,
    /// vertical margins that collapse with the neighbours' (`my-px` on event frames)
    pub mt: f32,
    pub mb: f32,
}

struct Inner {
    items: Vec<Option<ItemMetrics>>,
    /// None = following the bottom; Some((item, off)) = item's top edge is `off` px above the viewport top
    anchor: Option<(usize, f32)>,
    vp: (f32, f32),
    dist_to_bottom: f32,
    placed: Vec<(usize, f32, ItemMetrics)>,
    thumb: Option<(f32, f32)>,
    /// estimated content height (scrollbar), and the list's top edge in window coordinates
    total: f32,
    origin_y: f32,
}

#[derive(Clone)]
pub struct ChatListState(Rc<RefCell<Inner>>);

impl ChatListState {
    pub fn new() -> Self {
        ChatListState(Rc::new(RefCell::new(Inner { items: Vec::new(), anchor: None, vp: (0.0, 0.0), dist_to_bottom: 0.0, placed: Vec::new(), thumb: None, total: 0.0, origin_y: 0.0 })))
    }

    pub fn item_count(&self) -> usize {
        self.0.borrow().items.len()
    }

    /// The rows were rebuilt in any shape (rows inserted in the middle, removed, trimmed at the top, appended):
    /// `map[i]` is old item i's new index and whether its height still holds. A scrolled-up view keeps the row it
    /// was on, so a busy chat never moves (or un-pauses) what is being read.
    pub fn remap(&self, new_len: usize, map: &[Option<(usize, bool)>]) {
        let mut s = self.0.borrow_mut();
        let mut items: Vec<Option<ItemMetrics>> = vec![None; new_len];
        for (old, m) in map.iter().enumerate() {
            if let Some((j, true)) = m {
                if let (Some(slot), Some(Some(h))) = (items.get_mut(*j), s.items.get(old)) {
                    *slot = Some(*h);
                }
            }
        }
        s.items = items;
        let idx: Vec<Option<usize>> = map.iter().map(|m| m.map(|(j, _)| j)).collect();
        s.anchor = remap_anchor(s.anchor, &idx, new_len);
    }

    /// New content entirely: forget everything, follow the bottom.
    pub fn reset(&self, count: usize) {
        let mut s = self.0.borrow_mut();
        s.items = vec![None; count];
        s.anchor = None;
    }

    /// Every height is stale (settings, width): re-measure, keep the position.
    pub fn invalidate_all(&self) {
        for m in self.0.borrow_mut().items.iter_mut() {
            *m = None;
        }
    }

    pub fn invalidate(&self, ix: usize) {
        if let Some(m) = self.0.borrow_mut().items.get_mut(ix) {
            *m = None;
        }
    }

    pub fn following(&self) -> bool {
        self.0.borrow().anchor.is_none()
    }

    pub fn follow(&self) {
        self.0.borrow_mut().anchor = None;
    }

    /// Px between the end of the content and the bottom of the viewport (0 when following).
    pub fn dist_to_bottom(&self) -> f32 {
        self.0.borrow().dist_to_bottom
    }

    pub fn viewport(&self) -> (f32, f32) {
        self.0.borrow().vp
    }

    /// (thumb top, thumb height) in px for the scrollbar, when the content overflows.
    pub fn thumb(&self) -> Option<(f32, f32)> {
        self.0.borrow().thumb
    }

    /// Top and height of an item in the last layout (px below the list's top), if it was in view.
    pub fn bounds_for_item(&self, ix: usize) -> Option<(f32, f32)> {
        self.0.borrow().placed.iter().find(|(i, _, _)| *i == ix).map(|(_, y, m)| (*y, m.h))
    }

    /// Put item `ix`'s top `from_top` px below the viewport top (a jump to a message). Content that ends above
    /// the bottom snaps back to following at the next layout.
    pub fn scroll_to_item(&self, ix: usize, from_top: f32) {
        let mut s = self.0.borrow_mut();
        if ix >= s.items.len() {
            return;
        }
        s.anchor = Some((ix, -from_top));
        // not at the bottom until the next layout measures it (the pane would follow again at once)
        s.dist_to_bottom = f32::MAX;
    }

    /// Scroll by `d` px (positive = toward the newest messages).
    pub fn scroll_by(&self, d: f32) {
        let mut s = self.0.borrow_mut();
        if d == 0.0 || s.items.is_empty() {
            return;
        }
        let (mut ix, mut off) = match s.anchor {
            Some(a) => a,
            None => {
                if d > 0.0 {
                    return;
                }
                match s.placed.first() {
                    Some((i, y, _)) => (*i, -*y),
                    None => return,
                }
            }
        };
        off += d;
        let n = s.items.len();
        let metric = |s: &Inner, i: usize| s.items.get(i).copied().flatten().unwrap_or_default();
        while off > 0.0 && ix + 1 < n {
            let m = metric(&s, ix);
            let next = metric(&s, ix + 1);
            let step = m.h + m.mb.max(next.mt);
            if m.h == 0.0 || off < step {
                break;
            }
            off -= step;
            ix += 1;
        }
        while off < 0.0 && ix > 0 {
            let prev = metric(&s, ix - 1);
            let cur = metric(&s, ix);
            let step = prev.h + prev.mb.max(cur.mt);
            if prev.h == 0.0 {
                break;
            }
            off += step;
            ix -= 1;
        }
        if ix == 0 {
            let top0 = PAD + metric(&s, 0).mt;
            if off < -top0 {
                off = -top0;
            }
        }
        s.anchor = Some((ix, off));
        // The pane decides "back at the bottom, follow again" from dist_to_bottom BEFORE the next prepaint
        // re-measures it; left stale (0 while following) it snapped every wheel-up straight back down.
        if s.dist_to_bottom != f32::MAX {
            s.dist_to_bottom = (s.dist_to_bottom - d).max(0.0);
        }
    }

    /// The scrollbar's window-space track: (top, height), plus the thumb (top, height) inside it.
    pub fn track(&self) -> (f32, f32) {
        let s = self.0.borrow();
        (s.origin_y, s.vp.1)
    }

    /// Put the scrollbar thumb's top at `thumb_top` px (inside the track): the scrollbar drag.
    pub fn scroll_to_thumb(&self, thumb_top: f32) {
        let (vh, total, thumb_h) = {
            let s = self.0.borrow();
            match s.thumb {
                Some((_, h)) => (s.vp.1, s.total, h),
                None => return,
            }
        };
        let room = (vh - thumb_h).max(1.0);
        let top = (thumb_top.clamp(0.0, room) / room) * (total - vh).max(0.0);
        self.scroll_to_top(top, total - vh);
    }

    /// Scroll so the content `top` px down (estimated heights for unmeasured rows) is at the viewport top.
    fn scroll_to_top(&self, top: f32, max_top: f32) {
        let mut s = self.0.borrow_mut();
        let n = s.items.len();
        if n == 0 {
            return;
        }
        if top >= max_top - 0.5 {
            s.anchor = None;
            s.dist_to_bottom = 0.0;
            return;
        }
        let known: Vec<f32> = s.items.iter().filter_map(|m| m.map(|m| m.h + m.mb.max(m.mt))).collect();
        let avg = if known.is_empty() { 20.0 } else { known.iter().sum::<f32>() / known.len() as f32 };
        let mut y = PAD;
        let mut anchor = (n - 1, 0.0);
        for (i, m) in s.items.iter().enumerate() {
            let step = m.map(|m| m.h + m.mb.max(m.mt)).unwrap_or(avg);
            if y + step > top {
                anchor = (i, top - y);
                break;
            }
            y += step;
        }
        s.anchor = Some(anchor);
        s.dist_to_bottom = (max_top - top).max(0.0);
    }
}

/// Where a scrolled-up view goes when its rows were rebuilt (`map[i]` = old row i's new index, None = gone).
/// Following stays following. The row it was on keeps its place; if that row is gone (trimmed off the top, removed),
/// the view moves to the next row that survived (the oldest kept one after a trim), else the one before it. Only when
/// nothing of the old view is left does it go back to the bottom.
pub fn remap_anchor(anchor: Option<(usize, f32)>, map: &[Option<usize>], new_len: usize) -> Option<(usize, f32)> {
    let (ix, off) = anchor?;
    if new_len == 0 {
        return None;
    }
    let clamp = |j: usize| j.min(new_len - 1);
    if let Some(Some(j)) = map.get(ix) {
        return Some((clamp(*j), off));
    }
    if let Some(j) = map.iter().skip(ix + 1).find_map(|m| *m) {
        return Some((clamp(j), 0.0));
    }
    map.iter().take(ix.min(map.len())).rev().find_map(|m| *m).map(|j| (clamp(j), 0.0))
}

impl Default for ChatListState {
    fn default() -> Self {
        Self::new()
    }
}

pub type MeasureFn = Rc<dyn Fn(usize, f32, &mut Window, &mut App) -> ItemMetrics>;
pub type RenderFn = Rc<dyn Fn(usize, &mut Window, &mut App) -> AnyElement>;

pub struct ChatList {
    state: ChatListState,
    measure: MeasureFn,
    render: RenderFn,
}

pub fn chat_list(state: ChatListState, measure: MeasureFn, render: RenderFn) -> ChatList {
    ChatList { state, measure, render }
}

impl IntoElement for ChatList {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

pub struct ListPrepaint {
    hitbox: Hitbox,
    items: Vec<AnyElement>,
    bounds: Bounds<Pixels>,
}

impl ChatList {
    fn metrics(&self, ix: usize, w: f32, window: &mut Window, cx: &mut App) -> ItemMetrics {
        if let Some(Some(m)) = self.state.0.borrow().items.get(ix) {
            return *m;
        }
        let m = (self.measure)(ix, w, window, cx);
        if let Some(slot) = self.state.0.borrow_mut().items.get_mut(ix) {
            *slot = Some(m);
        }
        m
    }
}

impl Element for ChatList {
    type RequestLayoutState = ();
    type PrepaintState = ListPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, cx: &mut App) -> ListPrepaint {
        let w = bounds.size.width.f();
        let vh = bounds.size.height.f();
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        {
            let mut s = self.state.0.borrow_mut();
            // a new width changes every wrap: all heights are stale
            if (s.vp.0 - w).abs() > 0.01 {
                for m in s.items.iter_mut() {
                    *m = None;
                }
            }
            s.vp = (w, vh);
            s.origin_y = bounds.origin.y.f();
        }
        let n = self.state.item_count();
        let mut placed: Vec<(usize, f32, ItemMetrics)> = Vec::new();
        let mut dist = 0.0f32;
        let mut thumb = None;
        if n > 0 {
            let mut follow = self.state.following();
            let mut guard = 0;
            loop {
                guard += 1;
                placed.clear();
                if follow {
                    let last = self.metrics(n - 1, w, window, cx);
                    let mut y = vh - PAD - last.mb - last.h;
                    placed.push((n - 1, y, last));
                    let mut ix = n - 1;
                    let mut below = last;
                    while y > 0.0 && ix > 0 {
                        ix -= 1;
                        let m = self.metrics(ix, w, window, cx);
                        y = y - m.mb.max(below.mt) - m.h;
                        placed.push((ix, y, m));
                        below = m;
                    }
                    placed.reverse();
                    // content shorter than the viewport: it starts at the top
                    if ix == 0 {
                        let top0 = PAD + placed[0].2.mt;
                        if placed[0].1 > top0 {
                            let shift = top0 - placed[0].1;
                            for p in placed.iter_mut() {
                                p.1 += shift;
                            }
                        }
                    }
                    dist = 0.0;
                } else {
                    let (aix, off) = self.state.0.borrow().anchor.unwrap();
                    let aix = aix.min(n - 1);
                    let am = self.metrics(aix, w, window, cx);
                    let mut y = -off;
                    placed.push((aix, y, am));
                    // up: fill the space above while the anchor sits below the top
                    let mut ix = aix;
                    let mut cur = am;
                    let mut ytop = y;
                    while ytop > 0.0 && ix > 0 {
                        ix -= 1;
                        let m = self.metrics(ix, w, window, cx);
                        ytop = ytop - m.mb.max(cur.mt) - m.h;
                        placed.insert(0, (ix, ytop, m));
                        cur = m;
                    }
                    // down
                    let mut ix = aix;
                    let mut prev = am;
                    let mut reached_end = true;
                    while y + prev.h + prev.mb < vh {
                        if ix + 1 >= n {
                            break;
                        }
                        ix += 1;
                        let m = self.metrics(ix, w, window, cx);
                        y = y + prev.h + prev.mb.max(m.mt);
                        placed.push((ix, y, m));
                        prev = m;
                    }
                    if ix + 1 < n {
                        reached_end = false;
                    }
                    let (ly, lm) = placed.last().map(|(_, y, m)| (*y, *m)).unwrap();
                    if reached_end {
                        let end = ly + lm.h + lm.mb + PAD;
                        dist = (end - vh).max(0.0);
                        if end <= vh + 0.5 && guard < 3 {
                            // scrolled past the end: back to following
                            self.state.0.borrow_mut().anchor = None;
                            follow = true;
                            continue;
                        }
                    } else {
                        dist = f32::MAX;
                    }
                    // keep the anchor on the first item in view
                    if let Some((fi, fy, _)) = placed.iter().find(|(_, y, m)| y + m.h > 0.0).copied() {
                        self.state.0.borrow_mut().anchor = Some((fi, -fy));
                    }
                }
                break;
            }
            // scrollbar: estimates from the heights known so far
            let mut s = self.state.0.borrow_mut();
            let known: Vec<f32> = s.items.iter().filter_map(|m| m.map(|m| m.h + m.mb.max(m.mt))).collect();
            let avg = if known.is_empty() { 20.0 } else { known.iter().sum::<f32>() / known.len() as f32 };
            let total: f32 = PAD * 2.0 + s.items.iter().map(|m| m.map(|m| m.h + m.mb.max(m.mt)).unwrap_or(avg)).sum::<f32>();
            s.total = total;
            if total > vh + 0.5 {
                let first = placed.first().map(|(i, y, _)| (*i, *y)).unwrap_or((0, 0.0));
                let before: f32 = PAD + s.items[..first.0].iter().map(|m| m.map(|m| m.h + m.mb.max(m.mt)).unwrap_or(avg)).sum::<f32>();
                let top = (before - first.1).clamp(0.0, total - vh);
                let thumb_h = (vh * vh / total).clamp(20.0, vh);
                thumb = Some((top / (total - vh) * (vh - thumb_h), thumb_h));
            }
        }
        // lay out and position the items in view
        let mut items = Vec::with_capacity(placed.len());
        for (ix, y, _) in &placed {
            let mut el = (self.render)(*ix, window, cx);
            let _ = el.layout_as_root(size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MinContent), window, cx);
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                el.prepaint_at(bounds.origin + gpui::point(px(0.), px(*y)), window, cx);
            });
            items.push(el);
        }
        {
            let mut s = self.state.0.borrow_mut();
            s.placed = placed;
            s.dist_to_bottom = dist;
            s.thumb = thumb;
        }
        ListPrepaint { hitbox, items, bounds }
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), pp: &mut ListPrepaint, window: &mut Window, cx: &mut App) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for el in pp.items.iter_mut() {
                el.paint(window, cx);
            }
        });
        let state = self.state.clone();
        let hb = pp.hitbox.clone();
        let mut acc = gpui::ScrollDelta::default();
        window.on_mouse_event(move |ev: &ScrollWheelEvent, phase, window, _cx| {
            if phase == DispatchPhase::Bubble && hb.should_handle_scroll(window) {
                acc = acc.coalesce(ev.delta);
                // Chromium scrolls 100 px per notch at the default 3 lines (100 / 3 px a line); 20 px a line felt slow
                let d = acc.pixel_delta(px(100. / 3.));
                acc = gpui::ScrollDelta::default();
                state.scroll_by(-d.y.f());
                window.refresh();
            }
        });
        let _ = pp.bounds;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(h: f32) -> Option<ItemMetrics> {
        Some(ItemMetrics { h, mt: 0.0, mb: 0.0 })
    }

    fn list(n: usize) -> ChatListState {
        let s = ChatListState::new();
        {
            let mut i = s.0.borrow_mut();
            i.items = vec![m(20.0); n];
            i.vp = (300.0, 200.0);
            i.dist_to_bottom = f32::MAX;
        }
        s
    }

    fn anchor(s: &ChatListState) -> Option<(usize, f32)> {
        s.0.borrow().anchor
    }

    /// old row i -> new index, as `apply_rows` builds it (true: same row, height kept)
    fn shift(n_old: usize, f: impl Fn(usize) -> Option<usize>) -> Vec<Option<(usize, bool)>> {
        (0..n_old).map(|i| f(i).map(|j| (j, true))).collect()
    }

    #[test]
    fn following_stays_following() {
        assert_eq!(remap_anchor(None, &[Some(0), Some(1)], 5), None);
        let s = list(10);
        s.remap(15, &shift(10, Some));
        assert!(s.following());
    }

    #[test]
    fn appended_rows_do_not_move_a_scrolled_up_view() {
        let s = list(100);
        s.0.borrow_mut().anchor = Some((50, 0.0));
        s.scroll_by(-35.0);
        let a = anchor(&s).unwrap();
        assert_eq!(a, (48, 5.0));
        // a flood of new rows below, many times over
        for k in 0..50 {
            let n = 100 + k * 30;
            s.remap(n + 30, &shift(n, Some));
        }
        assert_eq!(anchor(&s), Some(a));
        assert!(!s.following());
    }

    #[test]
    fn a_row_put_in_the_middle_keeps_the_view_on_its_row() {
        // another chat's late line lands above the row being read: the view keeps that row, offset and all
        let s = list(100);
        s.0.borrow_mut().anchor = Some((60, 7.0));
        s.remap(101, &shift(100, |i| Some(if i < 40 { i } else { i + 1 })));
        assert_eq!(anchor(&s), Some((61, 7.0)));
        // below it: nothing moves
        s.remap(102, &shift(101, |i| Some(if i < 80 { i } else { i + 1 })));
        assert_eq!(anchor(&s), Some((61, 7.0)));
        assert!(!s.following());
    }

    #[test]
    fn trimming_shifts_the_anchor_and_keeps_heights() {
        let s = list(100);
        s.0.borrow_mut().anchor = Some((60, 3.0));
        s.0.borrow_mut().items[70] = m(55.0);
        // 10 trimmed at the top, 10 appended
        s.remap(100, &shift(100, |i| i.checked_sub(10)));
        assert_eq!(anchor(&s), Some((50, 3.0)));
        assert_eq!(s.0.borrow().items[60], m(55.0));
        assert_eq!(s.0.borrow().items[95], None);
    }

    #[test]
    fn the_row_being_read_trimmed_away_stays_at_the_oldest_kept_row() {
        let s = list(100);
        s.0.borrow_mut().anchor = Some((5, 9.0));
        s.remap(100, &shift(100, |i| i.checked_sub(20)));
        assert_eq!(anchor(&s), Some((0, 0.0)));
        assert!(!s.following());
    }

    #[test]
    fn a_removed_row_moves_the_view_to_its_neighbour() {
        assert_eq!(remap_anchor(Some((3, 4.0)), &[Some(0), Some(1), Some(2), None, Some(3)], 4), Some((3, 0.0)));
        // the last rows went: the one before
        assert_eq!(remap_anchor(Some((3, 4.0)), &[Some(0), Some(1), None, None], 2), Some((1, 0.0)));
        // nothing of the old view is left (a new search): back to the bottom
        assert_eq!(remap_anchor(Some((1, 4.0)), &[None, None], 3), None);
        assert_eq!(remap_anchor(Some((1, 4.0)), &[Some(0), Some(1)], 0), None);
    }

    #[test]
    fn a_changed_row_keeps_the_offset_but_is_measured_again() {
        let s = list(10);
        s.0.borrow_mut().anchor = Some((4, 6.0));
        let mut map = shift(10, Some);
        map[4] = Some((4, false));
        s.remap(10, &map);
        assert_eq!(anchor(&s), Some((4, 6.0)));
        assert_eq!(s.0.borrow().items[4], None);
        assert_eq!(s.0.borrow().items[3], m(20.0));
    }
}
