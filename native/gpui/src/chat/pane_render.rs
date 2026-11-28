//! The visual half of ChatPane.tsx / MessageList.tsx / RowFrame.tsx / PinnedCard.tsx / toast.tsx / emoteTip.tsx
//! and the logged-out `SendBox`: turns the pane's state and its `BuiltRow`s into GPUI elements each frame.
use std::rc::Rc;

use gpui::AnimationExt as _;
use gpui::{
    anchored, deferred, div, point, prelude::*, px, relative, size, AnyElement, App, Bounds, ClickEvent, Context, Corner, Div, Element, ElementId, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Render, SharedString, Style, Window,
};

use super::chat_list::chat_list;
use super::chat_pane::*;
use super::PxF;
use super::images::{ImagesHandle};
use super::inline::*;
use super::rows::*;
use super::twitch::colors::name_color;
use super::twitch::parts::build_parts;
use super::types::*;
use super::ui_util::*;
use crate::icons::{self, icon};
use crate::text_input::{InputStyle, TextInput, TextInputEvent};

/// Height of a row's corner tag line (FIRST MESSAGE, ...): the tag sits at top 2px with a 12px line, so the
/// message starts below it (the row's own 1px top padding + this).
pub(super) const LABEL_LINE: f32 = 13.0;
use crate::theme::{self, c};

/// the gap between a sub / gift notice's icon and its lines
pub(super) const NOTICE_ICON_GAP: f32 = 8.0;
/// the bar on sub / gift notices: the app's accent
const NOTICE_SUB_BAR: u32 = theme::ACCENT;
/// a notice's Reply button: its height and the space above it
pub(super) const NOTICE_BUTTON_H: f32 = 22.0;
pub(super) const NOTICE_BUTTON_GAP: f32 = 4.0;

fn eid(prefix: &str, key: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("{prefix}{key}")))
}

/// `toLocaleString()` for an integer-ish amount.
pub(super) fn thousands(n: f64) -> String {
    let neg = n < 0.0;
    let s = format!("{}", n.abs().round() as i64);
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if neg { format!("-{out}") } else { out }
}

fn bits_icon(amount: f64) -> (&'static str, u32) {
    if amount >= 10000.0 {
        (icons::BITS_10000, 0xf43021)
    } else if amount >= 5000.0 {
        (icons::BITS_5000, 0x0099fe)
    } else if amount >= 1000.0 {
        (icons::BITS_1000, 0x1db2a5)
    } else if amount >= 100.0 {
        (icons::BITS_100, 0x9c3ee8)
    } else {
        (icons::BITS_0, 0x979797)
    }
}

/// An image drawn from the shared cache into a fixed square with `object-fit: contain`.
pub struct ImageBox {
    pub url: SharedString,
    pub side: f32,
    /// corner radius (an avatar is `rounded-full`)
    pub radius: f32,
    /// drawn (if already loaded) while `url` is not ready: the row's small emote under the popup's big one
    pub fallback: Option<SharedString>,
}

impl IntoElement for ImageBox {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ImageBox {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size = size(px(self.side).into(), px(self.side).into());
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, _: &mut (), _: &mut Window, _: &mut App) {}
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, _: &mut (), _: &mut (), window: &mut Window, cx: &mut App) {
        let imgs = cx.global::<ImagesHandle>().clone();
        let img = imgs.0.get(&self.url, self.side, self.side, window.scale_factor()).or_else(|| self.fallback.as_ref().and_then(|f| imgs.0.ready(f)));
        if let Some(img) = img {
            // a `#square` URL holds the middle square of the picture (images.rs), whatever its natural size
            let (nw, nh) = if self.url.ends_with(super::images::SQUARE) { (1.0, 1.0) } else { (img.w as f32, img.h as f32) };
            let s = (self.side / nw).min(self.side / nh);
            let (w, h) = (nw * s, nh * s);
            let r = Bounds::new(point(bounds.origin.x + px((self.side - w) / 2.0), bounds.origin.y + px((self.side - h) / 2.0)), size(px(w), px(h)));
            let idx = imgs.0.frame(&img, window, cx);
            let _ = window.paint_image(r, gpui::Corners::all(px(self.radius)), img.gpu.clone(), idx, false);
        }
    }
}

impl ChatPane {
    // ---- inline flow with this pane's handlers ---------------------------------------------------------

    pub(super) fn flow(&self, content: &Rc<InlineContent>, actions: Rc<Vec<RowAction>>, cx: &mut Context<Self>) -> InlineFlow {
        let weak = cx.entity().downgrade();
        let w2 = weak.clone();
        let on_act: OnAct = Rc::new(move |act, rect, w, cx| {
            let wb = w.bounds();
            let _ = weak.update(cx, |p, _| p.last_act = Some((rect, wb)));
            match act.clone() {
            Act::Url(u) => {
                let _ = weak.update(cx, |p, _| p.open_url(&u));
            }
            Act::Tag(t) => {
                if let Some(a) = actions.get(t as usize).cloned() {
                    // Ctrl+click on a name: @name at the start of the message box instead of the user card.
                    if let (RowAction::Card { sk, user }, true) = (&a, crate::text_input::primary_held(&w.modifiers())) {
                        let (sk, user) = (sk.clone(), user.clone());
                        let _ = weak.update(cx, |p, cx| p.mention_user(Some(&sk), &user, w, cx));
                        return;
                    }
                    let _ = weak.update(cx, |p, cx| p.run_action(a, cx));
                }
            }
        }
        });
        let on_tip: OnTip = Rc::new(move |tip, _w, cx| {
            let _ = w2.update(cx, |p, cx| {
                p.tip = tip;
                cx.notify();
            });
        });
        let f = inline_flow(content.clone()).on_act(on_act).on_tip(on_tip, self.tip_state.clone());
        // inside a list row: selectable there, numbered in the order the row draws its flows
        let sel = self.sel_ctx.borrow_mut().as_mut().map(|(scope, row, sub)| {
            let key = super::select::FlowKey { row: *row, sub: *sub };
            *sub += 1;
            (scope.clone(), key)
        });
        match sel {
            Some((scope, key)) => f.selectable(scope, key),
            None => f,
        }
    }

    /// Draw a list row with its flows selectable in `scope` as row `row`.
    pub(super) fn with_sel_row<R>(&mut self, scope: &Rc<super::select::SelScope>, row: usize, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.sel_ctx.replace(Some((scope.clone(), row as u32, 0)));
        let r = f(self);
        *self.sel_ctx.borrow_mut() = prev;
        r
    }

    // ---- rows --------------------------------------------------------------------------------------------

    fn toolbar_button(&self, id: String, icon_path: &'static str, title: &'static str, danger: bool, cx: &mut Context<Self>, on: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        self.toolbar_button_at(id, icon_path, title, danger, cx, move |p, _pos, cx| on(p, cx))
    }

    /// Like `toolbar_button`, and also hands over where the click landed (window coordinates) for menus.
    fn toolbar_button_at(&self, id: String, icon_path: &'static str, title: &'static str, danger: bool, cx: &mut Context<Self>, on: impl Fn(&mut Self, gpui::Point<gpui::Pixels>, &mut Context<Self>) + 'static) -> AnyElement {
        let _ = title;
        div()
            .id(ElementId::from(SharedString::from(id)))
            .p(px(4.))
            .rounded(px(4.))
            .text_color(c(theme::MUTED))
            .hover(move |s| s.bg(c(theme::RAISED_2)).text_color(c(if danger { theme::RED_400 } else { theme::TEXT })))
            .child(icon(icon_path, 14.0))
            .on_click(cx.listener(move |this, e: &ClickEvent, _w, cx| {
                // not also a click on what holds the row (a Mentions panel entry jumps on click)
                cx.stop_propagation();
                on(this, e.position(), cx)
            }))
            .into_any_element()
    }

    fn frame_el(&mut self, key: &str, spec: &FrameSpec, nested: bool, cx: &mut Context<Self>) -> AnyElement {
        let base = self.settings.font_size;
        let actions = Rc::new(spec.actions.clone());
        // no hover tools while a drag selects across the rows
        let hovered = (self.hover_row.as_deref() == Some(key) || self.hover_bar.as_deref() == Some(key)) && !self.sel_main.dragging() && !self.sel_thread.dragging();
        let mut d = div().id(eid("row-", key)).relative().w_full().flex_none().py(px(1.));
        if spec.bar.is_some() {
            d = d.border_l_3().pl(px(6.)).pr(px(8.));
        } else {
            d = d.px(px(8.));
        }
        if let Some(b) = spec.border {
            d = d.border_1().border_color(b);
        }
        if let Some(bar) = spec.bar {
            d = d.border_color(bar);
        }
        if let Some(bg) = spec.bg {
            d = d.bg(bg);
        } else if let Some(t) = spec.tint_class {
            d = d.bg(t);
        } else if spec.alt {
            d = d.bg(c(theme::PANEL_ALT));
        }
        let k1 = key.to_string();
        d = d.on_hover(cx.listener(move |this, h: &bool, _w, cx| {
            if *h {
                this.hover_row = Some(k1.clone());
            } else if this.hover_row.as_deref() == Some(k1.as_str()) {
                this.hover_row = None;
            }
            cx.notify();
        }));
        // A corner tag (FIRST MESSAGE, MENTIONS YOU, ...) gets its own line above the message instead of sitting
        // over the end of the first line (a change from the Electron app, where it overlapped the text).
        if spec.label.is_some() {
            d = d.child(div().w_full().h(px(LABEL_LINE)));
        }
        if let Some(line) = &spec.before {
            // cut off at the row's width; hovering shows the whole message it answers, a click opens the thread
            let mut el = div()
                .id(eid("reply-line-", key))
                .w_full()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(px(base * 0.85))
                .line_height(px(base * 0.85 * LINE_HEIGHT))
                .text_color(c(theme::FAINT))
                .tooltip(crate::layout::ui::tip_wrap(line.full.clone(), 360.0))
                .child(SharedString::from(line.text.clone()));
            if let Some(act) = line.action() {
                let reveal = key.starts_with(super::mentions::PANEL_KEY);
                el = el.cursor_pointer().hover(|s| s.text_color(c(theme::MUTED))).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    cx.stop_propagation();
                    this.run_action(act.clone(), cx);
                    if reveal {
                        // the thread opens over this split's list: show it
                        cx.emit(super::chat_pane::PaneEvent::Reveal);
                    }
                }));
            }
            d = d.child(el);
        }
        d = d.child(self.flow(&spec.content, actions, cx));
        if let Some(card) = spec.link.as_ref().and_then(|l| self.link_card_el(key, l, cx)) {
            d = d.child(card);
        }
        if let Some((text, col)) = &spec.label {
            d = d.child(div().absolute().right(px(6.)).top(px(2.)).text_size(px(9.)).line_height(px(12.)).font_weight(FontWeight::BOLD).text_color(*col).child(tracked(text)));
        }
        if hovered {
            let mut bar = div()
                .id(eid("bar-", key))
                .absolute()
                .right(px(6.))
                .top(px(-14.))
                .flex()
                .items_center()
                .gap(px(2.))
                .rounded(px(4.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::RAISED))
                .px(px(2.))
                .py(px(2.))
                .shadow_lg();
            let k2 = key.to_string();
            bar = bar.on_hover(cx.listener(move |this, h: &bool, _w, cx| {
                if *h {
                    this.hover_bar = Some(k2.clone());
                } else if this.hover_bar.as_deref() == Some(k2.as_str()) {
                    this.hover_bar = None;
                }
                cx.notify();
            }));
            let h = &spec.hover;
            if let Some(t) = h.copy.clone() {
                bar = bar.child(self.toolbar_button(format!("copy-{key}"), icons::COPY, "Copy", false, cx, move |p, cx| p.run_action(RowAction::Copy(t.clone()), cx)));
            }
            // The user's order: copy, pin, reply. No delete here (deleting stays on the mod buttons and the
            // user card), so a stray click on the hover bar can't remove a message.
            let in_panel = key.starts_with(super::mentions::PANEL_KEY);
            if let Some((sk, id)) = h.pin.clone().filter(|_| !in_panel) {
                bar = bar.child(self.toolbar_button_at(format!("pin-{key}"), icons::PIN, "Pin", false, cx, move |p, pos, cx| {
                    // opens the "Pin for" menu at the button; choosing an entry does the pinning
                    p.pin_pick = Some(super::chat_pane::PinPick { sk: sk.clone(), id: id.clone(), pos });
                    cx.notify();
                }));
            }
            if let Some(act) = h.pin_now.clone().filter(|_| !in_panel) {
                bar = bar.child(self.toolbar_button(format!("pin-{key}"), icons::PIN, "Pin this message", false, cx, move |p, cx| p.run_action(act.clone(), cx)));
            }
            if let Some(r) = h.reply.clone() {
                bar = bar.child(self.toolbar_button(format!("reply-{key}"), icons::REPLY, "Reply", false, cx, move |p, cx| {
                    p.run_action(RowAction::Reply(r.clone()), cx);
                    if in_panel {
                        // the reply is typed in this split's message box: show it
                        cx.emit(super::chat_pane::PaneEvent::Reveal);
                    }
                }));
            }
            if let Some(r) = h.reply.clone().or_else(|| h.menu.clone()).filter(|_| self.settings.commands_button && !self.menu_commands().is_empty()) {
                bar = bar.child(self.toolbar_button_at(format!("cmds-{key}"), icons::DOTS_H, "Commands", false, cx, move |p, pos, cx| {
                    p.cmd_pick = Some(super::cmd_menu::CmdPick { target: r.clone(), pos });
                    cx.notify();
                }));
            }
            d = d.child(bar);
        }
        d.into_any_element()
    }

    /// The link preview card under a message, once its preview is in. A click opens the link; a title cut short
    /// shows whole on hover. In the message list it is drawn at the size the row's measure gave it.
    fn link_card_el(&mut self, key: &str, link: &super::link_card::LinkCard, cx: &mut Context<Self>) -> Option<AnyElement> {
        use super::link_card::*;
        let svc = self.svc.clone();
        let p = svc.previews.get(&link.target, &svc.net, cx)?;
        let base = self.settings.font_size;
        let (title_px, title_lh, sub_px, sub_lh) = fonts(base);
        let lay = link.layout.get();
        let lines = lay.map(|l| l.title_lines as usize).unwrap_or(2);
        let href = link.href.clone();
        let mut card = div()
            .id(eid("link-card-", key))
            .mt(px(MARGIN_T))
            .mb(px(MARGIN_B))
            .flex()
            .flex_row()
            .items_start()
            .gap(px(GAP))
            .p(px(PAD))
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .overflow_hidden()
            .cursor_pointer()
            .hover(|s| s.bg(c(theme::RAISED_2)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                cx.stop_propagation();
                this.open_url(&href)
            }));
        card = match lay {
            Some(l) => card.w(px(l.w)).h(px(l.h)),
            None => card.w_full().max_w(px(CARD_MAX_W)),
        };
        if lines > 1 || lay.is_none() {
            card = card.tooltip(crate::layout::ui::tip_wrap(p.title.clone(), 360.0));
        }
        let lead = match &p.thumb {
            Some(u) => Thumb { url: u.clone().into(), w: THUMB_W, h: THUMB_H }.into_any_element(),
            None => div().flex_none().text_color(c(theme::TEXT)).child(icon(provider_icon(p.provider), MARK)).into_any_element(),
        };
        let mut text = div().flex_1().min_w_0().flex().flex_col().child(
            div()
                .text_size(px(title_px))
                .line_height(px(title_lh))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(theme::TEXT))
                .line_clamp(lines)
                .text_ellipsis()
                .child(SharedString::from(p.title.clone())),
        );
        if !p.subtitle.is_empty() {
            text = text.child(div().text_size(px(sub_px)).line_height(px(sub_lh)).text_color(c(theme::MUTED)).truncate().child(SharedString::from(p.subtitle.clone())));
        }
        Some(card.child(lead).child(text).into_any_element())
    }

    fn event_el(&mut self, key: &str, e: &EventSpec, cx: &mut Context<Self>) -> AnyElement {
        let base = self.settings.font_size;
        let amount = e.amount;
        let mut head = div().flex().items_center().gap(px(8.)).px(px(8.)).pt(px(2.));
        if e.inner.is_none() || e.image.is_some() {
            head = head.pb(px(2.));
        }
        let left = div().flex_1().min_w_0().child(self.flow(&e.content, Rc::new(e.actions.clone()), cx));
        let mut right = div().flex().flex_none().items_center().gap(px(4.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::MUTED)).text_size(px(base)).line_height(px(base * LINE_HEIGHT));
        if e.is_bits {
            let (path, col) = bits_icon(amount.unwrap_or(1.0));
            right = right.child(div().text_color(c(col)).child(icon(path, base)));
        } else if amount.is_some() {
            right = right.child(div().text_color(c(theme::ACCENT_SOFT)).child(icon(icons::CHANNEL_POINTS, base)));
        }
        if let Some(a) = amount {
            right = right.child(SharedString::from(thousands(a)));
        }
        if let Some(u) = &e.image {
            // a KICKs gift's coin bundle, as kick.com draws it beside the gift
            right = right.child(ImageBox { url: SharedString::from(u.clone()), side: event_image_px(base), radius: 0.0, fallback: None });
        }
        head = head.child(left).child(right);
        // like 7TV: a coloured bar on the left and a soft tint, no box
        let mut d = div().id(eid("ev-", key)).w_full().border_l_3().border_color(token_a(theme::PURPLE_500, 0.9)).bg(token_a(theme::PURPLE_500, 0.12)).flex_none().child(head);
        if let Some(inner) = &e.inner {
            d = d.child(self.frame_el(key, inner, true, cx));
        }
        d.into_any_element()
    }

    pub(super) fn notice_lines_w(width: f32, n: &NoticeSpec) -> f32 {
        width - 3.0 - 16.0 - n.icon.map(|(_, s)| s + NOTICE_ICON_GAP).unwrap_or(0.0)
    }

    fn notice_el(&mut self, key: &str, n: &NoticeSpec, cx: &mut Context<Self>) -> AnyElement {
        // like 7TV: a coloured bar on the left, a soft tint, Twitch's sentence in plain light text
        if let Some((ic, size)) = n.icon {
            // a sub / gift, as 7TV draws it: a dark red bar, a faint lift, the icon left of the name and sentence
            let mut lines = div().flex().flex_col().flex_1().min_w_0();
            if let Some(h) = &n.head {
                let acts = n.card.clone().map(|(sk, user)| vec![RowAction::Card { sk, user }]).unwrap_or_default();
                lines = lines.child(self.flow(h, Rc::new(acts), cx));
            }
            lines = lines.child(self.flow(&n.system, Rc::new(vec![]), cx));
            if let Some(t) = &n.text {
                lines = lines.child(self.flow(t, Rc::new(vec![]), cx));
            }
            if let Some(user) = n.reply.clone() {
                let reply_sk = n.card.as_ref().map(|(sk, _)| sk.clone());
                let in_panel = key.starts_with(super::mentions::PANEL_KEY);
                lines = lines.child(
                    div().flex().mt(px(NOTICE_BUTTON_GAP)).child(
                        div()
                            .id(eid("notice-reply-", key))
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .h(px(NOTICE_BUTTON_H))
                            .px(px(8.))
                            .rounded_full()
                            .bg(gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.08 })
                            .hover(|s| s.bg(gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.14 }))
                            .cursor_pointer()
                            .text_size(px(11.))
                            .text_color(c(theme::TEXT))
                            .child(icon(icons::REPLY, 12.0))
                            .child("Reply")
                            .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| {
                                cx.stop_propagation();
                                this.mention_user(reply_sk.as_deref(), &user, w, cx);
                                if in_panel {
                                    cx.emit(super::chat_pane::PaneEvent::Reveal);
                                }
                            })),
                    ),
                );
            }
            let body = div()
                .w_full()
                .flex()
                .gap(px(NOTICE_ICON_GAP))
                .px(px(8.))
                .py(px(4.))
                .child(div().flex_none().w(px(size)).mt(px(2.)).text_color(c(theme::TEXT)).child(icon(ic, size)))
                .child(lines);
            let bar = n.bar.unwrap_or_else(|| gpui::rgb(NOTICE_SUB_BAR).into());
            return self.notice_frame(key, n, bar, gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.035 }.into(), body, cx);
        }
        let (bar, bg) = if let Some(b) = n.bar {
            // the app's own notices (7TV emote changes): their bar, a faint lift
            (b, gpui::Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.035 }.into())
        } else if n.raid {
            (token_a(theme::ORANGE_500, 0.9), token_a(theme::ORANGE_500, 0.12))
        } else {
            (token_a(theme::PURPLE_500, 0.9), token_a(theme::PURPLE_500, 0.12))
        };
        // a Kick raid's raider opens their card from the name in the sentence
        let acts = if n.head.is_none() { n.card.clone().map(|(sk, user)| vec![RowAction::Card { sk, user }]).unwrap_or_default() } else { vec![] };
        let mut body = div().w_full().px(px(8.)).py(px(4.)).child(self.flow(&n.system, Rc::new(acts), cx));
        if let Some(t) = &n.text {
            body = body.child(self.flow(t, Rc::new(vec![]), cx));
        }
        self.notice_frame(key, n, bar, bg, body, cx)
    }

    /// A notice's bar and tint around its sentence, and the attached message's chat line under it.
    fn notice_frame(&mut self, key: &str, n: &NoticeSpec, bar: gpui::Hsla, bg: gpui::Hsla, body: gpui::Div, cx: &mut Context<Self>) -> AnyElement {
        let mut d = div().id(eid("notice-", key)).w_full().flex_none().border_l_3().border_color(bar).bg(bg).child(body);
        if let Some(line) = &n.line {
            d = d.child(self.frame_el(key, line, true, cx));
        }
        d.into_any_element()
    }

    fn feed_el(&mut self, key: &str, f: &FeedSpec) -> AnyElement {
        let fs = f.base * 0.9;
        let mut d = div().id(eid("feed-", key)).w_full().flex_none().flex().gap(px(8.)).px(px(8.)).py(px(1.)).italic().text_size(px(fs)).line_height(px(fs * LINE_HEIGHT)).text_color(c(theme::SLATE_500));
        if let Some(t) = &f.time {
            d = d.child(div().flex_none().text_color(c(theme::SLATE_600)).child(SharedString::from(t.clone())));
        }
        d.child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(f.text.clone()))).into_any_element()
    }

    fn automod_el(&mut self, key: &str, a: &AutomodSpec, cx: &mut Context<Self>) -> AnyElement {
        let hs = a.base * 0.92;
        let mut d = div()
            .id(eid("automod-", key))
            .w_full()
            .flex_none()
            .rounded(px(4.))
            .py(px(4.))
            .border_l_3()
            .border_color(gpui::rgb(0xf59e0b))
            .bg(gpui::Rgba { r: 245.0 / 255.0, g: 158.0 / 255.0, b: 11.0 / 255.0, a: 0.08 });
        d = d.child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(6.))
                .px(px(8.))
                .text_size(px(hs))
                .child(div().flex_none().text_color(c(theme::AMBER_200)).child(icon(icons::WARNING, 14.0)))
                .child(div().flex_none().child(self.flow(&a.header, Rc::new(vec![]), cx).fit()))
                .child(
                    div()
                        .flex_none()
                        .rounded_full()
                        .border_1()
                        .border_color(token_a(theme::AMBER_500, 0.3))
                        .bg(token_a(theme::AMBER_500, 0.1))
                        .px(px(6.))
                        .py(px(1.))
                        .text_size(px(10.))
                        .line_height(px(14.5))
                        .text_color(c(theme::AMBER_300))
                        .child(SharedString::from(a.chip.clone())),
                ),
        );
        // the held message as a chat line: badges, a name that opens the card, emotes, the mod buttons that
        // apply to a message not in chat yet, Copy on hover
        d = d.child(self.frame_el(key, &a.msg, true, cx));
        if a.status == "held" {
            if a.can_resolve {
                let (sk1, id1, sk2, id2) = (a.sk.clone(), a.id.clone(), a.sk.clone(), a.id.clone());
                let btn = |label: &'static str, id: String, fg: u32, cx: &mut Context<Self>, action: RowAction| {
                    div()
                        .id(ElementId::from(SharedString::from(id)))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(token_a(fg_base(fg), 0.3))
                        .bg(token_a(fg_base(fg), 0.1))
                        .px(px(8.))
                        .py(px(1.))
                        .text_size(px(11.))
                        .line_height(px(15.95))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(c(fg))
                        .hover(move |s| s.border_color(token_a(fg_base(fg), 0.5)))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| this.run_action(action.clone(), cx)))
                };
                d = d.child(
                    div()
                        .mt(px(4.))
                        .px(px(8.))
                        .flex()
                        .gap(px(6.))
                        .child(btn("Allow", format!("allow-{key}"), theme::GREEN_300, cx, RowAction::Automod { sk: sk1, id: id1, allow: true }))
                        .child(btn("Deny", format!("deny-{key}"), theme::RED_300, cx, RowAction::Automod { sk: sk2, id: id2, allow: false })),
                );
            }
        } else {
            d = d.child(div().px(px(8.)).text_size(px(11.)).line_height(px(15.95)).text_color(c(theme::FAINT)).child(SharedString::from(a.resolved_line.clone())));
        }
        d.into_any_element()
    }

    pub(super) fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix >= self.shown.len() {
            return div().into_any_element();
        }
        let key = self.shown[ix].row.key.clone();
        let Some(b) = self.built(ix, cx) else { return div().into_any_element() };
        let scope = self.sel_main.clone();
        let el = self.with_sel_row(&scope, ix, |this| this.built_el(&key, &b, cx));
        if self.flash.as_deref() == Some(key.as_str()) {
            // a jump from the Mentions panel landed here: a short tint over the row (no change to its height)
            return div()
                .relative()
                .w_full()
                .child(el)
                .child(
                    div().absolute().top_0().left_0().size_full().bg(theme::a(theme::ACCENT, 0.28)).with_animation(
                        "mention-flash",
                        gpui::Animation::new(std::time::Duration::from_millis(1400)),
                        |d, t| d.opacity(1.0 - t * t),
                    ),
                )
                .into_any_element();
        }
        el
    }

    /// A row in the Mentions panel (its key prefixed, so its hover state and ids stay apart from the list's).
    pub(super) fn panel_row_el(&mut self, key: &str, b: &BuiltRow, cx: &mut Context<Self>) -> AnyElement {
        self.built_el(&format!("{}{key}", super::mentions::PANEL_KEY), b, cx)
    }

    fn built_el(&mut self, key: &str, b: &BuiltRow, cx: &mut Context<Self>) -> AnyElement {
        match b {
            BuiltRow::Frame(f) => self.frame_el(key, f, false, cx),
            BuiltRow::Event(e) => self.event_el(key, e, cx),
            BuiltRow::Notice(n) => self.notice_el(key, n, cx),
            BuiltRow::Feed(f) => self.feed_el(key, f),
            BuiltRow::Automod(a) => self.automod_el(key, a, cx),
        }
    }

    // ---- reply thread --------------------------------------------------------------------------------------

    /// The open reply thread, like DankChat's: over the message list, its first message then every reply in the
    /// order they came, drawn as the list draws them (names open cards, hover tools, mod buttons) and kept live.
    /// Esc, the close button or a click beside the sheet closes it.
    fn thread_el(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (sk, root) = {
            let t = self.thread.as_ref()?;
            (t.sk.clone(), t.root.clone())
        };
        if self.store.source(&sk).is_none() {
            self.thread = None;
            return None;
        }
        let built = self.thread_built_rows(cx);
        let has_root = super::thread::thread_rows(self.store.rows(), &sk, &root).first().and_then(|r| super::thread::reply_link(r).map(|l| l.id == root)).unwrap_or(false);
        let t = self.thread.as_mut()?;
        let first_frame = t.count == 0 && !built.is_empty();
        if built.len() > t.count {
            // a new reply (or the first frame): show the newest line
            t.scroll.scroll_to_bottom();
        }
        t.count = built.len();
        let (scroll, focus, sending) = (t.scroll.clone(), t.focus.clone(), t.sending);
        let input = if self.thread_can_reply() { self.thread_input(window, cx) } else { None };
        if input.is_some() {
            self.thread_sync_images(cx);
        }
        if first_frame {
            match &input {
                Some(i) => i.read(cx).focus(window),
                None => window.focus(&focus),
            }
        }
        let replies = built.len().saturating_sub(if has_root { 1 } else { 0 });

        // the reply box, with the message box's emote list and "@" list over it
        let reply_box = input.map(|i| {
            let mut row = div()
                .relative()
                .flex_none()
                .px(px(8.))
                .py(px(6.))
                .border_t_1()
                .border_color(c(theme::LINE))
                .when(sending, |d| d.opacity(0.6))
                .child(div().capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, w, cx| this.thread_key(ev, w, cx))).child(i));
            let entity = cx.entity();
            if let Some(t) = self.thread.as_mut() {
                if t.auto.is_none() {
                    t.auto_at = None;
                }
                if t.mentions.state.is_none() {
                    t.mention_at = None;
                }
                if let Some(a) = &t.auto {
                    if t.auto_at != Some((a.start, a.idx)) {
                        t.auto_at = Some((a.start, a.idx));
                        t.auto_scroll.scroll_to_item(a.idx);
                    }
                    let e = entity.clone();
                    let pick = move |i: usize, w: &mut Window, cx: &mut gpui::App| e.update(cx, |p, cx| p.thread_pick_auto(Some(i), w, cx));
                    row = row.child(deferred(super::input::autocomplete::autocomplete_dropdown(a, &t.auto_scroll, pick)).with_priority(1));
                }
                if let Some(m) = &t.mentions.state {
                    if t.mention_at != Some((m.start, m.idx)) {
                        t.mention_at = Some((m.start, m.idx));
                        t.mention_scroll.scroll_to_item(m.idx);
                    }
                    let e = entity.clone();
                    let pick = move |i: usize, w: &mut Window, cx: &mut gpui::App| {
                        e.update(cx, |p, cx| {
                            let name = p.thread.as_ref().and_then(|t| t.mentions.state.as_ref()).and_then(|s| s.matches.get(i).cloned());
                            p.thread_pick_mention(name, w, cx);
                        })
                    };
                    row = row.child(deferred(super::input::autocomplete::mention_dropdown(m, &t.mention_scroll, pick)).with_priority(1));
                }
            }
            row
        });

        let mut list = div().id("thread-list").flex_1().min_h_0().overflow_y_scroll().track_scroll(&scroll).flex().flex_col().pt(px(14.)).pb(px(4.));
        if !has_root {
            list = list.child(div().px(px(8.)).pb(px(4.)).text_size(px(11.)).line_height(px(15.)).text_color(c(theme::FAINT)).child("The message that started this thread is no longer in chat."));
        }
        let scope = self.sel_thread.clone();
        for (i, (key, b)) in built.iter().enumerate() {
            let el = self.with_sel_row(&scope, i, |this| this.built_el(&format!("thread|{key}"), b, cx));
            list = list.child(el);
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
            .child(div().flex_none().text_color(c(theme::MUTED)).child(icon(icons::REPLY, 14.0)))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child("Reply thread"))
            .child(div().flex_1().text_color(c(theme::FAINT)).child(SharedString::from(match replies {
                1 => "1 reply".to_string(),
                n => format!("{n} replies"),
            })))
            .child(
                div()
                    .id("thread-close")
                    .flex_none()
                    .p(px(3.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .text_color(c(theme::MUTED))
                    .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                    .child(icon(icons::X, 14.0))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.close_thread(cx))),
            );

        let sheet = div()
            .id("thread-sheet")
            .occlude()
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .h(relative(0.7))
            .flex()
            .flex_col()
            .bg(c(theme::PANEL))
            .border_t_1()
            .border_color(c(theme::LINE))
            .shadow_2xl()
            .track_focus(&focus)
            // a click inside the sheet is not a click on the backdrop behind it
            .on_mouse_down(gpui::MouseButton::Left, |_: &gpui::MouseDownEvent, _w, cx| cx.stop_propagation())
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _w, cx| {
                if ev.keystroke.key == "escape" {
                    this.close_thread(cx);
                    cx.stop_propagation();
                }
            }))
            .child(header)
            .child(list)
            // a reply to the thread's first message, so Twitch keeps it in this thread
            .when_some(reply_box, |d, b| d.child(b))
            // after the rows: it hit-tests against the flows they drew
            .child(super::select::sel_layer(self.sel_thread.clone()));

        // the dimmed list behind the sheet: a click there closes it
        Some(
            div()
                .id("thread-backdrop")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(gpui::Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.35 })
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| this.close_thread(cx)))
                .child(sheet)
                .into_any_element(),
        )
    }

    // ---- empty state ---------------------------------------------------------------------------------------

    fn empty_el(&self) -> AnyElement {
        let states = &self.store.sources;
        let any_loaded = states.iter().any(|s| s.loaded);
        let connecting = !any_loaded && states.iter().all(|s| s.conn.as_ref().map(|c| c.state == "connecting").unwrap_or(true));
        let mut d = div().w_full().px(px(16.)).py(px(40.)).flex().flex_col().items_center().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::FAINT));
        if connecting {
            d = d.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        // `animate-spin rounded-full border-2 border-faint border-t-transparent`: 1 s linear
                        gpui::svg()
                            .path("icons/spinner.svg")
                            .size(px(12.))
                            .flex_none()
                            .text_color(c(theme::FAINT))
                            .with_animation(
                                "connecting-spin",
                                gpui::Animation::new(std::time::Duration::from_secs(1)).repeat(),
                                |s, d| s.with_transformation(gpui::Transformation::rotate(gpui::percentage(d))),
                            ),
                    )
                    .child("Connecting..."),
            );
        } else {
            d = d.child(div().child("No messages yet."));
            for s in states {
                if let Some(cn) = &s.conn {
                    if cn.state != "open" {
                        if let Some(det) = cn.detail.as_ref().filter(|d| !d.is_empty()) {
                            d = d.child(div().mt(px(4.)).child(SharedString::from(format!("{}: {}", s.source.platform.as_str(), det))));
                        }
                    }
                }
            }
        }
        d.into_any_element()
    }

    // ---- pinned card ---------------------------------------------------------------------------------------

    /// Rumble's pinned message: one bar with the pin icon, who said it and the line (the chat's own emotes drawn),
    /// and an Unpin button for a moderator.
    fn rumble_pinned_el(&mut self, sk: &str, m: &RumbleMsg, cx: &mut Context<Self>) -> Option<AnyElement> {
        let st = self.store.source(sk)?;
        let emotes = st.rumble_emotes.clone();
        let sk_owned = sk.to_string();
        let can_mod = self.co_caps.get(sk).map(|c| c.can_moderate).unwrap_or(false);
        let size = super::twitch::emotes::SizeOpt::Small;
        let mut hf = InlineContentBuilder::new(11.0);
        hf.icon(icons::PIN, 12.0, token(theme::MUTED));
        hf.gap(4.0);
        hf.text("Pinned", token(theme::MUTED), FontWeight::NORMAL);
        let header = hf.finish();
        let mut cf = InlineContentBuilder::new(self.settings.font_size);
        let who = if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() };
        cf.text(&format!("{who}: "), token(theme::TEXT), FontWeight::SEMIBOLD);
        let pieces = agu_chat_model::user_card::card_pieces(&RowBody::Rumble(m.clone()), None, &emotes);
        cf.pieces(&pieces, size, None, token(theme::TEXT), FontWeight::NORMAL);
        let content = cf.finish();
        let key = format!("{sk}|rumble-pin");
        let text_col = div().min_w_0().flex_1().child(self.flow(&header, Rc::new(vec![]), cx)).child(div().mt(px(2.)).w_full().child(self.flow(&content, Rc::new(vec![]), cx)));
        let mut row = div().flex().items_start().gap(px(8.)).child(text_col);
        if can_mod {
            let msg = m.clone();
            row = row.child(
                div()
                    .id(eid("rumble-unpin-", &key))
                    .flex_none()
                    .px(px(8.))
                    .py(px(3.))
                    .rounded(px(4.))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(c(theme::MUTED))
                    .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                    .cursor_pointer()
                    .child("Unpin")
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.run_action(RowAction::RumbleMod { sk: sk_owned.clone(), action: "unpin", msg: Box::new(msg.clone()), seconds: 300 }, cx);
                    })),
            );
        }
        Some(div().id(eid("pin-", &key)).flex_none().mx(px(6.)).mt(px(6.)).rounded(px(4.)).border_1().border_color(c(theme::LINE)).bg(c(theme::RAISED)).px(px(10.)).py(px(6.)).shadow_lg().child(row).into_any_element())
    }

    fn pinned_el(&mut self, sk: &str, p: &ChatMsg, multi: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let env = self.envs.get(sk)?.clone();
        let rs = self.settings.clone();
        let base = rs.font_size;
        let ends_at = super::pins::pin_ends_at_ms(p);
        let now = chrono::Utc::now().timestamp_millis();
        if super::pins::pin_expired(p, now) {
            return None;
        }
        let open = self.pin_open.contains(&p.id);
        let by = p.pin.as_ref().map(|x| x.by.clone());
        let left = ends_at.map(|e| super::pins::pin_left_secs(e, now));
        let left_text = left.map(super::pins::left_text);
        let size = super::twitch::emotes::SizeOpt::Small;
        let key = format!("{sk}|{}", p.id);

        // header: pin icon, channel chip, "Pinned by", the pinner's badges and name (11px, muted)
        let mut hf = InlineContentBuilder::new(11.0);
        hf.icon(icons::PIN, 12.0, token(theme::MUTED));
        hf.gap(4.0);
        if multi {
            hf.pill(&format!("#{}", env.source.channel), 10.0, token(theme::MUTED), FontWeight::SEMIBOLD, token_a(theme::WHITE, 0.1), 4.0, 4.0);
        }
        hf.text(if by.as_ref().map(|b| !b.name.is_empty()).unwrap_or(false) { "Pinned by " } else { "Pinned" }, token(theme::MUTED), FontWeight::NORMAL);
        if let Some(b) = by.as_ref().filter(|b| !b.name.is_empty()) {
            // the pinner's badges come from their latest line in this chat
            let pinner = self.store.rows().iter().rev().find_map(|r| match &r.body {
                RowBody::Msg(m) if r.sk == sk && m.user.id == b.id => Some((m.badges.clone(), m.user.color.clone())),
                _ => None,
            });
            if let Some((badges, _)) = &pinner {
                for bd in super::twitch::badges::parse_badge_list(badges, &env.badge_map) {
                    hf.badge(&bd.url);
                    hf.gap(2.0);
                }
            }
            hf.text(&b.name, token(theme::TEXT), FontWeight::SEMIBOLD);
        }
        let header = hf.finish();

        let parts = build_parts(&p.text, &p.emotes, env.emotes.as_deref(), None, p.gifs.as_deref().unwrap_or(""));
        let mut cf = InlineContentBuilder::new(base);
        cf.message(&parts, size, env.emotes.as_deref(), token(theme::TEXT), FontWeight::SEMIBOLD);
        let content = cf.finish();

        let icon_btn = |id: String| div().id(ElementId::from(SharedString::from(id))).size(px(24.)).flex().items_center().justify_center().rounded_full().text_color(c(theme::MUTED)).hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)));

        let mut text_col = div().min_w_0().flex_1().child(self.flow(&header, Rc::new(vec![]), cx));
        if open {
            if let Some(r) = &p.reply {
                text_col = text_col.child(
                    div().mt(px(4.)).overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(SharedString::from(format!("Replying to @{}: {}", r.user, r.text))),
                );
            }
        }
        let body = div().mt(px(2.)).w_full().child(self.flow(&content, Rc::new(vec![]), cx));
        text_col = text_col.child(if open { body } else { body.max_h(px(base * 1.5)).overflow_hidden() });
        if open {
            let mut df = InlineContentBuilder::new(12.0);
            for bd in super::twitch::badges::parse_badge_list(&p.badges, &env.badge_map) {
                df.badge(&bd.url);
                df.gap(2.0);
            }
            df.text(&p.user.name, hex(&name_color(&p.user, rs.readable_colors)), FontWeight::SEMIBOLD);
            df.text(&format!(" sent at {}", fmt_time(p.ts, false)), token(theme::MUTED), FontWeight::NORMAL);
            if let Some(l) = &left_text {
                df.text(&format!(" · unpins in {l}"), token(theme::FAINT), FontWeight::NORMAL);
            }
            text_col = text_col.child(div().mt(px(4.)).child(self.flow(&df.finish(), Rc::new(vec![]), cx)));
        }

        let k_menu = key.clone();
        let k_open = key.clone();
        let pid = p.id.clone();
        let pid2 = p.id.clone();
        let menu_open = self.pin_menu.as_ref().map(|(k, _)| k == &key).unwrap_or(false);
        let buttons = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .child(
                icon_btn(format!("pin-more-{key}"))
                    .child(icon(icons::DOTS, 16.0))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.pin_menu = if this.pin_menu.as_ref().map(|(k, _)| k == &k_menu).unwrap_or(false) { None } else { Some((k_menu.clone(), false)) };
                        cx.notify();
                    })),
            )
            .child(
                icon_btn(format!("pin-open-{key}"))
                    .child(icon(if open { icons::CHEVRON_UP } else { icons::CHEVRON }, 16.0))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        if !this.pin_open.remove(&pid) {
                            this.pin_open.insert(pid.clone());
                        }
                        let _ = &k_open;
                        cx.notify();
                    })),
            );

        let mut card = div()
            .id(eid("pin-", &key))
            .relative()
            .flex_none()
            .mx(px(6.))
            .mt(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .px(px(10.))
            .py(px(6.))
            .shadow_lg()
            .child(div().flex().items_start().gap(px(8.)).child(text_col).child(buttons));

        if menu_open {
            let can_mod = env.caps.pin;
            let item = |id: String, ic: &'static str, label: &'static str, more: bool| {
                div()
                    .id(ElementId::from(SharedString::from(id)))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .w_full()
                    .rounded(px(4.))
                    .px(px(10.))
                    .py(px(6.))
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .child(div().text_color(c(theme::MUTED)).child(icon(ic, 16.0)))
                    .child(div().flex_1().child(label))
                    .when(more, |d| d.child(div().text_color(c(theme::MUTED)).child(icon(icons::CHEVRON_RIGHT, 16.0))))
            };
            let mut menu = div().absolute().right(px(8.)).top(px(36.)).w(px(224.)).rounded(px(4.)).border_1().border_color(c(theme::LINE)).bg(c(theme::RAISED)).p(px(4.)).text_size(px(14.)).line_height(px(20.)).text_color(c(theme::TEXT)).shadow_xl();
            let src = env.source.clone();
            if self.pin_menu.as_ref().map(|(_, a)| *a).unwrap_or(false) && self.unpin_after.is_some() {
                // "Unpin After": from what is left on the pin now
                menu = menu.child(self.unpin_after_el(&src, &pid2, cx));
                return Some(card.child(deferred(menu).with_priority(3)).into_any_element());
            }
            let (p_id, k1, k2) = (pid2.clone(), key.clone(), key.clone());
            if can_mod {
                menu = menu.child(item(format!("unpin-{key}"), icons::MINUS_CIRCLE, "Unpin this Message", false).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    this.pin_menu = None;
                    let (bridge, src, id) = (this.bridge_clone(), src.clone(), p_id.clone());
                    cx.spawn(async move |this, cx| {
                        let (_, t) = super::mod_actions::unpin_message(&bridge, &src, &id).await;
                        let _ = this.update(cx, |p, cx| p.show_toast(t, cx));
                    })
                    .detach();
                    cx.notify();
                })));
            }
            if can_mod {
                let k3 = key.clone();
                let left_s = left;
                menu = menu.child(item(format!("after-{key}"), icons::CLOCK, "Unpin After", true).on_click(cx.listener(move |this, _: &ClickEvent, w, cx| {
                    // starts from what's left on the pin now, in minutes when it's a minute or more
                    let (time_mode, unit_min, amount) = super::pins::unpin_after_start(left_s);
                    let input = cx.new(|cx| {
                        let mut t = TextInput::new(w, cx, "", InputStyle::Chat);
                        t.set_max_chars(Some(6));
                        t.set_centered(true);
                        t.set_text(&amount, cx);
                        t
                    });
                    let sub = cx.subscribe(&input, |this, _i, ev: &TextInputEvent, cx| {
                        // `onFocus={() => setMode("time")}`: typing in the amount chooses it
                        if let (TextInputEvent::Changed, Some(u)) = (ev, this.unpin_after.as_mut()) {
                            u.time_mode = true;
                            cx.notify();
                        }
                    });
                    this.unpin_after = Some(UnpinAfter { time_mode, unit_min, amount: input, busy: false, _sub: sub });
                    this.pin_menu = Some((k3.clone(), true));
                    cx.notify();
                })));
            }
            let hide_id = pid2.clone();
            menu = menu.child(item(format!("hide-{key}"), icons::EYE_OFF, "Hide for Yourself", false).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                this.pin_menu = None;
                this.hide_pin(&hide_id);
                let _ = (&k1, &k2);
                cx.notify();
            })));
            card = card.child(deferred(menu).with_priority(3));
        }
        Some(card.into_any_element())
    }

    // ---- search bar, pill, scrollbar, toasts, send box, tip -----------------------------------------------------

    fn search_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.search_open {
            return None;
        }
        let n_rows = self.shown.len();
        let q = super::search::parse_search(&self.query);
        let filtering = !super::search::is_empty_query(&q);
        let count = if filtering { format!("{} {}", n_rows, if n_rows == 1 { "match" } else { "matches" }) } else { format!("{} messages", self.store.rows().len()) };
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .border_b_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::RAISED))
                .px(px(6.))
                .py(px(4.))
                .text_size(px(12.))
                .line_height(px(16.))
                .child(div().flex_none().text_color(c(theme::FAINT)).child(icon(icons::SEARCH, 14.0)))
                .children(self.search.clone())
                .child(div().flex_none().text_color(c(theme::FAINT)).child(SharedString::from(count)))
                .child(
                    div()
                        .id("search-close")
                        .flex_none()
                        .rounded(px(4.))
                        .p(px(2.))
                        .text_color(c(theme::FAINT))
                        .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                        .child(icon(icons::X, 14.0))
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.close_search(window, cx))),
                )
                .into_any_element(),
        )
    }

    fn pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at_bottom = !(self.scroll_held || self.frozen());
        if at_bottom {
            return None;
        }
        let n = self.new_count();
        let label = format!("{}{}", if self.search_open { "Newest matches" } else if self.scroll_held && !self.frozen() { "More messages below" } else if self.frozen() { "Chat paused" } else { "More messages below" }, if n > 0 { format!(" ({})", if n > 999 { "999+".to_string() } else { n.to_string() }) } else { String::new() });
        Some(
            div()
                .absolute()
                .bottom(px(8.))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("more-pill")
                        .rounded_full()
                        .border_1()
                        .border_color(token_a(theme::ACCENT, 0.5))
                        .bg(c(theme::RAISED))
                        .px(px(12.))
                        .py(px(4.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(theme::ACCENT_SOFT))
                        .shadow_xl()
                        .hover(|s| s.bg(c(theme::RAISED_2)))
                        .child(SharedString::from(label))
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.resume(cx))),
                )
                .into_any_element(),
        )
    }

    /// The `overflow-y: auto` scrollbar: drag the thumb, or press the track to page (Chromium pages by
    /// most of a viewport toward the press).
    fn scrollbar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (top, h) = self.list_state.thumb()?;
        let thumb = div()
            .id("sb-thumb")
            .absolute()
            .right_0()
            .top(px(top))
            .w(px(8.))
            .h(px(h))
            .rounded(px(4.))
            .bg(c(theme::SCROLLBAR_THUMB))
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, ev: &gpui::MouseDownEvent, _w, cx| {
                cx.stop_propagation();
                this.sb_drag = Some((ev.position.y.f(), top));
                cx.notify();
            }));
        let track = div()
            .id("sb-track")
            .absolute()
            .right_0()
            .top_0()
            .w(px(8.))
            .h_full()
            .on_mouse_down(gpui::MouseButton::Left, cx.listener(move |this, ev: &gpui::MouseDownEvent, _w, cx| {
                let (track_top, vh) = this.list_state.track();
                let page = (vh * 0.875).max(40.0);
                let y = ev.position.y.f() - track_top;
                this.list_state.scroll_by(if y < top { -page } else { page });
                cx.notify();
            }));
        let mut el = div().absolute().right_0().top_0().w(px(8.)).h_full().child(track).child(thumb);
        // while dragging, follow the pointer anywhere in the window until the button is released
        if self.sb_drag.is_some() {
            let this = cx.entity().downgrade();
            el = el.child(
                gpui::canvas(
                    |_, _, _| {},
                    move |_, _, window, _| {
                        let this2 = this.clone();
                        window.on_mouse_event(move |ev: &gpui::MouseMoveEvent, phase, _w, cx| {
                            if phase != gpui::DispatchPhase::Capture {
                                return;
                            }
                            let _ = this2.update(cx, |p, cx| {
                                if let Some((y0, t0)) = p.sb_drag {
                                    if ev.pressed_button != Some(gpui::MouseButton::Left) {
                                        p.sb_drag = None;
                                    } else {
                                        p.list_state.scroll_to_thumb(t0 + ev.position.y.f() - y0);
                                    }
                                    cx.notify();
                                }
                            });
                        });
                        let this3 = this.clone();
                        window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _w, cx| {
                            if phase == gpui::DispatchPhase::Capture {
                                let _ = this3.update(cx, |p, cx| {
                                    p.sb_drag = None;
                                    cx.notify();
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            );
        }
        Some(el.into_any_element())
    }

    fn toasts_el(&self) -> Option<AnyElement> {
        if self.toasts.is_empty() {
            return None;
        }
        let mut col = div().absolute().right(px(8.)).top(px(8.)).flex().flex_col().items_end().gap(px(4.)).max_w(relative(0.8));
        for t in &self.toasts {
            col = col.child(
                div()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(if t.warn { token_a(theme::AMBER_500, 0.4) } else { token(theme::LINE) })
                    .bg(c(theme::RAISED))
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(if t.warn { token(theme::AMBER_200) } else { token(theme::TEXT) })
                    .shadow_lg()
                    .child(SharedString::from(t.text.clone())),
            );
        }
        Some(col.into_any_element())
    }

    /// The send box: the pane's own (Rumble / Kick ban) confirm bar over the `SendBox` entity.
    fn send_box(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        use super::input::send_box::SendBox;
        let weak = cx.entity().downgrade();
        let sb = self.send_box.get_or_insert_with(|| cx.new(|cx| SendBox::new(window, cx, weak))).clone();
        let (sources, focused, reply) = (self.send_sources(), self.focused, self.reply_target.clone());
        sb.update(cx, |b, cx| b.set_props(sources, focused, reply, window, cx));
        let mut root = div().flex_none();
        if let Some(cf) = &self.confirm {
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(c(theme::LINE))
                    .bg(token_a(theme::RED_500, 0.1))
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(c(theme::RED_300))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(cf.text.clone())))
                    .child(crate::layout::ui::btn("pc-yes", "Confirm", crate::layout::ui::BtnOpts::danger()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.confirm_yes(cx))))
                    .child(crate::layout::ui::btn("pc-no", "Cancel", Default::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.confirm = None;
                        cx.notify();
                    }))),
            );
        }
        root.child(sb).into_any_element()
    }

    /// `UnpinAfter`: the menu's second page.
    fn unpin_after_el(&mut self, src: &agu_chat_core::protocol::SourceRef, pin_id: &str, cx: &mut Context<Self>) -> AnyElement {
        let Some(u) = self.unpin_after.as_ref() else { return div().into_any_element() };
        let (time_mode, unit_min, busy) = (u.time_mode, u.unit_min, u.busy);
        let amount = u.amount.read(cx).text();
        let secs = unpin_secs(&amount, unit_min);
        let valid = !time_mode || secs.is_some();
        let radio = |id: &'static str, on: bool, label: &'static str, cx: &mut Context<Self>, set: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(4.))
                .child(
                    div()
                        .flex_none()
                        .size(px(13.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .border_1()
                        .border_color(if on { c(theme::ACCENT) } else { c(theme::FAINT) })
                        .when(on, |d| d.child(div().size(px(7.)).rounded_full().bg(c(theme::ACCENT)))),
                )
                .child(div().min_w_0().flex_1().child(label))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    if let Some(u) = this.unpin_after.as_mut() {
                        u.time_mode = set;
                    }
                    cx.notify();
                }))
        };
        let (src, pid) = (src.clone(), pin_id.to_string());
        let unit_btn = div()
            .id("unit")
            .flex()
            .items_center()
            .gap(px(4.))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .px(px(8.))
            .py(px(4.))
            .child(if unit_min { "min" } else { "sec" })
            .child(div().text_color(c(theme::MUTED)).child(icon(icons::CHEVRON, 12.0)))
            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                if let Some(u) = this.unpin_after.as_mut() {
                    u.unit_min = !u.unit_min;
                    u.time_mode = true;
                }
                cx.notify();
            }));
        let mut col = div().p(px(6.)).child(
            div()
                .relative()
                .mb(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("unpin-back")
                        .absolute()
                        .left_0()
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .hover(|s| s.bg(c(theme::RAISED_2)))
                        .child(icon(icons::CHEVRON_LEFT, 16.0))
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            if let Some((k, _)) = this.pin_menu.take() {
                                this.pin_menu = Some((k, false));
                            }
                            cx.notify();
                        })),
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child("Unpin After")),
        );
        col = col.child(radio("r-time", time_mode, "Customized time", cx, true)).child(
            div().flex().gap(px(8.)).pb(px(4.)).pl(px(24.)).child(div().w(px(64.)).child(u.amount.clone())).child(unit_btn),
        );
        if time_mode && !valid {
            col = col.child(div().pl(px(24.)).text_size(px(11.)).line_height(px(16.)).text_color(c(theme::AMBER_400)).child(super::pins::PIN_RANGE_HINT));
        }
        col = col.child(radio("r-end", !time_mode, "Manually unpinned or end of stream", cx, false)).child(
            div()
                .id("unpin-save")
                .mt(px(8.))
                .w_full()
                .flex()
                .justify_center()
                .rounded(px(4.))
                .bg(c(theme::ACCENT))
                .py(px(4.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(theme::WHITE))
                .when(!valid || busy, |d| d.opacity(0.4))
                .when(valid && !busy, |d| d.hover(|s| s.bg(c(theme::ACCENT_STRONG))))
                .child("Save")
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    let Some(u) = this.unpin_after.as_mut() else { return };
                    let secs = if u.time_mode { unpin_secs(&u.amount.read(cx).text(), u.unit_min) } else { None };
                    if (u.time_mode && secs.is_none()) || u.busy {
                        return;
                    }
                    u.busy = true;
                    let (bridge, src, pid) = (this.bridge_clone(), src.clone(), pid.clone());
                    cx.spawn(async move |this, cx| {
                        let (ok, t) = super::mod_actions::set_pin_duration(&bridge, &src, &pid, secs.map(|s| s as u32)).await;
                        let _ = this.update(cx, |p, cx| {
                            if let Some(u) = p.unpin_after.as_mut() {
                                u.busy = false;
                            }
                            if ok {
                                p.pin_menu = None;
                                p.unpin_after = None;
                            }
                            p.show_toast(t, cx);
                            cx.notify();
                        });
                    })
                    .detach();
                    cx.notify();
                })),
        );
        col.into_any_element()
    }

    /// The Ban / Warn popover, created here because building its inputs needs the window.
    fn mod_pop_el(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyView> {
        if let Some(r) = self.pop_req.take() {
            let weak = cx.entity().downgrade();
            self.mod_pop = Some(cx.new(|cx| super::mod_popover::ModPopoverView::new(window, cx, weak, r.sk, r.msg, r.kind, r.pos, r.above)));
        }
        self.mod_pop.clone().map(|v| v.into())
    }

    /// The "Pin for" menu a message's pin button opens: anchored at the button, below it or above it in the lower half.
    fn pin_pick_el(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pick = self.pin_pick.as_ref()?;
        let (sk, id, pos) = (pick.sk.clone(), pick.id.clone(), pick.pos);
        let above = pos.y.f() > window.viewport_size().height.f() / 2.0;
        let mut menu = div()
            .id("pin-pick")
            .occlude()
            .w(px(232.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(4.))
            .shadow_2xl()
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .font_family(theme::FONT)
            .on_mouse_down_out(cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
                this.pin_pick = None;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(4.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::FAINT)).child("PIN FOR"));
        for (label, secs) in super::mod_actions::PIN_CHOICES {
            let (sk, id) = (sk.clone(), id.clone());
            menu = menu.child(
                div()
                    .id(ElementId::from(SharedString::from(format!("pin-for-{}", secs.unwrap_or(0)))))
                    .w_full()
                    .rounded(px(4.))
                    .px(px(8.))
                    .py(px(5.))
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .child(label)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.pin_pick = None;
                        this.pin_with(sk.clone(), id.clone(), secs, cx);
                        cx.notify();
                    })),
            );
        }
        let corner = if above { Corner::BottomLeft } else { Corner::TopLeft };
        let at = point(pos.x, if above { pos.y - px(12.) } else { pos.y + px(12.) });
        Some(deferred(anchored().position(at).anchor(corner).snap_to_window_with_margin(gpui::Edges::all(px(4.))).child(menu)).with_priority(4).into_any_element())
    }

    /// Right-click over a text selection: Copy.
    fn copy_menu_el(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (pos, _) = self.copy_menu.as_ref()?;
        let pos = *pos;
        let above = pos.y.f() > window.viewport_size().height.f() / 2.0;
        let shortcut = if cfg!(target_os = "macos") { "⌘C" } else { "Ctrl+C" };
        let menu = div()
            .id("copy-menu")
            .occlude()
            .min_w(px(140.))
            .flex()
            .flex_col()
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(4.))
            .shadow_2xl()
            .text_size(px(13.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .font_family(theme::FONT)
            .on_mouse_down_out(cx.listener(|this, _: &gpui::MouseDownEvent, _w, cx| {
                this.copy_menu = None;
                cx.notify();
            }))
            .child(
                div()
                    .id("copy-menu-copy")
                    .w_full()
                    .flex()
                    .justify_between()
                    .gap(px(16.))
                    .rounded(px(4.))
                    .px(px(8.))
                    .py(px(5.))
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .child("Copy")
                    .child(div().text_color(c(theme::FAINT)).child(shortcut))
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        if let Some((_, text)) = this.copy_menu.take() {
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                        }
                        cx.notify();
                    })),
            );
        let corner = if above { Corner::BottomLeft } else { Corner::TopLeft };
        Some(deferred(anchored().position(pos).anchor(corner).snap_to_window_with_margin(gpui::Edges::all(px(4.))).child(menu)).with_priority(4).into_any_element())
    }

    pub(super) fn tip_el(&self, window: &Window) -> Option<AnyElement> {
        let (tip, pos) = self.tip.as_ref()?;
        let vp = window.viewport_size();
        let left = (pos.x.f() + 14.0).min(vp.width.f() - 140.0);
        let top = (pos.y.f() + 14.0).min(vp.height.f() - 130.0);
        let mut b = div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(8.))
            .text_size(px(12.))
            .line_height(px(16.))
            .shadow_2xl();
        if !tip.glyph.is_empty() {
            // a Unicode emoji, in the system's emoji font at the size an emote image gets
            b = b.child(div().h(px(64.)).flex().items_center().text_size(px(52.)).line_height(px(64.)).child(tip.glyph.clone()));
        } else if !tip.img.is_empty() {
            let fallback = Some(tip.fallback.clone()).filter(|f| !f.is_empty() && *f != tip.img);
            b = b.child(ImageBox { url: tip.img.clone(), side: 64.0, radius: 0.0, fallback });
        }
        b = b.child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child(tip.name.clone()));
        if !tip.label.is_empty() {
            b = b.child(div().text_color(c(theme::FAINT)).child(tip.label.clone()));
        }
        Some(deferred(anchored().anchor(Corner::TopLeft).position(point(px(left), px(top))).child(b)).with_priority(5).into_any_element())
    }
}

fn fg_base(fg: u32) -> u32 {
    if fg == theme::GREEN_300 { theme::GREEN_500 } else { theme::RED_500 }
}

impl Render for ChatPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.drain_events(cx);
        // on a screen with a higher scale the emotes need bigger files: the rows pick their URLs again
        super::rows::note_scale_factor(window.scale_factor());
        if super::rows::emote_scale_factor() != self.emote_scale {
            self.emote_scale = super::rows::emote_scale_factor();
            self.gen += 1;
            self.invalidate_all();
        }
        self.test_aids(window, cx);
        if self.settle > 0 {
            // a list needs a frame or two after its content changed (measure, then stick to the bottom)
            self.settle -= 1;
            window.request_animation_frame();
        }
        self.track_scroll();
        if !self.scroll_held && !self.frozen() {
            self.scroll_to_end();
        }
        let font = self.settings.font_size;

        // pinned cards (twitch sources with a pin that was not hidden)
        let multi_twitch = self.store.sources.iter().filter(|s| s.source.platform == agu_chat_core::protocol::Platform::Twitch).count() > 1;
        let pins: Vec<(String, ChatMsg)> = self.store.sources.iter().filter(|s| s.source.platform == agu_chat_core::protocol::Platform::Twitch).filter_map(|s| s.pinned.clone().map(|p| (s.sk.clone(), p))).filter(|(_, p)| !self.hidden_pins.contains(&p.id)).collect();
        let mut cards: Vec<AnyElement> = Vec::new();
        let mut needs_tick = false;
        for (sk, p) in &pins {
            if p.pin.as_ref().and_then(|x| x.ends_at.as_ref()).is_some() {
                needs_tick = true;
            }
            if let Some(el) = self.pinned_el(sk, p, multi_twitch, cx) {
                cards.push(el);
            }
        }
        let rumble_pins: Vec<(String, RumbleMsg)> = self.store.sources.iter().filter_map(|s| s.rumble_pinned.clone().map(|p| (s.sk.clone(), p))).collect();
        for (sk, p) in &rumble_pins {
            if let Some(el) = self.rumble_pinned_el(sk, p, cx) {
                cards.push(el);
            }
        }
        if needs_tick {
            self.ensure_pin_tick(cx);
        }
        let hype = self.hype_bars(cx);

        let list_el: AnyElement = if self.shown.is_empty() {
            div().size_full().overflow_hidden().child(self.empty_el()).into_any_element()
        } else {
chat_list(
                self.list_state.clone(),
                {
                    let p = cx.processor(|this, (ix, w): (usize, f32), window, cx| this.row_metrics(ix, w, window, cx));
                    std::rc::Rc::new(move |ix, w, window, cx| p((ix, w), window, cx))
                },
                std::rc::Rc::new(cx.processor(|this, ix: usize, _window, cx| this.render_row(ix, cx))),
            )
            .into_any_element()
        };

        let hover_pause = self.settings.pause_chat == "hover";
        let overflow = self.list_state.thumb().is_some();
        let list_area = div()
            .id("msg-list")
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .on_hover(cx.listener(move |this, h: &bool, _w, cx| {
                if hover_pause {
                    this.set_hover_hold(*h, cx);
                }
            }))
            .child(div().flex_1().min_w_0().h_full().overflow_hidden().child(list_el))
            // after the rows: it hit-tests against the flows they drew
            .child(super::select::sel_layer(self.sel_main.clone()))
            .when(overflow, |d| d.child(div().flex_none().w(px(8.)).h_full()))
            .children(self.scrollbar(cx))
            .children(self.pill(cx));

        let thread = self.thread_el(window, cx);
        let message_list = div().relative().flex().flex_col().flex_1().min_h_0().children(self.search_bar(cx)).child(list_area).children(thread);

        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(c(theme::PANEL))
            .text_color(c(theme::TEXT))
            .text_size(px(font))
            .font_family(theme::FONT)
            .children(hype)
            .children(cards)
            .child(message_list)
            .children(self.toasts_el())
            .children(self.mod_pop_el(window, cx))
            .children(self.pin_pick_el(window, cx))
            .children(self.cmd_pick_el(window, cx))
            .children(self.copy_menu_el(window, cx))
            .child(self.send_box(window, cx))
            .children(self.tip_el(window))
    }
}

/// Small builder for the card's inline contents (the JS used JSX fragments).
pub struct InlineContentBuilder {
    items: Vec<Item>,
    base: f32,
    ratio: f32,
}

impl InlineContentBuilder {
    pub fn new(base: f32) -> Self {
        InlineContentBuilder { items: Vec::new(), base, ratio: 1.5 }
    }
    pub fn text(&mut self, t: &str, color: Hsla, w: FontWeight) {
        self.items.push(Item::Text(TextItem::new(t.to_string(), self.base, color).weight(w)));
    }
    pub fn gap(&mut self, w: f32) {
        self.items.push(Item::Gap(w));
    }
    pub fn icon(&mut self, path: &'static str, size: f32, color: Hsla) {
        self.items.push(Item::Icon(IconItem { path, size, color, valign: VAlign::Middle, margin_x: 0.0 }));
    }
    pub fn pill(&mut self, t: &str, size: f32, color: Hsla, w: FontWeight, bg: Hsla, pad_x: f32, margin_r: f32) {
        self.items.push(Item::Pill(PillItem { text: t.to_string().into(), size, color, weight: w, bg, pad_x, margin_r, radius: 4.0, copy: false }));
    }
    pub fn badge(&mut self, url: &str) {
        self.items.push(Item::Image(ImageItem {
            url: url.to_string().into(),
            max_w: 16.0,
            max_h: 16.0,
            fixed: true,
            wrap: false,
            vmargin: -3.0,
            margin_x: 0.0,
            valign: VAlign::BoxCenter,
            tip: None,
            overlays: vec![],
            radius: 0.0,
            act: None,
        }));
    }
    /// a picture of `size` px, square (`radius` rounds it: half the size for a circle)
    pub fn image(&mut self, url: &str, size: f32, radius: f32, tip: Option<Rc<Tip>>) {
        self.items.push(Item::Image(ImageItem {
            url: url.to_string().into(),
            max_w: size,
            max_h: size,
            fixed: true,
            wrap: false,
            vmargin: 0.0,
            margin_x: 0.0,
            valign: VAlign::BoxCenter,
            tip,
            overlays: vec![],
            radius,
            act: None,
        }));
    }
    pub fn message(&mut self, parts: &[super::twitch::parts::MsgPart], size: super::twitch::emotes::SizeOpt, emotes: Option<&super::twitch::emotes::EmoteSets>, color: Hsla, w: FontWeight) {
        let mut f = super::rows::message_flow(self.base, parts, size, emotes, color);
        for it in f.iter_mut() {
            if let Item::Text(t) = it {
                t.weight = w;
            }
        }
        self.items.extend(f);
    }
    /// A line from `agu_chat_model::user_card::card_pieces`: text, the platform's own emote pictures and the chat's
    /// message parts (Twitch / Kick emotes), the way the chat rows draw them.
    pub fn pieces(&mut self, pieces: &[agu_chat_model::user_card::CardPiece], size: super::twitch::emotes::SizeOpt, emotes: Option<&super::twitch::emotes::EmoteSets>, color: Hsla, w: FontWeight) {
        use agu_chat_model::user_card::CardPiece;
        for p in pieces {
            match p {
                CardPiece::Text(t) => self.text(t, color, w),
                CardPiece::Parts(parts) => self.message(parts, size, emotes, color, w),
                CardPiece::Image { url, alt, label } => {
                    let (px, max_w, vm) = super::rows::emote_box(self.base, size);
                    self.items.push(Item::Image(ImageItem {
                        url: url.clone().into(),
                        max_w,
                        max_h: px,
                        fixed: false,
                        wrap: false,
                        vmargin: vm,
                        margin_x: 1.0,
                        valign: VAlign::Middle,
                        tip: Some(Rc::new(Tip::new(url.clone(), alt.clone(), label.clone()))),
                        overlays: vec![],
                        radius: 0.0,
                        act: None,
                    }));
                }
            }
        }
    }
    /// line-height as a multiple of the font size (`text-xs` is 16/12)
    pub fn ratio(mut self, r: f32) -> Self {
        self.ratio = r;
        self
    }
    pub fn finish(self) -> Rc<InlineContent> {
        InlineContent::new(self.items, self.base, self.ratio)
    }
}

#[allow(dead_code)]
fn _unused(_: Div) {}
