//! Selecting message text with the mouse and copying it, like a browser chat: press on a message and drag to
//! select (across rows too), double-click selects a word, triple-click a whole line, Shift+click extends.
//! Ctrl+C (Cmd+C on macOS) or the right-click menu copies; emotes copy as their code.
//!
//! A `SelScope` is one list a selection lives in (a split's message list, its reply thread, the Mentions panel).
//! Every selectable flow drawn in a frame registers itself in its scope (`InlineFlow::selectable`) with its place
//! in the list (`FlowKey`: row, then the flow's number inside the row), its bounds and its content; the scope's
//! `SelLayer`, drawn after the list, turns the mouse into selection points against those. A selection point is a
//! flow and a unit inside it (`InlineContent::unit_at`), so the selection outlives scrolling and is copied from the
//! rows' contents, not from what is on screen. Nothing is laid out again for it: hit testing and the highlight use
//! the layouts the rows were drawn with.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use gpui::{
    px, relative, App, Bounds, ClipboardItem, DispatchPhase, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Style, Window, WindowId,
};

use super::inline::InlineContent;
use super::PxF;

/// A flow's place in its list: the row, then which of the row's flows (`BuiltRow::contents` order).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct FlowKey {
    pub row: u32,
    pub sub: u32,
}

/// A position in a list: a flow and a unit inside it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct SelPoint {
    pub key: FlowKey,
    pub unit: u32,
}

impl SelPoint {
    pub fn new(row: u32, sub: u32, unit: u32) -> Self {
        SelPoint { key: FlowKey { row, sub }, unit }
    }
}

/// A row's flows, each with its number in the row.
pub type RowFlows = Vec<(u32, Rc<InlineContent>)>;
/// The flows of a row by index, for rows that are not on screen (the message list draws only what is in view).
pub type RowsFn = Rc<dyn Fn(u32, &mut App) -> Option<RowFlows>>;
/// Told when the scope gets or loses a selection (deferred, so it may update the entity that owns the scope).
pub type ChangeFn = Rc<dyn Fn(bool, &mut App)>;
/// The right-click menu: where, and the text it would copy.
pub type ContextFn = Rc<dyn Fn(Point<Pixels>, String, &mut Window, &mut App)>;
/// Scrolls the list by px (negative = up), while a drag is held above or below it.
pub type ScrollFn = Rc<dyn Fn(f32)>;

struct Entry {
    key: FlowKey,
    bounds: Bounds<Pixels>,
    content: Rc<InlineContent>,
}

#[derive(Clone, Copy)]
struct Press {
    pos: Point<Pixels>,
    at: SelPoint,
}

/// Moves under this many px are still a click.
const DRAG_PX: f32 = 3.0;
/// How far above / below a flow a press beside the text still starts a selection on it (row padding).
const PAD_REACH: f32 = 8.0;

#[derive(Default)]
pub struct SelScope {
    window: Cell<Option<WindowId>>,
    /// the flows drawn in the last frame, and the ones the frame being drawn registers
    entries: RefCell<Vec<Entry>>,
    next: RefCell<Vec<Entry>>,
    /// (anchor, focus): where the drag began and where it is now
    sel: Cell<Option<(SelPoint, SelPoint)>>,
    press: Cell<Option<Press>>,
    dragging: Cell<bool>,
    reported: Cell<bool>,
    rows: RefCell<Option<RowsFn>>,
    on_change: RefCell<Option<ChangeFn>>,
    on_context: RefCell<Option<ContextFn>>,
    scroll: RefCell<Option<ScrollFn>>,
}

thread_local! {
    /// The scope holding the newest selection: what Ctrl+C copies.
    static ACTIVE: RefCell<Weak<SelScope>> = const { RefCell::new(Weak::new()) };
}

impl SelScope {
    pub fn new() -> Rc<SelScope> {
        Rc::new(SelScope::default())
    }

    pub fn set_rows(&self, f: RowsFn) {
        *self.rows.borrow_mut() = Some(f);
    }
    pub fn set_on_change(&self, f: ChangeFn) {
        *self.on_change.borrow_mut() = Some(f);
    }
    pub fn set_on_context(&self, f: ContextFn) {
        *self.on_context.borrow_mut() = Some(f);
    }
    pub fn set_scroll(&self, f: ScrollFn) {
        *self.scroll.borrow_mut() = Some(f);
    }

    /// A flow drawn this frame (`InlineFlow::prepaint`).
    pub fn register(&self, key: FlowKey, bounds: Bounds<Pixels>, content: Rc<InlineContent>) {
        self.next.borrow_mut().push(Entry { key, bounds, content });
    }

    /// Anchor and focus in list order.
    pub fn ordered(&self) -> Option<(SelPoint, SelPoint)> {
        let (a, f) = self.sel.get()?;
        Some(if a <= f { (a, f) } else { (f, a) })
    }

    /// Something is selected (not just a press).
    pub fn has_selection(&self) -> bool {
        self.ordered().is_some_and(|(a, b)| a != b)
    }

    /// A drag is selecting right now.
    pub fn dragging(&self) -> bool {
        self.dragging.get()
    }

    fn set_sel(self: &Rc<Self>, anchor: SelPoint, focus: SelPoint, cx: &mut App) {
        self.sel.set(Some((anchor, focus)));
        let prev = ACTIVE.with(|a| a.replace(Rc::downgrade(self)));
        if let Some(p) = prev.upgrade().filter(|p| !Rc::ptr_eq(p, self)) {
            p.clear(cx);
        }
        self.report(cx);
    }

    /// Drop the selection (and tell the owner).
    pub fn clear(&self, cx: &mut App) {
        self.sel.set(None);
        self.press.set(None);
        self.dragging.set(false);
        self.report(cx);
    }

    /// Drop the selection without telling the owner (the owner is doing it).
    pub fn clear_quiet(&self) {
        self.sel.set(None);
        self.press.set(None);
        self.dragging.set(false);
        self.reported.set(false);
    }

    /// The rows moved (the list was rebuilt): `f` maps an old row to its new index; a selection whose rows are
    /// gone is dropped. Returns whether a selection is left.
    pub fn remap_rows(&self, f: impl Fn(u32) -> Option<u32>) -> bool {
        let Some((a, b)) = self.sel.get() else { return false };
        match (f(a.key.row), f(b.key.row)) {
            (Some(ra), Some(rb)) => {
                self.sel.set(Some((SelPoint { key: FlowKey { row: ra, ..a.key }, ..a }, SelPoint { key: FlowKey { row: rb, ..b.key }, ..b })));
                true
            }
            _ => {
                self.clear_quiet();
                false
            }
        }
    }

    fn report(&self, cx: &mut App) {
        let has = self.has_selection();
        if has != self.reported.get() {
            self.reported.set(has);
            if let Some(f) = self.on_change.borrow().clone() {
                cx.defer(move |cx| f(has, cx));
            }
        }
    }

    /// The selection point under (or nearest to) a window position, from the flows drawn last.
    fn hit(&self, pos: Point<Pixels>) -> Option<SelPoint> {
        let entries = self.entries.borrow();
        let (x, y) = (pos.x.f(), pos.y.f());
        let dist = |lo: f32, hi: f32, v: f32| if v < lo { lo - v } else if v > hi { v - hi } else { 0.0 };
        let e = entries
            .iter()
            .filter(|e| e.bounds.size.height.f() > 0.0)
            .min_by(|a, b| {
                let da = (dist(a.bounds.top().f(), a.bounds.bottom().f(), y), dist(a.bounds.left().f(), a.bounds.right().f(), x));
                let db = (dist(b.bounds.top().f(), b.bounds.bottom().f(), y), dist(b.bounds.left().f(), b.bounds.right().f(), x));
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })?;
        let unit = e.content.unit_at(x - e.bounds.left().f(), y - e.bounds.top().f()).unwrap_or(0);
        Some(SelPoint { key: e.key, unit })
    }

    /// A left press beside the flows (a row's padding, its left bar, the gap between rows): it starts a selection
    /// at the nearest flow, as a press on the text would. Not right of the flows (the scrollbar) and not far off.
    fn pad_down(self: &Rc<Self>, ev: &MouseDownEvent, cx: &mut App) -> bool {
        let (x, y) = (ev.position.x.f(), ev.position.y.f());
        let target = {
            let entries = self.entries.borrow();
            if entries.iter().any(|e| e.bounds.contains(&ev.position)) {
                return false;
            }
            let near = entries.iter().filter(|e| x <= e.bounds.right().f()).map(|e| {
                let dy = if y < e.bounds.top().f() { e.bounds.top().f() - y } else if y > e.bounds.bottom().f() { y - e.bounds.bottom().f() } else { 0.0 };
                (dy, e)
            });
            match near.min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal)) {
                Some((dy, e)) if dy <= PAD_REACH => Some((e.key, e.content.clone(), e.bounds)),
                _ => None,
            }
        };
        let Some((key, content, b)) = target else { return false };
        let unit = content.unit_at(x - b.left().f(), (y - b.top().f()).clamp(0.0, (b.size.height.f() - 0.5).max(0.0))).unwrap_or(0);
        self.flow_down(key, unit, &content, ev, cx);
        true
    }

    /// A left press on a selectable flow, at unit `unit` (after the parts that act on a press had their turn).
    pub fn flow_down(self: &Rc<Self>, key: FlowKey, unit: u32, content: &InlineContent, ev: &MouseDownEvent, cx: &mut App) {
        let at = SelPoint { key, unit };
        match ev.click_count {
            2 => {
                let (a, b) = content.word_at(unit);
                self.set_sel(SelPoint { key, unit: a }, SelPoint { key, unit: b }, cx);
            }
            n if n >= 3 => self.set_sel(SelPoint { key, unit: 0 }, SelPoint { key, unit: content.unit_len() }, cx),
            _ => match self.sel.get().filter(|_| ev.modifiers.shift) {
                Some((anchor, _)) => {
                    self.set_sel(anchor, at, cx);
                    self.press.set(Some(Press { pos: ev.position, at: anchor }));
                    self.dragging.set(true);
                }
                None => self.press.set(Some(Press { pos: ev.position, at })),
            },
        }
    }

    /// The selected text (rows off screen come from the owner's `rows`, the rest from what was drawn).
    pub fn text(&self, cx: &mut App) -> Option<String> {
        let rows = self.rows.borrow().clone();
        match rows {
            Some(f) => self.text_with(&mut |r| f(r, cx)),
            None => {
                let drawn = self.drawn_rows();
                self.text_with(&mut |r| drawn.get(&r).cloned())
            }
        }
    }

    fn drawn_rows(&self) -> std::collections::HashMap<u32, RowFlows> {
        let mut m: std::collections::HashMap<u32, RowFlows> = std::collections::HashMap::new();
        for e in self.entries.borrow().iter() {
            let v = m.entry(e.key.row).or_default();
            if !v.iter().any(|(s, _)| *s == e.key.sub) {
                v.push((e.key.sub, e.content.clone()));
            }
        }
        for v in m.values_mut() {
            v.sort_by_key(|(s, _)| *s);
        }
        m
    }

    /// The selected text, with the rows' flows from `rows`.
    pub fn text_with(&self, rows: &mut dyn FnMut(u32) -> Option<RowFlows>) -> Option<String> {
        let sel = self.ordered().filter(|(a, b)| a != b)?;
        let t = selection_text(sel, rows);
        (!t.is_empty()).then_some(t)
    }
}

/// The units of flow `key` inside the selection [a, b], None when it is outside.
pub fn range_in((a, b): (SelPoint, SelPoint), key: FlowKey, total: u32) -> Option<(u32, u32)> {
    if key < a.key || key > b.key {
        return None;
    }
    let s = if key == a.key { a.unit.min(total) } else { 0 };
    let e = if key == b.key { b.unit.min(total) } else { total };
    (s < e).then_some((s, e))
}

/// The text of a selection: each selected flow's text on its own line.
pub fn selection_text((a, b): (SelPoint, SelPoint), rows: &mut dyn FnMut(u32) -> Option<RowFlows>) -> String {
    let mut lines: Vec<String> = Vec::new();
    for row in a.key.row..=b.key.row {
        let Some(flows) = rows(row) else { continue };
        for (sub, c) in flows {
            if let Some((s, e)) = range_in((a, b), FlowKey { row, sub }, c.unit_len()) {
                let t = c.copy_text(s, e);
                if !t.is_empty() {
                    lines.push(t);
                }
            }
        }
    }
    lines.join("\n")
}

/// Ctrl+C / Cmd+C (and Ctrl+Insert off macOS) copy the chat selection of the window it was pressed in, even while
/// the message box has the keyboard. Call once at startup.
pub fn install(cx: &mut App) {
    cx.intercept_keystrokes(|ev, window, cx| {
        let k = &ev.keystroke;
        let m = &k.modifiers;
        let copy = (k.key == "c" && crate::text_input::primary_held(m) && !m.shift && !m.alt) || (!cfg!(target_os = "macos") && k.key == "insert" && m.control && !m.shift && !m.alt);
        if !copy {
            return;
        }
        let Some(scope) = ACTIVE.with(|a| a.borrow().upgrade()) else { return };
        if scope.window.get() != Some(window.window_handle().window_id()) || !scope.has_selection() {
            return;
        }
        if let Some(text) = scope.text(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            cx.stop_propagation();
        }
    })
    .detach();
}

/// The scope's mouse handling, drawn after its list and filling it: a press anywhere drops the selection, a drag
/// that began on a flow moves its focus, a right-click on the list offers Copy, and a drag held above or below the
/// list scrolls it.
pub fn sel_layer(scope: Rc<SelScope>) -> SelLayer {
    SelLayer { scope }
}

pub struct SelLayer {
    scope: Rc<SelScope>,
}

impl IntoElement for SelLayer {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for SelLayer {
    type RequestLayoutState = ();
    /// over the list, after its rows: hovered when nothing that blocks the mouse (a sheet, a menu) is on top
    type PrepaintState = gpui::Hitbox;

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.position = gpui::Position::Absolute;
        style.inset.top = px(0.).into();
        style.inset.left = px(0.).into();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), window: &mut Window, _: &mut App) -> gpui::Hitbox {
        // the list drew before this: its flows are the ones to hit-test from now on
        let s = &self.scope;
        let next = std::mem::take(&mut *s.next.borrow_mut());
        *s.entries.borrow_mut() = next;
        s.window.set(Some(window.window_handle().window_id()));
        window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal)
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), area: &mut gpui::Hitbox, window: &mut Window, cx: &mut App) {
        let s = self.scope.clone();
        // a drag held above or below the list scrolls it, a step a frame, and the focus follows
        if s.dragging.get() {
            if let Some(scroll) = s.scroll.borrow().clone() {
                let m = window.mouse_position();
                let over = if m.y < bounds.top() { (m.y - bounds.top()).f() } else if m.y > bounds.bottom() { (m.y - bounds.bottom()).f() } else { 0.0 };
                if over != 0.0 {
                    scroll(over.clamp(-40.0, 40.0) / 4.0);
                    let edge = Point { x: m.x, y: m.y.clamp(bounds.top(), bounds.bottom()) };
                    if let (Some(p), Some(f)) = (s.press.get(), s.hit(edge)) {
                        s.set_sel(p.at, f, cx);
                    }
                    window.request_animation_frame();
                }
            }
        }
        let s1 = s.clone();
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || ev.button != MouseButton::Left {
                return;
            }
            s1.press.set(None);
            s1.dragging.set(false);
            if !ev.modifiers.shift && s1.sel.get().is_some() {
                s1.sel.set(None);
                s1.report(cx);
                window.refresh();
            }
        });
        let s2 = s.clone();
        window.on_mouse_event(move |ev: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture {
                return;
            }
            let Some(p) = s2.press.get() else { return };
            if ev.pressed_button != Some(MouseButton::Left) {
                // released outside the window
                s2.press.set(None);
                s2.dragging.set(false);
                return;
            }
            if !s2.dragging.get() {
                let d = ev.position - p.pos;
                if d.x.f().abs() < DRAG_PX && d.y.f().abs() < DRAG_PX {
                    return;
                }
                s2.dragging.set(true);
            }
            if let Some(f) = s2.hit(ev.position) {
                s2.set_sel(p.at, f, cx);
            }
            window.refresh();
        });
        let s3 = s.clone();
        window.on_mouse_event(move |ev: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || ev.button != MouseButton::Left {
                return;
            }
            let dragged = s3.dragging.get();
            s3.press.set(None);
            s3.dragging.set(false);
            if dragged {
                // the end of a drag is not a click on what it ended over (a Mentions entry would jump)
                cx.stop_propagation();
                window.refresh();
            }
        });
        let (s5, area5) = (s.clone(), area.clone());
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
            // runs before the rows' handlers (it was registered after them), so only where no flow is
            if phase == DispatchPhase::Bubble && ev.button == MouseButton::Left && area5.is_hovered(window) && s5.pad_down(ev, cx) {
                window.refresh();
            }
        });
        let s4 = s;
        window.on_mouse_event(move |ev: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || ev.button != MouseButton::Right || !bounds.contains(&ev.position) || !s4.has_selection() {
                return;
            }
            let Some(menu) = s4.on_context.borrow().clone() else { return };
            if let Some(text) = s4.text(cx) {
                menu(ev.position, text, window, cx);
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::inline::{ImageItem, Item, PillItem, TextItem, Tip, VAlign};
    use gpui::{FontWeight, Hsla};

    fn text(t: &str) -> Item {
        Item::Text(TextItem::new(t.to_string(), 13.0, Hsla::default()))
    }
    fn image(name: &str, fixed: bool) -> Item {
        Item::Image(ImageItem {
            url: "u".into(),
            max_w: 28.0,
            max_h: 28.0,
            fixed,
            wrap: true,
            vmargin: 0.0,
            margin_x: 0.0,
            valign: VAlign::Middle,
            tip: Some(Rc::new(Tip::new("", name.to_string(), ""))),
            overlays: vec![],
            radius: 0.0,
            act: None,
        })
    }
    fn pill(t: &str, copy: bool) -> Item {
        Item::Pill(PillItem { text: t.to_string().into(), size: 11.0, color: Hsla::default(), weight: FontWeight::BOLD, bg: Hsla::default(), pad_x: 4.0, margin_r: 4.0, radius: 4.0, copy })
    }

    /// "12:34 [badge] Agu: hi KEKW there"
    fn chat_line() -> Rc<InlineContent> {
        InlineContent::new(vec![text("12:34"), Item::Gap(6.0), image("Moderator", true), Item::Gap(4.0), text("Agu"), text(":"), text(" "), text("hi "), image("KEKW", false), text(" there")], 13.0, 1.5)
    }

    #[test]
    fn a_whole_line_copies_as_name_and_text_with_emote_codes() {
        let c = chat_line();
        assert_eq!(c.copy_text(0, c.unit_len()), "12:34 Agu: hi KEKW there");
    }

    #[test]
    fn part_of_a_line_copies_only_that_part() {
        let c = chat_line();
        // units: "12:34"=0..5, gap 5, badge 6, gap 7, "Agu" 8..11, ":" 11, " " 12, "hi " 13..16, KEKW 16, " there" 17..23
        assert_eq!(c.copy_text(13, 17), "hi KEKW");
        assert_eq!(c.copy_text(18, 21), "the");
        assert_eq!(c.copy_text(6, 7), "");
        assert_eq!(c.copy_text(8, 11), "Agu");
    }

    #[test]
    fn badges_icons_and_chips_are_left_out_but_amounts_are_kept() {
        let c = InlineContent::new(vec![pill("M", false), text("bob"), text(": "), pill("$5", true), text("thanks")], 13.0, 1.5);
        assert_eq!(c.copy_text(0, c.unit_len()), "bob: $5 thanks");
    }

    #[test]
    fn double_click_takes_the_word_or_the_emote() {
        let c = chat_line();
        assert_eq!(c.word_at(19), (18, 23)); // "there"
        assert_eq!(c.word_at(23), (18, 23)); // just after its last letter
        assert_eq!(c.word_at(16), (16, 17)); // the emote
        assert_eq!(c.word_at(9), (8, 11)); // the name
    }

    #[test]
    fn flows_inside_the_selection_take_all_their_units() {
        let sel = (SelPoint::new(2, 0, 5), SelPoint::new(4, 1, 3));
        assert_eq!(range_in(sel, FlowKey { row: 1, sub: 0 }, 10), None);
        assert_eq!(range_in(sel, FlowKey { row: 2, sub: 0 }, 10), Some((5, 10)));
        assert_eq!(range_in(sel, FlowKey { row: 3, sub: 0 }, 10), Some((0, 10)));
        assert_eq!(range_in(sel, FlowKey { row: 4, sub: 0 }, 10), Some((0, 10)));
        assert_eq!(range_in(sel, FlowKey { row: 4, sub: 1 }, 10), Some((0, 3)));
        assert_eq!(range_in(sel, FlowKey { row: 4, sub: 2 }, 10), None);
        // the anchor at a flow's very end selects nothing of it
        assert_eq!(range_in((SelPoint::new(2, 0, 10), SelPoint::new(3, 0, 2)), FlowKey { row: 2, sub: 0 }, 10), None);
    }

    #[test]
    fn a_selection_over_rows_copies_one_line_per_row() {
        let rows: Vec<Rc<InlineContent>> = vec![
            InlineContent::new(vec![text("Agu"), text(": "), text("first line")], 13.0, 1.5),
            InlineContent::new(vec![text("Bob"), text(": "), text("second "), image("KEKW", false)], 13.0, 1.5),
            InlineContent::new(vec![text("Cat"), text(": "), text("third")], 13.0, 1.5),
        ];
        let mut get = |r: u32| rows.get(r as usize).map(|c| vec![(0u32, c.clone())]);
        // from "line" in the first row to "Ca" in the third
        let t = selection_text((SelPoint::new(0, 0, 11), SelPoint::new(2, 0, 2)), &mut get);
        assert_eq!(t, "line\nBob: second KEKW\nCa");
        let scope = SelScope::new();
        scope.sel.set(Some((SelPoint::new(2, 0, 2), SelPoint::new(0, 0, 11))));
        assert_eq!(scope.text_with(&mut get).as_deref(), Some("line\nBob: second KEKW\nCa"));
    }

    #[test]
    fn rebuilt_rows_carry_the_selection_or_drop_it() {
        let scope = SelScope::new();
        scope.sel.set(Some((SelPoint::new(5, 0, 1), SelPoint::new(7, 0, 2))));
        assert!(scope.remap_rows(|r| r.checked_sub(3)));
        assert_eq!(scope.ordered(), Some((SelPoint::new(2, 0, 1), SelPoint::new(4, 0, 2))));
        assert!(!scope.remap_rows(|r| if r == 2 { None } else { Some(r) }));
        assert_eq!(scope.ordered(), None);
    }
}
