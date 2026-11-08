//! Port of src/renderer/layout/SplitView.tsx.
//! Renders one tab's SplitNode tree.
//!
//! The tree is laid out to fractional rectangles and every leaf is drawn as a flat, absolutely positioned child.
//! Each ChatPane lives in `AguApp::panes` (keyed by split id), so a pane stays alive (and its scrollback intact)
//! when a sibling is closed, a split is moved, or the tree reshapes.
use agu_chat_core::protocol::SplitNode;
use gpui::{
    div, prelude::*, px, relative, AnyElement, Bounds, ClickEvent, Context, CursorStyle, DragMoveEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    SharedString, Window,
};

use crate::layout::app::{AguApp, SourcesDrag, SplitDrag, TabDrag};
use crate::layout::tree::{source_key, Axis, Edge, Leaf};
use crate::layout::ui::{btn, BtnOpts};
use crate::theme::{self, a, c};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

pub struct Divider {
    pub key: String,
    pub container_id: String,
    pub index: usize,
    pub axis: Axis,
    pub rect: Rect,
    pub container: Rect,
    pub sizes: Vec<f32>,
}

const MIN_W: f32 = 140.0;
const MIN_H: f32 = 70.0;

pub fn compute_layout(root: Option<&SplitNode>) -> (Vec<(Leaf, Rect)>, Vec<Divider>) {
    fn walk(n: &SplitNode, r: Rect, leaves: &mut Vec<(Leaf, Rect)>, dividers: &mut Vec<Divider>) {
        match n {
            SplitNode::Leaf { id, sources, mod_mode } => leaves.push((Leaf { id: id.clone(), sources: sources.clone(), mod_mode: *mod_mode }, r)),
            SplitNode::Row { id, children, sizes } | SplitNode::Col { id, children, sizes } => {
                let row = matches!(n, SplitNode::Row { .. });
                let mut cur = if row { r.x } else { r.y };
                for (i, child) in children.iter().enumerate() {
                    let f = sizes.get(i).copied().unwrap_or(1.0 / children.len() as f32);
                    let cr = if row { Rect { x: cur, y: r.y, w: r.w * f, h: r.h } } else { Rect { x: r.x, y: cur, w: r.w, h: r.h * f } };
                    walk(child, cr, leaves, dividers);
                    cur += if row { cr.w } else { cr.h };
                    if i < children.len() - 1 {
                        let line = if row { Rect { x: cur, y: r.y, w: 0.0, h: r.h } } else { Rect { x: r.x, y: cur, w: r.w, h: 0.0 } };
                        dividers.push(Divider {
                            key: format!("{id}:{i}"),
                            container_id: id.clone(),
                            index: i,
                            axis: if row { Axis::Row } else { Axis::Col },
                            rect: line,
                            container: r,
                            sizes: sizes.clone(),
                        });
                    }
                }
            }
        }
    }
    let mut leaves = Vec::new();
    let mut dividers = Vec::new();
    if let Some(root) = root {
        walk(root, Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }, &mut leaves, &mut dividers);
    }
    (leaves, dividers)
}

/// A whole split being moved (its header), some of a split's channels (a header chip), or another tab.
#[derive(Clone, Debug, PartialEq)]
pub enum Dragging {
    Split { leaf_id: String },
    Sources { leaf_id: String, keys: Vec<String> },
    Tab { tab_id: String },
}

/// Where a drop lands: an edge makes a new split there, the middle merges into this split.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    Edge(Edge),
    Center,
}

fn zone_at(bounds: Bounds<Pixels>, pos: Point<Pixels>, can_merge: bool) -> Zone {
    let x = f32::from(pos.x - bounds.left()) / f32::from(bounds.size.width) - 0.5;
    let y = f32::from(pos.y - bounds.top()) / f32::from(bounds.size.height) - 0.5;
    if can_merge && x.abs() < 0.25 && y.abs() < 0.25 {
        return Zone::Center;
    }
    if x.abs() > y.abs() {
        Zone::Edge(if x < 0.0 { Edge::Left } else { Edge::Right })
    } else {
        Zone::Edge(if y < 0.0 { Edge::Top } else { Edge::Bottom })
    }
}

pub struct Resize {
    pub tab_id: String,
    pub container_id: String,
    pub index: usize,
    pub horizontal: bool,
    pub start: f32,
    pub base: Vec<f32>,
    pub px: f32,
}

#[derive(Default)]
pub struct SplitViewState {
    /// own drag (header / chip); a tab drag comes from `AguApp::tab_drag`
    pub dragging: Option<Dragging>,
    pub hover: Option<(String, Zone)>,
    pub resize: Option<Resize>,
}

impl SplitViewState {
    pub fn clear_drag(&mut self) {
        self.dragging = None;
        self.hover = None;
    }
}

impl AguApp {
    /// The drag in progress, if any: a split / chip from this view, or another tab dragged from the tab bar.
    fn current_dragging(&self, cx: &Context<Self>) -> Option<Dragging> {
        if !cx.has_active_drag() {
            return None;
        }
        if let Some(d) = &self.sv.dragging {
            return Some(d.clone());
        }
        let tab = self.tab_drag.as_ref()?;
        // Another tab being dragged from the tab bar: its splits can be dropped beside these.
        let has_root = self.layout.tabs.iter().find(|t| &t.id == tab).map(|t| t.root.is_some()).unwrap_or(false);
        (tab != &self.layout.active && has_root).then(|| Dragging::Tab { tab_id: tab.clone() })
    }

    pub fn on_root_mouse_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(r) = &self.sv.resize else { return };
        let pos = if r.horizontal { f32::from(ev.position.x) } else { f32::from(ev.position.y) };
        let delta = (pos - r.start) / r.px;
        let pair = r.base[r.index] + r.base[r.index + 1];
        let min_frac = ((if r.horizontal { MIN_W } else { MIN_H }) / r.px).min(pair / 2.0);
        let av = (r.base[r.index] + delta).min(pair - min_frac).max(min_frac);
        let mut next = r.base.clone();
        next[r.index] = av;
        next[r.index + 1] = pair - av;
        let (tab, cid) = (r.tab_id.clone(), r.container_id.clone());
        self.resize(&tab, &cid, &next, cx);
    }

    pub fn on_root_mouse_up(&mut self, _ev: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.sv.resize.take().is_some() {
            cx.notify();
        }
    }

    fn start_resize(&mut self, d: &Divider, ev: &MouseDownEvent, cx: &mut Context<Self>) {
        let Some(host) = self.anchor("split-host") else { return };
        let horizontal = d.axis == Axis::Row;
        let px_total = if horizontal { f32::from(host.size.width) * d.container.w } else { f32::from(host.size.height) * d.container.h };
        self.sv.resize = Some(Resize {
            tab_id: self.layout.active.clone(),
            container_id: d.container_id.clone(),
            index: d.index,
            horizontal,
            start: if horizontal { f32::from(ev.position.x) } else { f32::from(ev.position.y) },
            base: d.sizes.clone(),
            px: px_total.max(1.0),
        });
        cx.stop_propagation();
    }

    pub fn render_split_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tab = self.active_tab().clone();
        let tab_id = tab.id.clone();
        let dragging = self.current_dragging(cx);

        let Some(root) = tab.root.as_ref() else {
            let tab_drag = match &dragging {
                Some(Dragging::Tab { tab_id }) => Some(tab_id.clone()),
                _ => None,
            };
            let drag_id = self.tab_drag.clone().filter(|_| cx.has_active_drag());
            return div()
                .id("split-empty")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .gap(px(8.))
                .bg(c(theme::PANEL))
                .on_drop(cx.listener({
                    let tab_id = tab_id.clone();
                    move |this, d: &TabDrag, _w, cx| this.take_tab(&tab_id, &d.tab_id, None, Edge::Right, cx)
                }))
                .child(btn("add-first", "Add a channel", BtnOpts::primary()).on_click({
                    let tab_id = tab_id.clone();
                    cx.listener(move |this, _: &ClickEvent, w, cx| this.open_dialog(crate::layout::app::Dialog::Add { tab_id: tab_id.clone() }, w, cx))
                }))
                .child(btn("add-whisper", "Whisper", BtnOpts::default()).on_click({
                    let tab_id = tab_id.clone();
                    cx.listener(move |this, _: &ClickEvent, w, cx| this.open_dialog(crate::layout::app::Dialog::Whisper { tab_id: tab_id.clone() }, w, cx))
                }))
                .child(btn("empty-settings", "Settings", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open_dialog(crate::layout::app::Dialog::Settings, w, cx))))
                .when(tab_drag.is_some() || drag_id.is_some(), |d| {
                    d.child(div().absolute().top(px(8.)).left(px(8.)).right(px(8.)).bottom(px(8.)).rounded(px(2.)).border_2().border_dashed().border_color(c(theme::ACCENT)))
                })
                .into_any_element();
        };

        let (leaves, dividers) = compute_layout(Some(root));
        let multi = leaves.len() > 1;
        let focused_id = self.focused_of(&tab);
        // The split whose header sits under the window's top-right corner gets the Settings button.
        let corner = leaves
            .iter()
            .filter(|(_, r)| r.y < 1e-6)
            .fold(None::<&(Leaf, Rect)>, |best, cur| match best {
                Some(b) if cur.1.x + cur.1.w <= b.1.x + b.1.w => Some(b),
                _ => Some(cur),
            })
            .map(|(l, _)| l.id.clone());
        let host = self.anchor("split-host");
        let host_w = host.map(|b| f32::from(b.size.width)).unwrap_or(f32::from(window.viewport_size().width));
        let host_h = host.map(|b| f32::from(b.size.height)).unwrap_or(f32::from(window.viewport_size().height));

        let mut host_el = div().id("split-host").relative().size_full().overflow_hidden().bg(c(theme::PANEL)).child(self.probe("split-host".into(), cx));

        for (leaf, r) in &leaves {
            let focused = focused_id.as_deref() == Some(leaf.id.as_str());
            let header_w = host_w * r.w - 12.0;
            let view = self.render_leaf(&tab_id, leaf, focused, multi && focused, corner.as_deref() == Some(leaf.id.as_str()), header_w, dragging.as_ref(), cx);
            host_el = host_el.child(div().absolute().left(relative(r.x)).top(relative(r.y)).w(relative(r.w)).h(relative(r.h)).flex().flex_col().min_h_0().min_w_0().child(view));
        }

        for d in &dividers {
            let horizontal = d.axis == Axis::Row;
            let (key, container_id, index) = (d.key.clone(), d.container_id.clone(), d.index);
            let sizes = d.sizes.clone();
            let tab_id2 = tab_id.clone();
            let cx_div = Divider { key: d.key.clone(), container_id: d.container_id.clone(), index: d.index, axis: d.axis, rect: d.rect, container: d.container, sizes: d.sizes.clone() };
            // Chromium snaps the 1px line to a device pixel: calc(x% - 0.5px)
            let line_px = |frac: f32, total: f32| px((frac * total - 0.5).round());
            let mut wrap = div().absolute();
            let mut handle = div().id(SharedString::from(format!("divider-{key}"))).absolute().hover(|s| s.bg(a(theme::ACCENT, 0.4)));
            let mut line = div().absolute().bg(c(theme::LINE));
            if horizontal {
                wrap = wrap.left(line_px(d.rect.x, host_w)).top(relative(d.rect.y)).w(px(1.)).h(relative(d.rect.h));
                line = line.top_0().left_0().w(px(1.)).h_full();
                handle = handle.left(px(-3.)).w(px(7.)).top_0().bottom_0().cursor(CursorStyle::ResizeLeftRight);
            } else {
                wrap = wrap.top(line_px(d.rect.y, host_h)).left(relative(d.rect.x)).h(px(1.)).w(relative(d.rect.w));
                line = line.top_0().left_0().h(px(1.)).w_full();
                handle = handle.top(px(-3.)).h(px(7.)).left_0().right_0().cursor(CursorStyle::ResizeUpDown);
            }
            let handle = handle
                .tooltip(crate::layout::ui::tip("Drag to resize, double-click to even out"))
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _w, cx| this.start_resize(&cx_div, ev, cx)))
                .on_click(cx.listener(move |this, ev: &ClickEvent, _w, cx| {
                    if ev.click_count() >= 2 {
                        let mut next = sizes.clone();
                        let avg = (next[index] + next[index + 1]) / 2.0;
                        next[index] = avg;
                        next[index + 1] = avg;
                        this.resize(&tab_id2, &container_id, &next, cx);
                    }
                }));
            host_el = host_el.child(wrap.child(line).child(handle));
        }
        host_el.absolute().top_0().left_0().into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_leaf(&mut self, tab_id: &str, leaf: &Leaf, focused: bool, outline: bool, settings_button: bool, header_w: f32, dragging: Option<&Dragging>, cx: &mut Context<Self>) -> AnyElement {
        let leaf_id = leaf.id.clone();
        // A split can't be dropped on itself; its channels can (beside it), unless they are all it has.
        let drop_target = match dragging {
            None => false,
            Some(Dragging::Tab { .. }) => true,
            Some(Dragging::Split { leaf_id: l }) => *l != leaf.id,
            Some(Dragging::Sources { leaf_id: l, keys }) => *l != leaf.id || keys.len() < leaf.sources.len(),
        };
        // Merging only means something into another split; a whole tab only goes beside one.
        let can_merge = match dragging {
            Some(Dragging::Split { leaf_id: l }) | Some(Dragging::Sources { leaf_id: l, .. }) => *l != leaf.id,
            _ => false,
        };
        let header = self.render_header(tab_id, leaf, focused, settings_button, header_w, cx);
        let body = self.panes.get(&leaf.id).map(|s| s.view.clone());

        let mut el = div()
            .id(SharedString::from(format!("leaf-{leaf_id}")))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(c(theme::PANEL))
            .capture_any_mouse_down(cx.listener({
                let tab_id = tab_id.to_string();
                let leaf_id = leaf_id.clone();
                move |this, _: &MouseDownEvent, _w, cx| this.focus_leaf(&tab_id, &leaf_id, cx)
            }))
            .child(header)
            .child(div().relative().min_h_0().flex_1().overflow_hidden().children(body));

        if drop_target {
            let hover = self.sv.hover.clone().filter(|(l, _)| *l == leaf_id).map(|(_, z)| z);
            let tid = tab_id.to_string();
            let lid = leaf_id.clone();
            let over = move |this: &mut AguApp, bounds: Bounds<Pixels>, pos: Point<Pixels>, cx: &mut Context<AguApp>| {
                let z = if bounds.contains(&pos) { Some((lid.clone(), zone_at(bounds, pos, can_merge))) } else { None };
                let mine = this.sv.hover.as_ref().map(|(l, _)| *l == lid).unwrap_or(false);
                if z.is_some() && this.sv.hover != z {
                    this.sv.hover = z;
                    cx.notify();
                } else if z.is_none() && mine {
                    this.sv.hover = None;
                    cx.notify();
                }
            };
            let over2 = over.clone();
            let over3 = over.clone();
            let mut overlay = div()
                .id(SharedString::from(format!("drop-{leaf_id}")))
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<SplitDrag>, _w, cx| over(this, ev.bounds, ev.event.position, cx)))
                .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<SourcesDrag>, _w, cx| over2(this, ev.bounds, ev.event.position, cx)))
                .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<TabDrag>, _w, cx| over3(this, ev.bounds, ev.event.position, cx)));
            let (t1, t2, t3, l1, l2, l3) = (tid.clone(), tid.clone(), tid, leaf_id.clone(), leaf_id.clone(), leaf_id.clone());
            overlay = overlay
                .on_drop(cx.listener(move |this, d: &SplitDrag, _w, cx| {
                    let zone = this.take_zone(&l1);
                    match zone {
                        Zone::Center => this.merge(&t1, &d.leaf_id, None, &l1, cx),
                        Zone::Edge(e) => this.move_split(&t1, &d.leaf_id, &l1, e, cx),
                    }
                    cx.notify();
                }))
                .on_drop(cx.listener(move |this, d: &SourcesDrag, _w, cx| {
                    let zone = this.take_zone(&l2);
                    match zone {
                        Zone::Center => this.merge(&t2, &d.leaf_id, Some(&d.keys), &l2, cx),
                        Zone::Edge(e) => this.split_out(&t2, &d.leaf_id, &d.keys, &l2, e, cx),
                    }
                    cx.notify();
                }))
                .on_drop(cx.listener(move |this, d: &TabDrag, _w, cx| {
                    let zone = this.take_zone(&l3);
                    if let Zone::Edge(e) = zone {
                        this.take_tab(&t3, &d.tab_id, Some(&l3), e, cx);
                    }
                    cx.notify();
                }));
            if let Some(z) = hover {
                let mut hl = div()
                    .absolute()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(2.))
                    .border_2()
                    .border_color(c(theme::ACCENT))
                    .bg(a(theme::ACCENT, 0.25));
                hl = match z {
                    Zone::Edge(Edge::Left) => hl.left_0().top_0().w(relative(0.5)).h(relative(1.0)),
                    Zone::Edge(Edge::Right) => hl.right_0().top_0().w(relative(0.5)).h(relative(1.0)),
                    Zone::Edge(Edge::Top) => hl.left_0().top_0().w(relative(1.0)).h(relative(0.5)),
                    Zone::Edge(Edge::Bottom) => hl.left_0().bottom_0().w(relative(1.0)).h(relative(0.5)),
                    Zone::Center => hl.left(relative(0.25)).top(relative(0.25)).w(relative(0.5)).h(relative(0.5)),
                };
                if z == Zone::Center {
                    hl = hl.child(
                        div()
                            .rounded(px(4.))
                            .bg(c(theme::ACCENT))
                            .px(px(8.))
                            .py(px(2.))
                            .text_size(px(11.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(c(theme::WHITE))
                            .child("Merge into this split"),
                    );
                }
                overlay = overlay.child(hl);
            }
            el = el.child(overlay);
        }

        if outline {
            el = el.child(div().absolute().top_0().left_0().size_full().border_1().border_color(c(theme::ACCENT)));
        }
        el.into_any_element()
    }

    /// The zone the pointer was last over on this split's drop overlay (the drop event carries no position).
    fn take_zone(&mut self, leaf_id: &str) -> Zone {
        let z = self.sv.hover.take();
        self.sv.dragging = None;
        self.tab_drag = None;
        match z {
            Some((l, z)) if l == leaf_id => z,
            _ => Zone::Edge(Edge::Right),
        }
    }
}

#[allow(dead_code)]
fn _s(_: &str) -> String {
    source_key(&agu_chat_core::protocol::SourceRef { platform: agu_chat_core::protocol::Platform::Twitch, channel: String::new() })
}
