//! The message list's scroll container (what `MessageList.tsx` got from `overflow-y: auto`). `gpui::list()` rounds
//! every item height to whole pixels (taffy's rounding), which makes a column of 20.84 px rows drift away from the
//! DOM's fractional layout, so this is a small purpose-built list: items have *fractional* heights and margins
//! (with the DOM's adjacent-margin collapsing), are laid out from the bottom (stick-to-bottom) or from an anchor
//! item (when the user scrolled up, so rows arriving below or trimmed above do not move what is being read), and are
//! prepainted at fractional origins. Only the items in view are rendered; heights come from the pane's model.
//!
//! Content shorter than the viewport starts at the top, like a scroll container's content does.
use std::cell::RefCell;
use std::ops::Range;
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
}

#[derive(Clone)]
pub struct ChatListState(Rc<RefCell<Inner>>);

impl ChatListState {
    pub fn new() -> Self {
        ChatListState(Rc::new(RefCell::new(Inner { items: Vec::new(), anchor: None, vp: (0.0, 0.0), dist_to_bottom: 0.0, placed: Vec::new(), thumb: None })))
    }

    pub fn item_count(&self) -> usize {
        self.0.borrow().items.len()
    }

    /// Items `old` were replaced by `count` new ones (their heights are unknown); an anchor keeps its item.
    pub fn splice(&self, old: Range<usize>, count: usize) {
        let mut s = self.0.borrow_mut();
        let removed = old.end - old.start;
        s.items.splice(old.clone(), std::iter::repeat(None).take(count));
        if let Some((ix, off)) = s.anchor.as_mut() {
            if old.contains(ix) {
                *ix = old.start;
                *off = 0.0;
            } else if old.end <= *ix {
                *ix = *ix - removed + count;
            }
        }
        let n = s.items.len();
        if let Some((ix, _)) = s.anchor.as_mut() {
            if n > 0 && *ix >= n {
                *ix = n - 1;
            }
        }
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
    }
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
            let s = self.state.0.borrow();
            let known: Vec<f32> = s.items.iter().filter_map(|m| m.map(|m| m.h + m.mb.max(m.mt))).collect();
            let avg = if known.is_empty() { 20.0 } else { known.iter().sum::<f32>() / known.len() as f32 };
            let total: f32 = PAD * 2.0 + s.items.iter().map(|m| m.map(|m| m.h + m.mb.max(m.mt)).unwrap_or(avg)).sum::<f32>();
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
                let d = acc.pixel_delta(px(20.));
                acc = gpui::ScrollDelta::default();
                state.scroll_by(-d.y.f());
                window.refresh();
            }
        });
        let _ = pp.bounds;
    }
}
