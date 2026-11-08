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

fn eid(prefix: &str, key: &str) -> ElementId {
    ElementId::from(SharedString::from(format!("{prefix}{key}")))
}

/// `toLocaleString()` for an integer-ish amount.
fn thousands(n: f64) -> String {
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
        if let Some(img) = imgs.0.get(&self.url, self.side, self.side, window.scale_factor()) {
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

    fn flow(&self, content: &Rc<InlineContent>, actions: Rc<Vec<RowAction>>, cx: &mut Context<Self>) -> InlineFlow {
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
                    if let (RowAction::Card { user, .. }, true) = (&a, w.modifiers().control) {
                        let user = user.clone();
                        let _ = weak.update(cx, |p, cx| p.mention_user(&user, w, cx));
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
        inline_flow(content.clone()).on_act(on_act).on_tip(on_tip, self.tip_state.clone())
    }

    // ---- rows --------------------------------------------------------------------------------------------

    fn toolbar_button(&self, id: String, icon_path: &'static str, title: &'static str, danger: bool, cx: &mut Context<Self>, on: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        let _ = title;
        div()
            .id(ElementId::from(SharedString::from(id)))
            .p(px(4.))
            .rounded(px(4.))
            .text_color(c(theme::MUTED))
            .hover(move |s| s.bg(c(theme::RAISED_2)).text_color(c(if danger { theme::RED_400 } else { theme::TEXT })))
            .child(icon(icon_path, 14.0))
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| on(this, cx)))
            .into_any_element()
    }

    fn frame_el(&mut self, key: &str, spec: &FrameSpec, nested: bool, cx: &mut Context<Self>) -> AnyElement {
        let base = self.settings.font_size;
        let actions = Rc::new(spec.actions.clone());
        let hovered = self.hover_row.as_deref() == Some(key) || self.hover_bar.as_deref() == Some(key);
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
        if let Some(text) = &spec.before {
            d = d.child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(base * 0.85))
                    .line_height(px(base * 0.85 * LINE_HEIGHT))
                    .text_color(c(theme::FAINT))
                    .child(SharedString::from(text.clone())),
            );
        }
        d = d.child(self.flow(&spec.content, actions, cx));
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
            if let Some((sk, id)) = h.pin.clone() {
                bar = bar.child(self.toolbar_button(format!("pin-{key}"), icons::PIN, "Pin", false, cx, move |p, cx| p.run_action(RowAction::Pin { sk: sk.clone(), id: id.clone() }, cx)));
            }
            if let Some(r) = h.reply.clone() {
                bar = bar.child(self.toolbar_button(format!("reply-{key}"), icons::REPLY, "Reply", false, cx, move |p, cx| p.run_action(RowAction::Reply(r.clone()), cx)));
            }
            d = d.child(bar);
        }
        d.into_any_element()
    }

    fn event_el(&mut self, key: &str, e: &EventSpec, cx: &mut Context<Self>) -> AnyElement {
        let base = self.settings.font_size;
        let amount = e.amount;
        let mut head = div().flex().items_center().gap(px(8.)).px(px(8.)).pt(px(2.));
        if e.inner.is_none() {
            head = head.pb(px(2.));
        }
        let left = div().flex_1().min_w_0().child(self.flow(&e.content, Rc::new(vec![]), cx));
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
        head = head.child(left).child(right);
        let mut d = div().id(eid("ev-", key)).mx(px(4.)).rounded(px(4.)).border_1().border_color(token_a(theme::PURPLE_500, 0.4)).bg(token_a(theme::PURPLE_500, 0.07)).flex_none().child(head);
        if let Some(inner) = &e.inner {
            d = d.child(self.frame_el(key, inner, true, cx));
        }
        d.into_any_element()
    }

    fn notice_el(&mut self, key: &str, n: &NoticeSpec, cx: &mut Context<Self>) -> AnyElement {
        let (bar, bg) = if n.raid { (token_a(theme::ORANGE_500, 0.7), token_a(theme::ORANGE_500, 0.10)) } else { (token_a(theme::PURPLE_500, 0.7), token_a(theme::PURPLE_500, 0.05)) };
        let mut d = div().id(eid("notice-", key)).w_full().flex_none().border_l_2().border_color(bar).bg(bg).px(px(8.)).py(px(2.)).child(self.flow(&n.system, Rc::new(vec![]), cx));
        if let Some(t) = &n.text {
            d = d.child(self.flow(t, Rc::new(vec![]), cx));
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
            .px(px(8.))
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
                .text_size(px(hs))
                .child(div().flex_none().text_color(c(theme::AMBER_200)).child(icon(icons::WARNING, 14.0)))
                .child(div().flex_none().child(self.flow(&a.header, Rc::new(vec![RowAction::Card { sk: a.sk.clone(), user: a.user.clone() }]), cx).fit()))
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
        d = d.child(self.flow(&a.body, Rc::new(vec![]), cx));
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
                        .flex()
                        .gap(px(6.))
                        .child(btn("Allow", format!("allow-{key}"), theme::GREEN_300, cx, RowAction::Automod { sk: sk1, id: id1, allow: true }))
                        .child(btn("Deny", format!("deny-{key}"), theme::RED_300, cx, RowAction::Automod { sk: sk2, id: id2, allow: false })),
                );
            }
        } else {
            d = d.child(div().text_size(px(11.)).line_height(px(15.95)).text_color(c(theme::FAINT)).child(SharedString::from(a.resolved_line.clone())));
        }
        d.into_any_element()
    }

    pub(super) fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        if ix >= self.shown.len() {
            return div().into_any_element();
        }
        let key = self.shown[ix].row.key.clone();
        let Some(b) = self.built(ix, cx) else { return div().into_any_element() };
        match &*b {
            BuiltRow::Frame(f) => self.frame_el(&key, f, false, cx),
            BuiltRow::Event(e) => self.event_el(&key, e, cx),
            BuiltRow::Notice(n) => self.notice_el(&key, n, cx),
            BuiltRow::Feed(f) => self.feed_el(&key, f),
            BuiltRow::Automod(a) => self.automod_el(&key, a, cx),
        }
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

    fn pinned_el(&mut self, sk: &str, p: &ChatMsg, multi: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let env = self.envs.get(sk)?.clone();
        let rs = self.settings.clone();
        let base = rs.font_size;
        let ends_at = p.pin.as_ref().and_then(|x| x.ends_at.as_deref()).and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis());
        let now = chrono::Utc::now().timestamp_millis();
        if ends_at.map(|e| e <= now).unwrap_or(false) {
            return None;
        }
        let open = self.pin_open.contains(&p.id);
        let by = p.pin.as_ref().map(|x| x.by.clone());
        let left = ends_at.map(|e| ((e - now) as f64 / 1000.0).round().max(0.0) as i64);
        let left_text = left.map(|l| if l >= 60 { format!("{} min", (l as f64 / 60.0).ceil() as i64) } else { format!("{l}s") });
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
                    let (time_mode, unit_min, amount) = match left_s {
                        None => (false, true, "20".to_string()),
                        Some(l) if l >= 60 => (true, true, ((l as f64) / 60.0).ceil().to_string()),
                        Some(l) => (true, false, l.to_string()),
                    };
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
            col = col.child(div().pl(px(24.)).text_size(px(11.)).line_height(px(16.)).text_color(c(theme::AMBER_400)).child("30 seconds to 30 minutes"));
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

    fn tip_el(&self, window: &Window) -> Option<AnyElement> {
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
        if !tip.img.is_empty() {
            b = b.child(ImageBox { url: tip.img.clone(), side: 64.0, radius: 0.0 });
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
        if needs_tick {
            self.ensure_pin_tick(cx);
        }

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
            .when(overflow, |d| d.child(div().flex_none().w(px(8.)).h_full()))
            .children(self.scrollbar(cx))
            .children(self.pill(cx));

        let message_list = div().relative().flex().flex_col().flex_1().min_h_0().children(self.search_bar(cx)).child(list_area);

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
            .children(cards)
            .child(message_list)
            .children(self.toasts_el())
            .children(self.mod_pop_el(window, cx))
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
        self.items.push(Item::Pill(PillItem { text: t.to_string().into(), size, color, weight: w, bg, pad_x, margin_r, radius: 4.0 }));
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
    pub fn message(&mut self, parts: &[super::twitch::parts::MsgPart], size: super::twitch::emotes::SizeOpt, emotes: Option<&super::twitch::emotes::EmoteSets>, color: Hsla, w: FontWeight) {
        let mut f = super::rows::message_flow(self.base, parts, size, emotes, color);
        for it in f.iter_mut() {
            if let Item::Text(t) = it {
                t.weight = w;
            }
        }
        self.items.extend(f);
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
