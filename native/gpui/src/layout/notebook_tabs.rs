//! Port of src/renderer/layout/NotebookTabs.tsx.
//! The notebook's tab bar, which is also the window's title bar (the window has no OS frame): select, add,
//! close the selected tab (x / middle-click), rename (double-click), drag to reorder and the window buttons.
//! The bar never scrolls: when the tabs don't all fit it shows a run of them that always holds the selected one
//! (tab_window.rs), and the arrow at the left edge lists every tab. Empty space drags the window; double-click
//! maximizes (both are the native caption behaviour: `WindowControlArea::Drag`).
use gpui::{
    div, font, prelude::*, px, AnyElement, Bounds, ClickEvent, Context, DragMoveEvent, Entity, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    SharedString, Subscription, TextRun, Window, WindowControlArea,
};

use std::collections::HashSet;

use agu_chat_core::protocol::{ConnFrame, Tab};

use crate::icons;
use crate::layout::app::{AguApp, DragGhost, SourcesDrag, SplitDrag, TabDrag};
use crate::chat::chat_pane::Activity;
use crate::layout::hotkeys::{hotkey_hint, HotkeyAction};
use crate::layout::macwin;
use crate::layout::tab_window::tab_window;
use crate::layout::tree::{auto_title, leaves, source_key, tab_title, whisper_only};
use crate::layout::ui::{tip, MenuEntry, MenuItem, MenuState};
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

const ALL_W: f32 = 25.0; // the All tabs button, border included
const PLUS_W: f32 = 28.0;

#[derive(Default)]
pub struct TabsState {
    pub editing: Option<String>,
    pub input: Option<(Entity<TextInput>, Subscription)>,
    /// a tab being dragged (reorder)
    pub drag_id: Option<String>,
    /// insertion index while dragging
    pub drop_at: Option<usize>,
    /// a split or channel is being dragged over the bar
    pub split_over: bool,
    /// startRef: where the shown run of tabs starts
    pub start: usize,
}

impl TabsState {
    pub fn clear_drag(&mut self) {
        self.drag_id = None;
        self.drop_at = None;
        self.split_over = false;
    }
}

/// What a tab's small circle says. Ordered by priority: a mention beats live, live beats new messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TabDot {
    Message,
    Live,
    Highlight,
}

impl TabDot {
    pub fn color(self) -> u32 {
        match self {
            TabDot::Message => theme::FAINT,
            TabDot::Live => theme::EMERALD_500, // the split header's connected / live dot
            TabDot::Highlight => theme::RED_500,
        }
    }
}

/// The dot a tab shows. The selected tab has seen its messages and mentions, so only live can show on it.
pub fn tab_dot(selected: bool, activity: Option<Activity>, live: bool) -> Option<TabDot> {
    let act = if selected { None } else { activity };
    match act {
        Some(Activity::Highlight) => Some(TabDot::Highlight),
        _ if live => Some(TabDot::Live),
        Some(Activity::Message) => Some(TabDot::Message),
        None => None,
    }
}

/// The menu hint beside a tab in the All tabs list.
fn dot_hint(d: TabDot) -> &'static str {
    match d {
        TabDot::Highlight => "highlight",
        TabDot::Live => "live",
        TabDot::Message => "new",
    }
}

/// True when any of the given connection states says the channel is live right now.
pub fn any_live<'a>(states: impl IntoIterator<Item = Option<&'a ConnFrame>>) -> bool {
    states.into_iter().any(|st| st.map(|s| s.live == Some(true)).unwrap_or(false))
}

enum Dropped {
    Tab,
    Split(String, Option<Vec<String>>),
}

/// `ui.measure`: the width a piece of text takes in Segoe UI at `size`.
fn text_width(window: &Window, s: &str, size: f32) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    let run = TextRun { len: s.len(), font: font(theme::FONT), color: gpui::black(), background_color: None, underline: None, strikethrough: None };
    f32::from(window.text_system().shape_line(SharedString::from(s.to_string()), px(size), &[run], None).width)
}

impl AguApp {
    /// A tab's width as the DOM would lay it out: border, padding, the unread dot, the title (ellipsised at
    /// the 220px max) and the close button of the selected tab.
    fn tab_width(&self, window: &Window, t: &Tab, selected: bool, live_tabs: &HashSet<String>) -> f32 {
        let mark = tab_dot(selected, self.unread.get(&t.id).copied(), live_tabs.contains(&t.id)).is_some();
        let title = text_width(window, &tab_title(t, false), 12.0);
        let mut w = 1.0 + 20.0 + title;
        if mark {
            w += 6.0 + 4.0;
        }
        if whisper_only(t) {
            w += 12.0 + 4.0;
        }
        if selected && self.tabs.editing.as_deref() != Some(&t.id) {
            w += 4.0 + 16.0;
        }
        if self.tabs.editing.as_deref() == Some(&t.id) {
            w = 1.0 + 20.0 + 128.0;
        }
        w.min(220.0).ceil()
    }

    pub fn start_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(t) = self.layout.tabs.iter().find(|t| t.id == id).cloned() else { return };
        let placeholder = auto_title(&t);
        let input = cx.new(|cx| TextInput::new(window, cx, &placeholder, InputStyle::Rename));
        input.update(cx, |i, cx| {
            i.set_text(&t.title, cx);
            i.select_all_text(cx);
            i.focus(window);
        });
        let sub = cx.subscribe_in(&input, window, |this, _i, ev: &TextInputEvent, window, cx| match ev {
            TextInputEvent::Enter | TextInputEvent::Blur => this.commit_rename(window, cx),
            TextInputEvent::Escape => {
                this.tabs.editing = None;
                this.tabs.input = None;
                window.focus(&this.focus);
                cx.notify();
            }
            TextInputEvent::Changed | TextInputEvent::Tab => {}
        });
        self.tabs.editing = Some(id.to_string());
        self.tabs.input = Some((input, sub));
        cx.notify();
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.tabs.editing.take() else { return };
        let draft = self.tabs.input.take().map(|(i, _)| i.read(cx).text()).unwrap_or_default();
        window.focus(&self.focus);
        self.rename_tab(&id, draft.trim().to_string(), cx);
    }

    /// Opens a popup menu below its opener, or closes it when that opener's menu is already showing.
    pub fn toggle_menu(&mut self, key: &str, anchor: Option<Bounds<Pixels>>, align_right: bool, entries: Vec<MenuEntry>, cx: &mut Context<Self>) {
        if self.menu.as_ref().map(|m| m.key == key).unwrap_or(false) {
            self.menu = None;
        } else if let Some(b) = anchor {
            self.menu = Some(MenuState { key: key.into(), pos: if align_right { b.bottom_right() } else { b.bottom_left() }, align_right, entries, anchor: Some(b) });
        }
        cx.notify();
    }

    fn tab_menu_entries(&self, id: &str, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let _ = cx;
        let active = self.layout.active.clone();
        let n = self.layout.tabs.len();
        let (id_a, id_b, id_c) = (id.to_string(), id.to_string(), id.to_string());
        vec![
            MenuEntry::Item(MenuItem::new("Rename tab", move |this, w, cx| this.start_rename(&id_a, w, cx))),
            // Only the selected tab can be closed, so a stray click never closes one you aren't looking at.
            MenuEntry::Item(
                MenuItem::new("Close tab", move |this, _w, cx| this.close_tab(&id_b, cx))
                    .hint(hotkey_hint(&self.hotkey_table, HotkeyAction::CloseTab))
                    .disabled(id != active),
            ),
            MenuEntry::Item(MenuItem::new("Close other tabs", move |this, _w, cx| this.close_others(&id_c, cx)).disabled(n < 2)),
        ]
    }

    fn bar_drag_over(&mut self, split: bool, bounds: Bounds<Pixels>, pos: Point<Pixels>, at: usize, cx: &mut Context<Self>) {
        if !bounds.contains(&pos) {
            return;
        }
        self.tabs.drop_at = Some(at);
        if split {
            self.tabs.split_over = true;
        }
        cx.notify();
    }

    fn bar_drop(&mut self, what: Dropped, cx: &mut Context<Self>) {
        let at = self.tabs.drop_at.take();
        self.tabs.split_over = false;
        let end = self.layout.tabs.len();
        let at = at.unwrap_or(end);
        match what {
            Dropped::Tab => {
                if let Some(id) = self.tabs.drag_id.take() {
                    self.reorder(&id, at, cx);
                }
            }
            Dropped::Split(leaf, keys) => self.from_split(&leaf, keys.as_deref(), at, cx),
        }
        cx.notify();
    }

    /// The drop targets of one spot in the bar: the insertion index comes from `at(bounds, pointer)`.
    fn drop_targets<E: InteractiveElement>(&self, el: E, cx: &mut Context<Self>, at: impl Fn(&Bounds<Pixels>, &Point<Pixels>) -> usize + Copy + 'static) -> E {
        el.on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<TabDrag>, _w, cx| {
            this.bar_drag_over(false, ev.bounds, ev.event.position, at(&ev.bounds, &ev.event.position), cx)
        }))
        .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<SplitDrag>, _w, cx| {
            this.bar_drag_over(true, ev.bounds, ev.event.position, at(&ev.bounds, &ev.event.position), cx)
        }))
        .on_drag_move(cx.listener(move |this, ev: &DragMoveEvent<SourcesDrag>, _w, cx| {
            this.bar_drag_over(true, ev.bounds, ev.event.position, at(&ev.bounds, &ev.event.position), cx)
        }))
        .on_drop(cx.listener(|this, _: &TabDrag, _w, cx| this.bar_drop(Dropped::Tab, cx)))
        .on_drop(cx.listener(|this, d: &SplitDrag, _w, cx| this.bar_drop(Dropped::Split(d.leaf_id.clone(), None), cx)))
        .on_drop(cx.listener(|this, d: &SourcesDrag, _w, cx| this.bar_drop(Dropped::Split(d.leaf_id.clone(), Some(d.keys.clone())), cx)))
    }

    pub fn render_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let tabs = self.layout.tabs.clone();
        let active_id = self.layout.active.clone();
        let n = tabs.len();
        let active_idx = tabs.iter().position(|t| t.id == active_id).unwrap_or(0);

        // tabs with a live channel in any split, from the panes' connection state (a Status event re-renders the app)
        let live_tabs: HashSet<String> = tabs
            .iter()
            .filter(|t| {
                leaves(t.root.as_ref()).iter().any(|l| match self.panes.get(&l.id) {
                    Some(slot) => {
                        let status = &slot.view.read(cx).status;
                        any_live(l.sources.iter().map(|s| status.get(&source_key(s))))
                    }
                    None => false,
                })
            })
            .map(|t| t.id.clone())
            .collect();
        let dot_of = |t: &Tab| tab_dot(t.id == active_id, self.unread.get(&t.id).copied(), live_tabs.contains(&t.id));

        // ---- which tabs fit ----
        let widths: Vec<f32> = tabs.iter().map(|t| self.tab_width(window, t, t.id == active_id, &live_tabs)).collect();
        let vw: f32 = window.viewport_size().width.into();
        let zone_w = self.anchor("tabs-zone").map(|b| f32::from(b.size.width)).unwrap_or(vw - 28.0 - 3.0 * 36.0);
        let full = zone_w - PLUS_W;
        let total: f32 = widths.iter().sum();
        let (start, end) = if total <= full {
            (0usize, n as isize - 1)
        } else {
            let r = tab_window(&widths, full - ALL_W, active_idx, self.tabs.start);
            (r.start, r.end)
        };
        self.tabs.start = start;
        let shown = |i: usize| i >= start && (i as isize) <= end;
        let hidden: Vec<&Tab> = tabs.iter().enumerate().filter(|(i, _)| !shown(*i)).map(|(_, t)| t).collect();
        let hidden_mark = hidden.iter().filter_map(|t| dot_of(t)).max();

        let drag_live = cx.has_active_drag();
        let dragging = self.tabs.drag_id.is_some() || self.tabs.split_over;
        let drop_end = (end.min(n as isize - 1) + 1).max(0) as usize; // dropping on the empty bar: after the last shown tab
        let entity = cx.entity();

        // ---- chevron: every tab ----
        let menu_hints: std::collections::HashMap<String, &'static str> = tabs
            .iter()
            .filter_map(|t| {
                let dot = dot_of(t)?;
                Some((t.id.clone(), dot_hint(dot)))
            })
            .collect();
        let all_tabs = (!hidden.is_empty()).then(|| {
            let n_hidden = hidden.len();
            div()
                .id("tabs-all")
                .relative()
                .flex()
                .flex_none()
                .w(px(24.))
                .items_center()
                .justify_center()
                .border_r_1()
                .border_color(c(theme::LINE))
                .text_color(c(theme::MUTED))
                .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                .tooltip(tip(format!("All tabs ({n_hidden} more)")))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    let entries: Vec<MenuEntry> = this
                        .layout
                        .tabs
                        .iter()
                        .map(|t| {
                            let id = t.id.clone();
                            let hint = menu_hints.get(&t.id).map(|h| h.to_string());
                            MenuEntry::Item(MenuItem::new(tab_title(t, true), move |this, _w, cx| this.select_tab(&id, cx)).active(t.id == this.layout.active).hint(hint))
                        })
                        .collect();
                    let anchor = this.anchor("tabs-all");
                    this.toggle_menu("tabs-all", anchor, false, entries, cx);
                }))
                .child(icons::icon(icons::CHEVRON, 14.))
                .children(hidden_mark.map(|m| div().absolute().right(px(4.)).top(px(4.)).size(px(6.)).rounded_full().bg(c(m.color()))))
                .child(self.probe("tabs-all".into(), cx))
        });

        // ---- the tabs ----
        let mut tab_els: Vec<AnyElement> = Vec::new();
        for (i, t) in tabs.iter().enumerate() {
            if !shown(i) {
                continue;
            }
            let selected = t.id == active_id;
            let is_editing = self.tabs.editing.as_deref() == Some(&t.id);
            let mark = if selected { None } else { self.unread.get(&t.id).copied() };
            let dot = dot_of(t);
            let tid = t.id.clone();
            let label: SharedString = tab_title(t, false).into();
            let full_title: SharedString = tab_title(t, true).into();
            let is_drag_src = drag_live && self.tabs.drag_id.as_deref() == Some(&t.id);

            let mut el = div()
                .id(("tab", i))
                .relative()
                .flex()
                .flex_none()
                .max_w(px(220.))
                .items_center()
                .gap(px(4.))
                .border_r_1()
                .border_color(c(theme::LINE))
                .px(px(10.))
                .text_size(px(12.))
                .when(is_drag_src, |d| d.opacity(0.5));
            el = if selected {
                el.bg(c(theme::PANEL)).text_color(c(theme::TEXT))
            } else if mark == Some(Activity::Highlight) {
                el.bg(theme::mention()).text_color(c(theme::TEXT)).hover(|s| s.bg(c(theme::RAISED_2)))
            } else if mark == Some(Activity::Message) {
                el.text_color(c(theme::TEXT)).hover(|s| s.bg(c(theme::RAISED_2)))
            } else {
                el.text_color(c(theme::MUTED)).hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
            };

            // drag source (a tab can be reordered, or dropped onto another tab's splits)
            el = el.on_drag(TabDrag { tab_id: t.id.clone(), label: label.clone() }, {
                let entity = entity.clone();
                move |d: &TabDrag, _offset, _w, cx| {
                    let id = d.tab_id.clone();
                    entity.update(cx, |this, cx| {
                        this.tabs.drag_id = Some(id.clone());
                        this.tab_drag = Some(id);
                        cx.notify();
                    });
                    cx.new(|_| DragGhost(d.label.clone()))
                }
            });
            el = self.drop_targets(el, cx, move |b, p| if p.x < b.left() + b.size.width / 2.0 { i } else { i + 1 });

            el = el
                .on_click(cx.listener({
                    let tid = tid.clone();
                    move |this, ev: &ClickEvent, window, cx| {
                        this.select_tab(&tid, cx);
                        if ev.click_count() >= 2 {
                            this.start_rename(&tid, window, cx);
                        }
                    }
                }))
                .on_mouse_up(
                    MouseButton::Middle,
                    cx.listener({
                        let tid = tid.clone();
                        move |this, _: &MouseUpEvent, _w, cx| {
                            if this.layout.active == tid {
                                this.close_tab(&tid, cx);
                            }
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Right,
                    cx.listener({
                        let tid = tid.clone();
                        move |this, ev: &MouseUpEvent, _w, cx| {
                            let entries = this.tab_menu_entries(&tid, cx);
                            this.menu = Some(MenuState { key: format!("tab-ctx:{tid}"), pos: ev.position, align_right: false, entries, anchor: None });
                            cx.notify();
                        }
                    }),
                );

            // contents
            if is_editing {
                if let Some((input, _)) = &self.tabs.input {
                    el = el
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
                        .child(input.clone());
                }
            } else {
                if let Some(d) = dot {
                    el = el.child(div().size(px(6.)).flex_none().rounded_full().bg(c(d.color())));
                }
                if whisper_only(t) {
                    el = el.child(div().flex_none().child(icons::icon(icons::MAIL, 12.).color(c(theme::platform_color(agu_chat_core::protocol::Platform::Twitch)))));
                }
                el = el.child(div().id(("tab-title", i)).min_w_0().truncate().tooltip(tip(full_title)).child(label));
            }
            if selected && !is_editing {
                el = el.child(
                    div()
                        .id(("tab-close", i))
                        .flex()
                        .size(px(16.))
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .text_color(c(theme::FAINT))
                        .hover(|s| s.bg(c(theme::LINE)).text_color(c(theme::TEXT)))
                        .tooltip(tip("Close tab"))
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
                        .on_click(cx.listener({
                            let tid = tid.clone();
                            move |this, _: &ClickEvent, _w, cx| {
                                cx.stop_propagation();
                                this.close_tab(&tid, cx);
                            }
                        }))
                        .child(icons::icon(icons::X, 12.)),
                );
                el = el.child(div().absolute().bottom_0().left_0().right_0().h(px(2.)).bg(c(theme::ACCENT)));
            } else if selected {
                el = el.child(div().absolute().bottom_0().left_0().right_0().h(px(2.)).bg(c(theme::ACCENT)));
            }
            if mark == Some(Activity::Highlight) {
                el = el.child(div().absolute().top_0().left_0().right_0().h(px(2.)).bg(c(theme::ACCENT_SOFT)));
            }
            if dragging && self.tabs.drop_at == Some(i) {
                el = el.child(div().absolute().top_0().bottom_0().left_0().w(px(2.)).bg(c(theme::ACCENT)));
            }
            if dragging && self.tabs.drop_at == Some(i + 1) && i + 1 == drop_end {
                el = el.child(div().absolute().top_0().bottom_0().right_0().w(px(2.)).bg(c(theme::ACCENT)));
            }
            tab_els.push(el.into_any_element());
        }

        // ---- new tab ----
        let new_tab_hint = hotkey_hint(&self.hotkey_table, HotkeyAction::NewTab);
        let plus = div()
            .id("tabs-plus")
            .flex()
            .flex_none()
            .w(px(PLUS_W))
            .items_center()
            .justify_center()
            .text_size(px(16.))
            .line_height(gpui::relative(1.))
            .text_color(c(theme::MUTED))
            .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
            .tooltip(tip(match new_tab_hint {
                Some(k) => format!("New tab ({k})"),
                None => "New tab".to_string(),
            }))
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.add_tab(cx)))
            .child("+");

        // ---- the empty bar after the tabs: grab the window, or drop a split here ----
        let grab = div()
            .id("tabs-empty")
            .min_w_0()
            .flex_1()
            .h_full()
            .when(!drag_live, |d| d.window_control_area(WindowControlArea::Drag).occlude())
            .when(cfg!(target_os = "macos") && !drag_live, |d| d.on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, w, _| macwin::titlebar_press(ev, w)));
        let grab = self.drop_targets(grab, cx, move |_, _| drop_end);

        let zone = div()
            .id("tabs-zone")
            .relative()
            .flex()
            .min_w_0()
            .flex_1()
            
            .children(all_tabs)
            .child(div().relative().flex().min_w_0().overflow_hidden().children(tab_els))
            .child(plus)
            .child(grab)
            .child(self.probe("tabs-zone".into(), cx));

        // ---- window buttons ----
        let maximized = window.is_maximized();
        let win_btn = |id: &'static str, area: WindowControlArea, ico: &'static str, size: f32, danger: bool| {
            div()
                .id(id)
                .flex()
                .w(px(36.))
                .flex_none()
                .items_center()
                .justify_center()
                .text_color(c(theme::MUTED))
                .hover(move |s| if danger { s.bg(c(theme::RED_600)).text_color(c(theme::WHITE)) } else { s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)) })
                .window_control_area(area)
                // GPUI on Windows first offers a title-bar press to the app as a MouseDown; any focusable
                // ancestor (the root tracks focus) then calls prevent_default, GPUI reports the press as
                // handled and Windows never moves/minimizes/closes. Blocking the hitbox keeps the press
                // away from those ancestors (the control area itself still hit-tests: it is the top hitbox).
                .occlude()
                .child(icons::icon(ico, size))
        };

        div()
            .id("tabs-bar")
            .flex()
            .flex_none()
            .h(px(28.))
            
            .border_b_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            // Whenever the pointer is in the bar during a drag, the per-tab targets set the mark again.
            .on_drag_move(cx.listener(|this, _: &DragMoveEvent<TabDrag>, _w, cx| this.clear_bar_mark(cx)))
            .on_drag_move(cx.listener(|this, _: &DragMoveEvent<SplitDrag>, _w, cx| this.clear_bar_mark(cx)))
            .on_drag_move(cx.listener(|this, _: &DragMoveEvent<SourcesDrag>, _w, cx| this.clear_bar_mark(cx)))
            // macOS: room for the native traffic lights (which are the window buttons there); the strip drags too
            .when(cfg!(target_os = "macos"), |d| {
                d.child(div().w(px(macwin::TRAFFIC_INSET)).flex_none().h_full().on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, w, _| macwin::titlebar_press(ev, w)))
            })
            .child(zone)
            // Always some bare title bar to grab the window by, however many tabs there are.
            .child(
                div()
                    .w(px(28.))
                    .flex_none()
                    .h_full()
                    .when(!drag_live, |d| d.window_control_area(WindowControlArea::Drag).occlude())
                    .when(cfg!(target_os = "macos") && !drag_live, |d| d.on_mouse_down(MouseButton::Left, |ev: &MouseDownEvent, w, _| macwin::titlebar_press(ev, w))),
            )
            // the traffic lights replace the minimize / maximize / close buttons on macOS
            .when(!cfg!(target_os = "macos"), |d| {
                d.child(win_btn("win-min", WindowControlArea::Min, icons::WIN_MIN, 14., false))
                    .child(if maximized {
                        win_btn("win-max", WindowControlArea::Max, icons::WIN_RESTORE, 14., false)
                    } else {
                        win_btn("win-max", WindowControlArea::Max, icons::WIN_MAX, 12., false)
                    })
                    .child(win_btn("win-close", WindowControlArea::Close, icons::X, 14., true))
            })
            .into_any_element()
    }

    fn clear_bar_mark(&mut self, cx: &mut Context<Self>) {
        if self.tabs.drop_at.is_some() || self.tabs.split_over {
            cx.notify();
        }
        self.tabs.drop_at = None;
        self.tabs.split_over = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(live: Option<bool>) -> ConnFrame {
        serde_json::from_value(serde_json::json!({"state": "open", "live": live})).unwrap()
    }

    #[test]
    fn priority_is_mention_then_live_then_new() {
        let (m, h) = (Some(Activity::Message), Some(Activity::Highlight));
        assert_eq!(tab_dot(false, h, true), Some(TabDot::Highlight));
        assert_eq!(tab_dot(false, h, false), Some(TabDot::Highlight));
        assert_eq!(tab_dot(false, m, true), Some(TabDot::Live));
        assert_eq!(tab_dot(false, m, false), Some(TabDot::Message));
        assert_eq!(tab_dot(false, None, true), Some(TabDot::Live));
        assert_eq!(tab_dot(false, None, false), None);
    }

    #[test]
    fn selected_tab_shows_only_live() {
        assert_eq!(tab_dot(true, Some(Activity::Highlight), true), Some(TabDot::Live));
        assert_eq!(tab_dot(true, Some(Activity::Message), false), None);
        assert_eq!(tab_dot(true, None, false), None);
    }

    #[test]
    fn hidden_mark_takes_the_strongest() {
        let dots = [Some(TabDot::Message), None, Some(TabDot::Live), Some(TabDot::Message)];
        assert_eq!(dots.iter().filter_map(|d| *d).max(), Some(TabDot::Live));
        let dots = [Some(TabDot::Live), Some(TabDot::Highlight)];
        assert_eq!(dots.iter().filter_map(|d| *d).max(), Some(TabDot::Highlight));
    }

    #[test]
    fn live_aggregates_over_any_source() {
        let (on, off) = (conn(Some(true)), conn(Some(false)));
        let unknown = conn(None);
        assert!(any_live([Some(&off), Some(&on)]));
        assert!(any_live([None, Some(&on)]));
        assert!(!any_live([Some(&off), Some(&unknown), None]));
        assert!(!any_live(std::iter::empty::<Option<&ConnFrame>>()));
    }
}

#[allow(dead_code)]
fn _keep(_: gpui::Rgba) {
    let _ = a;
}
