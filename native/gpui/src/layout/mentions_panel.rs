//! The Mentions bell and panel (DankChat's mentions sheet): the bell in the title bar turns red with a count
//! when a message mentions you in any open chat; it opens a panel listing every such message, oldest at the top,
//! drawn like the chat list draws them. Opening it marks them read (the tabs keep their own red dots until
//! viewed); a click on an entry jumps to the message in its chat.
//!
//! The rows are drawn by the pane that has their chat (`ChatPane::mention_row_els`), so names open the user
//! card and the hover tools act there.
use std::collections::HashMap;

use gpui::{div, prelude::*, px, relative, AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent, SharedString, Window};

use crate::chat::chat_pane::ChatPane;
use crate::chat::mentions::{bell_count_text, MentionEntry};
use crate::chat::types::{RowBody, RowRef};
use crate::chat::ui_util::fmt_time;
use crate::icons;
use crate::layout::app::AguApp;
use crate::layout::tree::{is_whisper_key, leaves, short_name, source_key};
use crate::layout::ui::tip;
use crate::theme::{self, c};

/// The bell button's width in the title bar.
pub const BELL_W: f32 = 32.0;
const BELL_ANCHOR: &str = "mentions-bell";

/// The author and text of a row, for an entry whose chat is no longer open anywhere.
fn plain_line(r: &RowRef) -> (String, String) {
    match &r.body {
        RowBody::Msg(m) => (m.user.name.clone(), m.text.clone()),
        RowBody::Rumble(m) => (m.user.name.clone(), m.text.clone()),
        RowBody::Kick(m) => (if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() }, m.text.clone()),
        RowBody::YouTube(m) => (m.user.name.clone(), m.text.clone()),
        RowBody::X(m) => (m.user.name.clone(), m.text.clone()),
        RowBody::Notice(n) => (n.user.name.clone(), n.system.clone()),
        _ => (String::new(), String::new()),
    }
}

impl AguApp {
    // ---- the collector ----------------------------------------------------------------------------------

    /// A pane reported messages that mention the user (the same ones that turn its tab's dot red).
    pub(crate) fn on_mentions(&mut self, leaf_id: &str, rows: &[RowRef], cx: &mut Context<Self>) {
        let leaf = self.layout.tabs.iter().flat_map(|t| leaves(t.root.as_ref())).find(|l| l.id == leaf_id);
        let mut added = false;
        for r in rows {
            let channel = leaf.as_ref().and_then(|l| l.sources.iter().find(|s| source_key(s) == r.sk)).map(short_name).unwrap_or_else(|| r.sk.split_once(':').map(|(_, c)| c.to_string()).unwrap_or_default());
            added |= self.mentions.add(leaf_id, &channel, r.clone(), self.mentions_open);
        }
        if added {
            cx.notify();
        }
    }

    pub(crate) fn on_mentions_gone(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if self.mentions.remove(ids) {
            cx.notify();
        }
    }

    // ---- open / close ----------------------------------------------------------------------------------

    pub fn toggle_mentions(&mut self, cx: &mut Context<Self>) {
        if self.mentions_open {
            self.close_mentions(cx);
        } else {
            self.menu = None;
            self.mentions_open = true;
            self.mentions.mark_all_read();
            // the newest at the bottom, in view
            self.mentions_scroll.scroll_to_bottom();
            self.mentions_count = self.mentions.len();
            cx.notify();
        }
    }

    pub fn close_mentions(&mut self, cx: &mut Context<Self>) {
        if !self.mentions_open {
            return;
        }
        self.mentions_open = false;
        for slot in self.panes.values() {
            slot.view.update(cx, |p, _| p.forget_mention_rows());
        }
        cx.notify();
    }

    // ---- which split draws / receives an entry ------------------------------------------------------------

    /// The split showing the entry's chat: the one it came in on while that still shows it, else any split (in
    /// any tab) that does. With the tab it is in.
    fn split_for(&self, e: &MentionEntry, cx: &gpui::App) -> Option<(String, String)> {
        let has = |leaf_id: &str| self.panes.get(leaf_id).is_some_and(|s| s.view.read(cx).has_source(&e.row.sk));
        let mut first: Option<(String, String)> = None;
        for t in &self.layout.tabs {
            for l in leaves(t.root.as_ref()) {
                if !has(&l.id) {
                    continue;
                }
                if l.id == e.leaf_id {
                    return Some((t.id.clone(), l.id));
                }
                first.get_or_insert((t.id.clone(), l.id));
            }
        }
        first
    }

    /// Show the tab holding `leaf_id` with that split focused, and close the panel.
    pub(crate) fn reveal_leaf(&mut self, leaf_id: &str, cx: &mut Context<Self>) {
        let Some(tab_id) = self.layout.tabs.iter().find(|t| leaves(t.root.as_ref()).iter().any(|l| l.id == leaf_id)).map(|t| t.id.clone()) else { return };
        self.close_mentions(cx);
        self.select_tab(&tab_id, cx);
        self.focus_leaf(&tab_id, leaf_id, cx);
    }

    /// A click on an entry: its tab, its split, the list scrolled to the message (with a short flash).
    fn jump_to_mention(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(e) = self.mentions.get(id).cloned() else { return };
        match self.split_for(&e, cx) {
            Some((_, leaf_id)) => {
                self.reveal_leaf(&leaf_id, cx);
                if let Some(slot) = self.panes.get(&leaf_id) {
                    let key = e.row.key.clone();
                    slot.view.update(cx, |p, cx| p.jump_to(&key, cx));
                }
            }
            None => {
                self.close_mentions(cx);
                let tab = self.active_tab().clone();
                if let Some(slot) = self.focused_of(&tab).and_then(|id| self.panes.get(&id)) {
                    slot.view.update(cx, |p, cx| p.notify_toast("That chat is no longer open", cx));
                }
            }
        }
    }

    // ---- the bell -------------------------------------------------------------------------------------

    pub(crate) fn render_bell(&self, cx: &mut Context<Self>) -> AnyElement {
        let count = bell_count_text(self.mentions.unread());
        let open = self.mentions_open;
        let mut bell = div()
            .id(BELL_ANCHOR)
            .relative()
            .flex()
            .flex_none()
            .w(px(BELL_W))
            .h_full()
            .items_center()
            .justify_center()
            .tooltip(tip("Mentions"))
            // the panel's click-outside must not see the press that toggles it
            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.toggle_mentions(cx)));
        bell = match (&count, open) {
            (Some(_), _) => bell.text_color(c(theme::RED_500)).hover(|s| s.bg(c(theme::RAISED_2))).child(icons::icon(icons::BELL_FILL, 15.)),
            (None, true) => bell.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)).child(icons::icon(icons::BELL, 15.)),
            (None, false) => bell.text_color(c(theme::MUTED)).hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT))).child(icons::icon(icons::BELL, 15.)),
        };
        if let Some(t) = count {
            bell = bell.child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(1.))
                    .min_w(px(14.))
                    .h(px(14.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(c(theme::RED_500))
                    .text_color(c(theme::WHITE))
                    .text_size(px(9.))
                    .line_height(px(14.))
                    .font_weight(FontWeight::BOLD)
                    .child(SharedString::from(t)),
            );
        }
        bell.child(self.probe(BELL_ANCHOR.into(), cx)).into_any_element()
    }

    // ---- the panel ------------------------------------------------------------------------------------

    /// The open panel, over the top right of the splits. Esc, the close button or a click outside closes it.
    pub(crate) fn render_mentions_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.mentions_open {
            return None;
        }
        let entries: Vec<MentionEntry> = self.mentions.entries().iter().cloned().collect();
        // Every entry arriving while the panel is open is read at once; the newest comes into view.
        if entries.len() > self.mentions_count {
            self.mentions_scroll.scroll_to_bottom();
        }
        self.mentions_count = entries.len();

        // the rows, drawn by the split showing their chat (one call per split)
        let mut by_leaf: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, e) in entries.iter().enumerate() {
            if let Some((_, leaf)) = self.split_for(e, cx) {
                by_leaf.entry(leaf).or_default().push(i);
            }
        }
        let mut els: Vec<Option<Option<AnyElement>>> = (0..entries.len()).map(|_| None).collect();
        let active: Vec<String> = leaves(self.active_tab().root.as_ref()).into_iter().map(|l| l.id).collect();
        let mut tips: Vec<AnyElement> = Vec::new();
        for (leaf, idx) in by_leaf {
            let Some(view) = self.panes.get(&leaf).map(|s| s.view.clone()) else { continue };
            let rows: Vec<RowRef> = idx.iter().map(|&i| entries[i].row.clone()).collect();
            let built = view.update(cx, |p: &mut ChatPane, cx| p.mention_row_els(&rows, cx));
            for (i, el) in idx.into_iter().zip(built) {
                els[i] = Some(el);
            }
            // a split that isn't on screen can't draw its emote tooltips: they go with the panel
            if !active.contains(&leaf) {
                if let Some(t) = view.read(cx).tip_overlay(window) {
                    tips.push(t);
                }
            }
        }

        let mut list = div().id("mentions-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&self.mentions_scroll).flex().flex_col().py(px(4.));
        let mut shown = 0usize;
        for (i, (e, el)) in entries.iter().zip(els).enumerate() {
            let body: AnyElement = match el {
                Some(Some(el)) => el,
                // drawn nowhere now: deleted, or its author ignored since
                Some(None) => continue,
                // its chat is no longer open: a plain line
                None => {
                    let (name, text) = plain_line(&e.row);
                    div()
                        .px(px(8.))
                        .py(px(1.))
                        .text_size(px(self.settings.font_size))
                        .child(div().child(SharedString::from(format!("{name}: {text}"))))
                        .into_any_element()
                }
            };
            shown += 1;
            let whisper = is_whisper_key(&e.row.sk);
            let mark = if whisper {
                icons::icon(icons::MAIL, 11.).color(c(theme::platform_color(e.row.platform)))
            } else {
                icons::platform_icon(e.row.platform, 11., true)
            };
            let channel = if whisper { format!("Whisper: {}", e.channel) } else { e.channel.clone() };
            let header = div()
                .flex()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .pt(px(4.))
                .text_size(px(11.))
                .line_height(px(14.))
                .child(div().flex_none().child(mark))
                .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(c(theme::MUTED)).child(SharedString::from(channel)))
                .child(div().flex_none().text_color(c(theme::FAINT)).child(SharedString::from(fmt_time(e.row.ts, false))));
            let id = e.id.clone();
            list = list.child(
                div()
                    .id(("mention-entry", i))
                    .flex()
                    .flex_col()
                    .pb(px(2.))
                    .border_b_1()
                    .border_color(c(theme::LINE))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::a(theme::RAISED_2, 0.5)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| this.jump_to_mention(&id, cx)))
                    .child(header)
                    .child(body),
            );
        }
        if shown == 0 {
            list = list.child(div().w_full().px(px(16.)).py(px(40.)).flex().justify_center().text_size(px(12.)).text_color(c(theme::FAINT)).child("No mentions yet"));
        }

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(c(theme::LINE))
            .text_size(px(12.))
            .line_height(px(16.))
            .child(div().flex_none().text_color(c(theme::MUTED)).child(icons::icon(icons::BELL, 14.)))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child("Mentions"))
            .child(div().flex_1().text_color(c(theme::FAINT)).child(SharedString::from(match shown {
                0 => String::new(),
                1 => "1 message".to_string(),
                n => format!("{n} messages"),
            })))
            .child(
                div()
                    .id("mentions-close")
                    .flex_none()
                    .p(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(c(theme::MUTED))
                    .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                    .tooltip(tip("Close"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.close_mentions(cx)))
                    .child(icons::icon(icons::X, 14.)),
            );

        let vw: f32 = window.viewport_size().width.into();
        let bell = self.anchor(BELL_ANCHOR);
        Some(
            div()
                .id("mentions-panel")
                .occlude()
                .absolute()
                .top(px(4.))
                .right(px(8.))
                .w(px(440f32.min(vw - 16.0).max(200.0)))
                .h(relative(0.8))
                .flex()
                .flex_col()
                .rounded(px(6.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::PANEL))
                .text_color(c(theme::TEXT))
                .shadow_2xl()
                .on_mouse_down_out(cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    // the bell toggles it itself
                    if bell.is_some_and(|b| b.contains(&ev.position)) {
                        return;
                    }
                    this.close_mentions(cx);
                }))
                .child(header)
                .child(list)
                .children(tips)
                .into_any_element(),
        )
    }
}
