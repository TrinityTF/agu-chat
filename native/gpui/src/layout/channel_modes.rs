//! Port of src/renderer/layout/ChannelModes.tsx: the Modes button of a split's header (shown only to someone who can
//! moderate one of its Twitch channels), the room-settings panel (Shield Mode, subscribers / emote / unique-chat
//! toggles, followers-only and slow-mode choices) and the blocked-terms list. Every call goes through the bridge
//! like `relayApi(source, ...)`; errors show in the panel; the panel opens with fresh settings (`?fresh=1`) and the
//! button turns accent while a restricting mode is on.
//!
//! Differences: a `<select>` is a small drop-down list drawn under its field; with several channels the channel
//! picker cycles through them on click (a native select has no GPUI equivalent).
use std::cell::Cell;
use std::rc::Rc;

use agu_chat_core::protocol::{Platform, SourceRef};
use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::{
    anchored, deferred, div, prelude::*, px, AnyElement, Bounds, ClickEvent, Context, Corner, ElementId, Entity, FontWeight, KeyDownEvent, MouseDownEvent, Pixels,
    SharedString, Window,
};
use serde_json::{json, Value};

use crate::chat::input::slash::post_json;
use crate::chat::mod_caps::{fetch_mod_caps, relay_api, ModCaps};
use crate::chat::services::Services;
use crate::icons::{self, icon};
use crate::layout::tree::source_key;
use crate::layout::ui::toggle;
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

const FOLLOWER_MODE_OPTIONS: [(i64, &str); 9] = [(-1, "Off"), (0, "Any"), (10, "10m"), (30, "30m"), (60, "1h"), (1440, "1d"), (10080, "1w"), (43200, "1mo"), (129600, "3mo")];
const SLOW_MODE_OPTIONS: [i64; 8] = [0, 3, 5, 10, 20, 30, 60, 120];

fn can_moderate(c: Option<&ModCaps>) -> bool {
    c.map(|c| c.modes || c.shield || c.terms).unwrap_or(false)
}

/// The room settings of Helix's `GET /chats/settings`, as the relay returns them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Modes {
    pub emote_mode: bool,
    pub follower_mode: bool,
    pub follower_mode_duration: Option<i64>,
    pub slow_mode: bool,
    pub slow_mode_wait_time: Option<i64>,
    pub subscriber_mode: bool,
    pub unique_chat_mode: bool,
}

pub fn parse_modes(v: &Value) -> Modes {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let n = |k: &str| v.get(k).and_then(|x| x.as_i64());
    Modes {
        emote_mode: b("emote_mode"),
        follower_mode: b("follower_mode"),
        follower_mode_duration: n("follower_mode_duration"),
        slow_mode: b("slow_mode"),
        slow_mode_wait_time: n("slow_mode_wait_time"),
        subscriber_mode: b("subscriber_mode"),
        unique_chat_mode: b("unique_chat_mode"),
    }
}

/// `restricted`: a mode that limits who can talk is on (or Shield Mode).
pub fn restricted(m: Option<&Modes>, shield: bool) -> bool {
    shield || m.map(|m| m.emote_mode || m.follower_mode || m.slow_mode || m.subscriber_mode || m.unique_chat_mode).unwrap_or(false)
}

/// `withCurrent`: the options, plus the channel's current value when the list does not have it (set elsewhere).
pub fn with_current(opts: &[(i64, String)], current: i64, label: impl Fn(i64) -> String) -> Vec<(i64, String)> {
    if opts.iter().any(|(v, _)| *v == current) {
        return opts.to_vec();
    }
    let mut v = opts.to_vec();
    v.push((current, label(current)));
    v.sort_by_key(|(v, _)| *v);
    v
}

fn err_text(e: String) -> String {
    if e.is_empty() { "update failed".into() } else { e }
}

#[derive(Default)]
struct Terms {
    open: bool,
    loading: bool,
    list: Vec<(String, String)>,
    input: Option<Entity<TextInput>>,
    busy: bool,
    error: String,
}

pub struct ModesButton {
    twitch: Vec<SourceRef>,
    caps: std::collections::HashMap<String, ModCaps>,
    pick: Option<String>,
    open: bool,
    modes: Option<Modes>,
    shield: bool,
    loading: bool,
    busy: bool,
    error: String,
    select_open: Option<&'static str>,
    terms: Terms,
    anchor: Rc<Cell<Bounds<Pixels>>>,
    aid_done: bool,
    _tasks: Vec<gpui::Task<()>>,
}

impl ModesButton {
    pub fn new(sources: &[SourceRef], cx: &mut Context<Self>) -> Self {
        let twitch: Vec<SourceRef> = sources.iter().filter(|s| s.platform == Platform::Twitch).cloned().collect();
        let mut v = ModesButton {
            twitch,
            caps: Default::default(),
            pick: None,
            open: false,
            modes: None,
            shield: false,
            loading: false,
            busy: false,
            error: String::new(),
            select_open: None,
            terms: Terms::default(),
            anchor: Rc::new(Cell::new(Bounds::default())),
            aid_done: false,
            _tasks: vec![],
        };
        // Caps per channel, again whenever the account changes (log in / out / new scopes).
        let bridge = cx.global::<Services>().bridge.clone();
        let (tx, mut rx) = unbounded::<()>();
        let unsub = bridge.on_accounts(Arc_cb(tx));
        let task = cx.spawn(async move |this, cx| {
            let _keep = Unsub(Some(unsub));
            loop {
                if this.update(cx, |v, cx| v.load_caps(cx)).is_err() {
                    break;
                }
                if rx.next().await.is_none() {
                    break;
                }
            }
        });
        v._tasks.push(task);
        v
    }

    fn modded(&self) -> Vec<SourceRef> {
        self.twitch.iter().filter(|s| can_moderate(self.caps.get(&source_key(s)))).cloned().collect()
    }

    fn source(&self) -> Option<SourceRef> {
        let m = self.modded();
        m.iter().find(|s| Some(source_key(s)) == self.pick).cloned().or_else(|| m.first().cloned())
    }

    fn load_caps(&mut self, cx: &mut Context<Self>) {
        let bridge = cx.global::<Services>().bridge.clone();
        let list = self.twitch.clone();
        cx.spawn(async move |this, cx| {
            let mut out = Vec::new();
            for s in list {
                out.push((source_key(&s), fetch_mod_caps(&bridge, &s).await));
            }
            let _ = this.update(cx, |v, cx| {
                v.caps = out.into_iter().collect();
                v.load(false, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// The current modes, for the button's colour; the panel reloads them fresh when it opens.
    fn load(&mut self, fresh: bool, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        let c = self.caps.get(&source_key(&src)).cloned().unwrap_or_default();
        cx.spawn(async move |this, cx| {
            let m = if c.modes {
                let r = relay_api(&bridge, &src, &format!("/api/chat/settings{}", if fresh { "?fresh=1" } else { "" }), None, None).await;
                r.ok.then(|| parse_modes(&r.body))
            } else {
                None
            };
            let sh = if c.shield {
                let r = relay_api(&bridge, &src, "/api/chat/shield", None, None).await;
                r.ok.then(|| r.body.get("is_active").and_then(|x| x.as_bool()).unwrap_or(false))
            } else {
                None
            };
            let _ = this.update(cx, |v, cx| {
                if m.is_some() {
                    v.modes = m;
                }
                if let Some(s) = sh {
                    v.shield = s;
                }
                v.loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    fn open_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = true;
        self.error.clear();
        self.loading = true;
        self.select_open = None;
        if self.terms.input.is_none() {
            let input = cx.new(|cx| {
                let mut t = TextInput::new(window, cx, "Add a blocked term", InputStyle::Field);
                t.set_max_chars(Some(100));
                t
            });
            cx.subscribe_in(&input, window, |this, _i, ev: &TextInputEvent, _w, cx| match ev {
                TextInputEvent::Enter => this.add_term(cx),
                TextInputEvent::Changed => cx.notify(),
                _ => {}
            })
            .detach();
            self.terms.input = Some(input);
        }
        self.load(true, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.select_open = None;
        cx.notify();
    }

    fn patch(&mut self, body: Value, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        self.busy = true;
        self.error.clear();
        self.select_open = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = post_json(&bridge, &src, "/api/chat/settings", Some(body), "POST").await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(r) => v.modes = Some(parse_modes(&r.body)),
                    Err(e) => v.error = err_text(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_shield(&mut self, on: bool, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        self.busy = true;
        self.error.clear();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let r = post_json(&bridge, &src, "/api/chat/shield", Some(json!({"is_active": on})), "PUT").await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(r) => v.shield = r.body.get("is_active").and_then(|x| x.as_bool()).unwrap_or(on),
                    Err(e) => v.error = err_text(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn load_terms(&mut self, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        self.terms.loading = true;
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &src, "/api/chat/terms", None, None).await;
            let _ = this.update(cx, |v, cx| {
                v.terms.loading = false;
                if r.ok {
                    v.terms.list = r.body.get("terms").and_then(|t| t.as_array()).map(|a| a.iter().take(100).map(|t| (t.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(), t.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string())).collect()).unwrap_or_default();
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn add_term(&mut self, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        let Some(input) = self.terms.input.clone() else { return };
        let text = input.read(cx).text().trim().to_string();
        if text.is_empty() || self.terms.busy {
            return;
        }
        self.terms.busy = true;
        self.terms.error.clear();
        cx.spawn(async move |this, cx| {
            let r = post_json(&bridge, &src, "/api/chat/terms", Some(json!({"text": text})), "POST").await;
            let _ = this.update(cx, |v, cx| {
                v.terms.busy = false;
                match r {
                    Ok(_) => {
                        if let Some(i) = &v.terms.input {
                            i.update(cx, |i, cx| i.set_text("", cx));
                        }
                        v.load_terms(cx);
                    }
                    Err(e) => v.terms.error = err_text(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_term(&mut self, id: String, cx: &mut Context<Self>) {
        let (Some(src), bridge) = (self.source(), cx.global::<Services>().bridge.clone()) else { return };
        self.terms.busy = true;
        self.terms.error.clear();
        cx.spawn(async move |this, cx| {
            let r = post_json(&bridge, &src, &format!("/api/chat/terms?id={}", id.replace('%', "%25").replace('&', "%26")), None, "DELETE").await;
            let _ = this.update(cx, |v, cx| {
                v.terms.busy = false;
                match r {
                    Ok(_) => v.terms.list.retain(|(i, _)| *i != id),
                    Err(e) => v.terms.error = err_text(e),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    // ---- panel pieces ---------------------------------------------------------------------------------------

    fn row(label: &str) -> gpui::Div {
        div().flex().items_center().justify_between().gap(px(12.)).text_color(c(theme::TEXT)).child(SharedString::from(label.to_string()))
    }

    /// A `<select>`: its current label, and the choices as a list under it while open.
    fn select(&self, key: &'static str, current: i64, options: Vec<(i64, String)>, disabled: bool, on_pick: impl Fn(&mut Self, i64, &mut Context<Self>) + Clone + 'static, cx: &mut Context<Self>) -> AnyElement {
        let label = options.iter().find(|(v, _)| *v == current).map(|(_, l)| l.clone()).unwrap_or_default();
        let open = self.select_open == Some(key);
        let mut wrap = div().relative().flex_none().child(
            div()
                .id(key)
                .w(px(96.))
                .flex()
                .items_center()
                .justify_between()
                .rounded(px(4.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::APP))
                .px(px(8.))
                .py(px(2.))
                .text_size(px(12.))
                .line_height(px(18.))
                .when(disabled, |d| d.opacity(0.5))
                .child(SharedString::from(label))
                .child(div().text_color(c(theme::MUTED)).child(icon(icons::CHEVRON, 12.)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    if !disabled {
                        this.select_open = if this.select_open == Some(key) { None } else { Some(key) };
                        cx.notify();
                    }
                })),
        );
        if open {
            let mut list = div()
                .id(ElementId::from(SharedString::from(format!("{key}-list"))))
                .absolute()
                .top(px(26.))
                .right_0()
                .w(px(96.))
                .max_h(px(180.))
                .overflow_y_scroll()
                .occlude()
                .rounded(px(4.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::RAISED))
                .py(px(2.))
                .shadow_xl();
            for (i, (v, l)) in options.into_iter().enumerate() {
                let pick = on_pick.clone();
                list = list.child(
                    div()
                        .id(ElementId::from(SharedString::from(format!("{key}-{i}"))))
                        .px(px(8.))
                        .py(px(2.))
                        .text_size(px(12.))
                        .line_height(px(18.))
                        .text_color(if v == current { c(theme::ACCENT_SOFT) } else { c(theme::TEXT) })
                        .hover(|s| s.bg(c(theme::RAISED_2)))
                        .child(SharedString::from(l))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| pick(this, v, cx))),
                );
            }
            wrap = wrap.child(list);
        }
        wrap.into_any_element()
    }

    fn terms_section(&self, caps: &ModCaps, cx: &mut Context<Self>) -> AnyElement {
        let open = self.terms.open;
        let mut s = div().border_t_1().border_color(c(theme::LINE)).pt(px(10.)).child(
            div()
                .id("terms-head")
                .flex()
                .w_full()
                .items_center()
                .justify_between()
                .text_size(px(10.))
                .line_height(px(15.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(theme::FAINT))
                .hover(|st| st.text_color(c(theme::TEXT)))
                .child(crate::chat::inline::tracked("BLOCKED TERMS"))
                .child(if open { "\u{25be}" } else { "\u{25b8}" })
                .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                    this.terms.open = !this.terms.open;
                    if this.terms.open && this.caps.get(&this.source().map(|s| source_key(&s)).unwrap_or_default()).map(|c| c.terms).unwrap_or(false) {
                        this.load_terms(cx);
                    }
                    cx.notify();
                })),
        );
        if open {
            let mut body = div().mt(px(6.)).flex().flex_col().gap(px(8.));
            if !caps.terms {
                body = body.child(div().text_color(c(theme::AMBER_300)).child("Log in again to manage blocked terms."));
            }
            if self.terms.loading {
                body = body.child(div().h(px(14.)).w(px(96.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1)));
            } else {
                let mut list = div().id("terms-list").max_h(px(128.)).overflow_y_scroll().flex().flex_col().gap(px(4.));
                if self.terms.list.is_empty() {
                    list = list.child(div().text_color(c(theme::FAINT)).child("No blocked terms."));
                }
                for (i, (id, text)) in self.terms.list.iter().enumerate() {
                    let id = id.clone();
                    let off = !caps.terms || self.terms.busy;
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_color(c(theme::TEXT))
                            .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(text.clone())))
                            .child(
                                div()
                                    .id(("term-x", i))
                                    .flex_none()
                                    .rounded(px(4.))
                                    .p(px(2.))
                                    .text_color(c(theme::FAINT))
                                    .when(off, |d| d.opacity(0.4))
                                    .when(!off, |d| d.hover(|s| s.text_color(c(theme::RED_400))))
                                    .child(icon(icons::X, 14.))
                                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                        if this.caps.get(&this.source().map(|s| source_key(&s)).unwrap_or_default()).map(|c| c.terms).unwrap_or(false) && !this.terms.busy {
                                            this.remove_term(id.clone(), cx);
                                        }
                                    })),
                            ),
                    );
                }
                body = body.child(list);
            }
            let has_text = self.terms.input.as_ref().map(|i| !i.read(cx).text().trim().is_empty()).unwrap_or(false);
            let can_add = caps.terms && !self.terms.busy && has_text;
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().flex_1().children(self.terms.input.clone()))
                    .child(
                        div()
                            .id("term-add")
                            .flex_none()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c(theme::LINE))
                            .px(px(8.))
                            .py(px(4.))
                            .text_color(c(theme::TEXT))
                            .when(!can_add, |d| d.opacity(0.4))
                            .when(can_add, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
                            .child("Add")
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.add_term(cx))),
                    ),
            );
            if !self.terms.error.is_empty() {
                body = body.child(div().text_color(c(theme::RED_300)).child(SharedString::from(self.terms.error.clone())));
            }
            body = body.child(div().text_size(px(11.)).line_height(px(16.)).text_color(c(theme::FAINT)).child("Permitted terms aren't exposed by Twitch's API."));
            s = s.child(body);
        }
        s.into_any_element()
    }

    fn panel(&self, source: &SourceRef, caps: &ModCaps, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let modded = self.modded();
        let disabled = !caps.modes || self.busy;
        let b = self.anchor.get();
        // Prefer right-aligned with the button, but never past either edge; flip above when there is no room below.
        let vh = f32::from(window.viewport_size().height);
        let below = vh - f32::from(b.bottom()) - 8.0 - 6.0;
        let above = f32::from(b.top()) - 8.0 - 6.0;
        let flip = below < 220.0 && above > below;
        let max_h = if flip { above } else { below }.clamp(140.0, 560.0);
        let mut p = div()
            .id("modes-panel")
            .occlude()
            .w(px(288.))
            .max_h(px(max_h))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(12.))
            .shadow_2xl()
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .font_family(theme::FONT)
            .on_mouse_down_out(cx.listener(|this, ev: &MouseDownEvent, _w, cx| {
                // the button toggles
                if !this.anchor.get().contains(&ev.position) {
                    this.close(cx);
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key == "escape" {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }));
        let head_label: AnyElement = if modded.len() > 1 {
            let keys: Vec<String> = modded.iter().map(source_key).collect();
            let cur = source_key(source);
            div()
                .id("modes-channel")
                .min_w_0()
                .flex_1()
                .flex()
                .items_center()
                .justify_between()
                .rounded(px(4.))
                .border_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::APP))
                .px(px(8.))
                .py(px(2.))
                .child(SharedString::from(source.channel.clone()))
                .child(div().text_color(c(theme::MUTED)).child(icon(icons::CHEVRON, 12.)))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    let i = keys.iter().position(|k| *k == cur).unwrap_or(0);
                    this.pick = Some(keys[(i + 1) % keys.len()].clone());
                    this.modes = None;
                    this.shield = false;
                    this.loading = true;
                    this.terms = Terms { input: this.terms.input.clone(), open: this.terms.open, ..Default::default() };
                    this.load(true, cx);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(format!("{} channel modes", source.channel))).into_any_element()
        };
        p = p.child(
            div().flex().items_center().gap(px(8.)).child(head_label).child(
                div().id("modes-x").flex_none().rounded(px(4.)).p(px(2.)).text_color(c(theme::FAINT)).hover(|s| s.text_color(c(theme::TEXT))).child(icon(icons::X, 14.)).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.close(cx))),
            ),
        );
        let shield_off = !caps.shield || self.busy || self.loading;
        p = p.child(Self::row("Shield Mode").child(toggle("shield-t", self.shield, shield_off).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
            if !shield_off {
                let on = !this.shield;
                this.toggle_shield(on, cx);
            }
        }))));
        if !caps.modes {
            p = p.child(div().text_color(c(theme::AMBER_300)).child("Log in again to enable chat mode controls."));
        }
        match (&self.modes, self.loading) {
            (None, true) => {
                p = p.child(div().flex().flex_col().gap(px(8.)).child(div().h(px(14.)).w(px(128.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1))).child(div().h(px(14.)).w(px(96.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1))));
            }
            (Some(m), _) => {
                let tg = |id: &'static str, label: &str, on: bool, key: &'static str, this: &Self, cx: &mut Context<Self>| {
                    let _ = this;
                    Self::row(label).child(toggle(id, on, disabled).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        if !disabled {
                            this.patch(json!({ key: !on }), cx);
                        }
                    })))
                };
                p = p.child(tg("t-sub", "Subscribers-Only", m.subscriber_mode, "subscriber_mode", self, cx));
                p = p.child(tg("t-emote", "Emote-Only", m.emote_mode, "emote_mode", self, cx));
                p = p.child(tg("t-uniq", "Unique (R9K)", m.unique_chat_mode, "unique_chat_mode", self, cx));
                let fcur = if m.follower_mode { m.follower_mode_duration.unwrap_or(0) } else { -1 };
                let fopts = with_current(&FOLLOWER_MODE_OPTIONS.iter().map(|(v, l)| (*v, l.to_string())).collect::<Vec<_>>(), fcur, |v| format!("{v}m"));
                p = p.child(Self::row("Followers-Only").child(self.select("sel-follow", fcur, fopts, disabled, |this, v, cx| this.patch(if v < 0 { json!({"follower_mode": false}) } else { json!({"follower_mode": true, "follower_mode_duration": v}) }, cx), cx)));
                let scur = if m.slow_mode { m.slow_mode_wait_time.unwrap_or(0) } else { 0 };
                let sopts = with_current(&SLOW_MODE_OPTIONS.iter().map(|s| (*s, if *s == 0 { "Off".to_string() } else { format!("{s}s") })).collect::<Vec<_>>(), scur, |v| format!("{v}s"));
                p = p.child(Self::row("Slow Mode").child(self.select("sel-slow", scur, sopts, disabled, |this, v, cx| this.patch(if v <= 0 { json!({"slow_mode": false}) } else { json!({"slow_mode": true, "slow_mode_wait_time": v}) }, cx), cx)));
            }
            _ => {}
        }
        if !self.error.is_empty() {
            p = p.child(div().text_color(c(theme::RED_300)).child(SharedString::from(self.error.clone())));
        }
        p = p.child(self.terms_section(caps, cx));
        let corner = if flip { Corner::BottomRight } else { Corner::TopRight };
        let pos = if flip { gpui::point(b.right(), b.top() - px(6.)) } else { gpui::point(b.right(), b.bottom() + px(6.)) };
        deferred(anchored().position(pos).anchor(corner).snap_to_window_with_margin(gpui::Edges::all(px(8.))).child(p)).with_priority(5).into_any_element()
    }
}

impl Render for ModesButton {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(source) = self.source() else { return div().into_any_element() };
        // `AGU_SHOT=channelmodes` (test aid): open the panel without a click
        if !self.aid_done && std::env::var("AGU_SHOT").map(|v| v == "channelmodes").unwrap_or(false) && self.anchor.get().size.width > px(0.) {
            self.aid_done = true;
            self.open_panel(window, cx);
            if std::env::var("AGU_TERMS").is_ok() {
                self.terms.open = true;
                self.load_terms(cx);
            }
        }
        let caps = self.caps.get(&source_key(&source)).cloned().unwrap_or_default();
        let on = restricted(self.modes.as_ref(), self.shield);
        let anchor = self.anchor.clone();
        let open = self.open;
        let mut root = div()
            .relative()
            .flex_none()
            .on_children_prepainted(move |bounds, _w, _cx| {
                if let Some(b) = bounds.first() {
                    anchor.set(*b);
                }
            })
            .child(
                div()
                    .id("hdr-modes")
                    .flex()
                    .size(px(20.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .text_color(if on { c(theme::ACCENT_SOFT) } else { c(theme::FAINT) })
                    .hover(|s| s.bg(c(theme::RAISED_2)).text_color(if on { c(theme::ACCENT_SOFT) } else { c(theme::TEXT) }))
                    .tooltip(crate::layout::ui::tip(if on { "Chat modes (a mode is on)" } else { "Chat modes" }))
                    .child(icon(icons::SHIELD, 14.))
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                        if this.open {
                            this.close(cx);
                        } else {
                            this.open_panel(w, cx);
                        }
                    })),
            );
        if open {
            root = root.child(self.panel(&source, &caps, window, cx));
        }
        root.into_any_element()
    }
}

/// An `on_accounts` callback that pings the caps loader.
#[allow(non_snake_case)]
fn Arc_cb(tx: futures::channel::mpsc::UnboundedSender<()>) -> std::sync::Arc<dyn Fn(&agu_chat_core::protocol::AccountsState) + Send + Sync> {
    std::sync::Arc::new(move |_| {
        let _ = tx.unbounded_send(());
    })
}

/// Runs the subscription's unsubscribe when dropped.
struct Unsub(Option<agu_chat_core::protocol::Unsubscribe>);
impl Drop for Unsub {
    fn drop(&mut self) {
        if let Some(u) = self.0.take() {
            u();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_helix_settings_and_flags_restricting_modes() {
        let m = parse_modes(&json!({"emote_mode": false, "follower_mode": true, "follower_mode_duration": 30, "slow_mode": false, "slow_mode_wait_time": null, "subscriber_mode": false, "unique_chat_mode": null}));
        assert_eq!(m.follower_mode_duration, Some(30));
        assert!(restricted(Some(&m), false));
        assert!(!restricted(Some(&Modes::default()), false));
        assert!(restricted(Some(&Modes::default()), true));
        assert!(!restricted(None, false));
    }

    #[test]
    fn a_value_set_elsewhere_joins_the_options_in_order() {
        let opts: Vec<(i64, String)> = vec![(-1, "Off".into()), (0, "Any".into()), (10, "10m".into()), (30, "30m".into())];
        assert_eq!(with_current(&opts, 10, |v| format!("{v}m")), opts);
        let w = with_current(&opts, 20, |v| format!("{v}m"));
        assert_eq!(w.iter().map(|(v, _)| *v).collect::<Vec<_>>(), vec![-1, 0, 10, 20, 30]);
        assert_eq!(w[3].1, "20m");
    }

    #[test]
    fn only_someone_with_a_mode_right_sees_the_button() {
        assert!(!can_moderate(None));
        assert!(!can_moderate(Some(&ModCaps::default())));
        assert!(can_moderate(Some(&ModCaps { shield: true, ..Default::default() })));
        assert!(can_moderate(Some(&ModCaps { terms: true, ..Default::default() })));
    }
}
