//! Port of src/renderer/chat/UserCard.tsx: the mod control centre for one person, drawn in its own pop-out window
//! (`windowed`). One card for every platform: Twitch gets the `/api/mod/user` info plus the timeout ladder /
//! ban / unban / warn when the account has the rights; Rumble and Kick get their mod actions; every platform
//! gets the profile link, "Ignore user" and the person's recent lines in this split.
//!
//! The card reads the pane (rows, caps, ignores) when it draws and acts through it (toasts, deletes, the
//! Rumble / Kick ban confirmation), so what happens in the card shows in the chat window like it did in the DOM.
use std::collections::BTreeSet;
use std::sync::Arc;

use agu_chat_core::protocol::{Platform, SourceRef};
use gpui::{
    div, prelude::*, px, AnyElement, ClickEvent, Context, ElementId, Entity, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, SharedString, WeakEntity, Window,
    WindowControlArea,
};
use serde_json::{json, Value};

use super::chat_pane::ChatPane;
use super::inline::{inline_flow, InlineContent};
use super::mod_actions::{self, ToastMsg};
use super::mod_caps::{relay_api, ModCaps, ModReach, TIMEOUT_LADDER};
use super::pane_render::{ImageBox, InlineContentBuilder};
use super::rows::{CardUser, RowAction};
use super::services::Services;
use super::twitch::emotes::{EmoteSets, SizeOpt};
use super::twitch::parts::build_parts;
use super::types::*;
use super::ui_util::{fmt_time, token};
use crate::icons::{self, icon};
use super::ui_util::cbtn as btn;
use crate::layout::ui::BtnOpts;
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

/// Everything the card needs from the pane, copied out when it draws.
pub struct CardCtx {
    pub source: SourceRef,
    pub caps: ModCaps,
    pub reach: ModReach,
    pub suspicious: Option<SuspiciousUser>,
    pub co_can_moderate: bool,
    pub ignored: bool,
    /// the person has a name to ignore by
    pub can_ignore: bool,
    pub emotes: Option<Arc<EmoteSets>>,
    /// newest first, at most 15
    pub recent: Vec<RowRef>,
    pub readable_colors: bool,
}

/// `suspiciousDetails`: spell out why Twitch flagged this user, for the display-only info line.
pub fn suspicious_details(u: &SuspiciousUser) -> String {
    let mut parts: Vec<String> = Vec::new();
    if u.types.iter().any(|t| t == "ban_evader") {
        parts.push(match u.evaluation.as_deref() {
            Some("likely") => "likely ban evader",
            Some("possible") => "possible ban evader",
            _ => "ban evader",
        }
        .to_string());
    }
    if u.types.iter().any(|t| t == "manually_added") {
        parts.push("manually added by a moderator".into());
    }
    if u.types.iter().any(|t| t == "banned_in_shared_channel") {
        let n = u.shared_ban_channels as i64;
        parts.push(format!("banned in {n} shared channel{}", if n == 1 { "" } else { "s" }));
    }
    parts.join(", ")
}

pub fn profile_url(p: Platform, login: &str) -> Option<String> {
    match p {
        Platform::Twitch => Some(format!("https://twitch.tv/{login}")),
        Platform::Kick => Some(format!("https://kick.com/{login}")),
        Platform::Rumble => Some(format!("https://rumble.com/user/{login}")),
        Platform::X => Some(format!("https://x.com/{login}")),
        _ => None,
    }
}

/// `toLocaleDateString()` / `toLocaleTimeString()` of an ISO timestamp (en-US shapes).
fn local_date(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.with_timezone(&chrono::Local).format("%-m/%-d/%Y").to_string()).unwrap_or_else(|_| "Invalid Date".into())
}
fn local_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.with_timezone(&chrono::Local).format("%-I:%M:%S %p").to_string()).unwrap_or_else(|_| "Invalid Date".into())
}

#[derive(Clone, Debug, Default)]
struct ModUser {
    login: String,
    display_name: String,
    avatar: String,
    created_at: String,
    followed_at: Option<String>,
    follow_error: bool,
    ban: Option<(bool, Option<String>, String)>,
    ban_error: Option<String>,
}

fn parse_mod_user(v: &Value) -> ModUser {
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string();
    ModUser {
        login: s("/user/login"),
        display_name: s("/user/displayName"),
        avatar: s("/user/avatar"),
        created_at: s("/user/createdAt"),
        followed_at: v.get("followedAt").and_then(|x| x.as_str()).map(|x| x.to_string()),
        follow_error: v.get("followError").map(|x| !x.is_null() && x != &Value::Bool(false) && x != &Value::from("")).unwrap_or(false),
        ban: v.get("ban").filter(|b| b.is_object()).map(|b| (b.get("permanent").and_then(|x| x.as_bool()).unwrap_or(false), b.get("expiresAt").and_then(|x| x.as_str()).map(|x| x.to_string()), b.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string())),
        ban_error: v.get("banError").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(|x| x.to_string()),
    }
}

pub struct UserCardView {
    pane: WeakEntity<ChatPane>,
    pub id: String,
    pub sk: String,
    pub platform: Platform,
    user: CardUser,
    data: Option<ModUser>,
    relogin: bool,
    loading: bool,
    reason_mode: bool,
    reason_in: Entity<TextInput>,
    busy: bool,
    warn_open: bool,
    rules: Vec<String>,
    selected_rules: BTreeSet<String>,
    custom_in: Entity<TextInput>,
    /// The window height we last set (`fit_height`), and whether the user resized the window by hand since:
    /// then the card fills the window (`PopoutFill`) instead of the window following the card.
    fitted_h: f32,
    user_sized: bool,
    mismatches: u8,
    focus: gpui::FocusHandle,
    _subs: Vec<gpui::Subscription>,
}

impl UserCardView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, pane: WeakEntity<ChatPane>, id: String, sk: String, platform: Platform, user: CardUser) -> Self {
        let reason_in = cx.new(|cx| {
            let mut t = TextInput::new(window, cx, "Reason (optional)", InputStyle::Chat);
            t.set_max_chars(Some(500));
            t
        });
        let custom_in = cx.new(|cx| {
            let mut t = TextInput::new(window, cx, "Custom reason", InputStyle::Chat);
            t.set_max_chars(Some(500));
            t
        });
        let subs = vec![cx.observe(&reason_in, |_, _, cx| cx.notify()), cx.observe(&custom_in, |_, _, cx| cx.notify())];
        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut v = UserCardView {
            pane,
            id,
            sk,
            platform,
            loading: platform == Platform::Twitch,
            user,
            data: None,
            relogin: false,
            reason_mode: false,
            reason_in,
            busy: false,
            warn_open: false,
            rules: vec![],
            selected_rules: BTreeSet::new(),
            custom_in,
            fitted_h: 0.0,
            user_sized: false,
            mismatches: 0,
            focus,
            _subs: subs,
        };
        v.load(cx);
        v
    }

    fn fit_state(&self) -> (bool, f32, u8) {
        (self.user_sized, self.fitted_h, self.mismatches)
    }

    fn pane_ctx(&self, cx: &gpui::App) -> Option<CardCtx> {
        self.pane.upgrade().and_then(|p| p.read(cx).card_ctx(&self.sk, &self.user, self.platform))
    }

    fn toast(&self, t: ToastMsg, cx: &mut Context<Self>) {
        let _ = self.pane.update(cx, |p, cx| p.show_toast(t, cx));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.platform != Platform::Twitch || self.user.id.is_empty() {
            self.loading = false;
            return;
        }
        let Some(ctx) = self.pane_ctx(cx) else { return };
        self.loading = true;
        self.relogin = false;
        let bridge = cx.global::<Services>().bridge.clone();
        let id = self.user.id.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, &format!("/api/mod/user?id={}", id.replace('%', "%25").replace('&', "%26")), None, None).await;
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                if r.status == 403 && r.text().contains("relogin") {
                    v.relogin = true;
                    v.data = None;
                } else if r.status == 401 {
                    v.data = None; // logged out: the card just has no account info
                } else if !r.ok {
                    v.toast(Some((format!("Couldn't load user info: {}", r.text()), true)), cx);
                } else {
                    v.data = Some(parse_mod_user(&r.body));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn load_rules(&mut self, cx: &mut Context<Self>) {
        let Some(ctx) = self.pane_ctx(cx) else { return };
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, "/api/chat/rules", None, None).await;
            let rules: Vec<String> = if r.ok { r.body.get("rules").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect()).unwrap_or_default() } else { vec![] };
            let _ = this.update(cx, |v, cx| {
                v.rules = rules;
                cx.notify();
            });
        })
        .detach();
    }

    /// `act`: a Twitch mod call, then reload the account info.
    fn act(&mut self, body: Value, ok: &str, on_success: impl FnOnce(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) {
        let Some(ctx) = self.pane_ctx(cx) else { return };
        self.busy = true;
        cx.notify();
        let bridge = cx.global::<Services>().bridge.clone();
        let ok = ok.to_string();
        cx.spawn(async move |this, cx| {
            let (done, t) = mod_actions::twitch_action(&bridge, &ctx.source, body, Some(ok)).await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.toast(t, cx);
                if done {
                    v.reason_mode = false;
                    v.reason_in.update(cx, |i, cx| i.set_text("", cx));
                    on_success(v, cx);
                    v.load(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submit_warn(&mut self, cx: &mut Context<Self>) {
        let custom = self.custom_in.read(cx).text();
        let mut parts: Vec<String> = self.selected_rules.iter().cloned().collect();
        parts.push(custom.trim().to_string());
        let reason: String = parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ").chars().take(500).collect();
        let uid = self.user.id.clone();
        self.act(
            json!({"action": "warn", "userId": uid, "reason": reason}),
            "warned",
            |v, cx| {
                v.warn_open = false;
                v.selected_rules.clear();
                v.custom_in.update(cx, |i, cx| i.set_text("", cx));
            },
            cx,
        );
    }

    fn close(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        window.remove_window();
    }

    // ---- small widgets ------------------------------------------------------------------------------------

    /// `rounded border border-line px-1.5 py-0.5 text-[11px] text-muted hover:border-amber-500/50 hover:text-amber-300`
    fn ladder_btn(id: String, label: &str, disabled: bool) -> gpui::Stateful<gpui::Div> {
        div()
            .id(ElementId::from(SharedString::from(id)))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .px(px(6.))
            .py(px(2.))
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(c(theme::MUTED))
            .when(disabled, |d| d.opacity(0.4))
            .when(!disabled, |d| d.hover(|s| s.border_color(a(theme::AMBER_500, 0.5)).text_color(c(theme::AMBER_300))))
            .child(label.to_string())
    }

    fn heading(text: &str) -> gpui::Div {
        div().mb(px(4.)).text_size(px(10.)).line_height(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::FAINT)).child(crate::chat::inline::tracked(&text.to_uppercase()))
    }

    fn avatar(src: &str, name: &str) -> AnyElement {
        if !src.is_empty() {
            return ImageBox { url: src.to_string().into(), side: 36.0, radius: 18.0 }.into_any_element();
        }
        div()
            .size(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(c(theme::RAISED_2))
            .text_size(px(14.))
            .line_height(px(20.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c(theme::MUTED))
            .child(name.chars().next().map(|ch| ch.to_uppercase().to_string()).unwrap_or_else(|| "?".into()))
            .into_any_element()
    }

    fn recent_text(&self, r: &Row, ctx: &CardCtx) -> AnyElement {
        match &r.body {
            RowBody::Msg(m) => {
                let parts = build_parts(&m.text, &m.emotes, ctx.emotes.as_deref(), None, m.gifs.as_deref().unwrap_or(""));
                let mut b = InlineContentBuilder::new(12.0).ratio(16.0 / 12.0);
                b.message(&parts, SizeOpt::Small, ctx.emotes.as_deref(), token(theme::MUTED), FontWeight::NORMAL);
                let content: Rc<InlineContent> = b.finish();
                inline_flow(content).into_any_element()
            }
            RowBody::Kick(m) => div().child(SharedString::from(kick_plain(&m.text))).into_any_element(),
            RowBody::Rumble(m) => div().child(SharedString::from(rumble_plain(&m.text))).into_any_element(),
            RowBody::YouTube(m) => div().child(SharedString::from(m.text.clone())).into_any_element(),
            RowBody::X(m) => div().child(SharedString::from(m.text.clone())).into_any_element(),
            _ => div().into_any_element(),
        }
    }
}

use std::rc::Rc;

/// `splitKickEmotes` joined back: "[emote:id:name]" shows as its name.
fn kick_plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("[emote:") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(j) = tail.find(']') {
            let inner = &tail[7..j];
            match inner.split_once(':') {
                Some((_, name)) => out.push_str(name),
                None => out.push_str(&tail[..=j]),
            }
            rest = &tail[j + 1..];
        } else {
            out.push_str(tail);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

/// Rumble lines: ":name:" emotes are dropped from the text, like the DOM's text-only join.
fn rumble_plain(text: &str) -> String {
    text.to_string()
}

impl Render for UserCardView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(ctx) = self.pane_ctx(cx) else {
            // the split is gone: so is the card
            window.defer(cx, |w, _| w.remove_window());
            return div().into_any_element();
        };
        let is_twitch = self.platform == Platform::Twitch;
        let login = self.data.as_ref().map(|d| d.login.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.user.login.clone());
        let display = self.data.as_ref().map(|d| d.display_name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.user.name.clone());
        let avatar_src = self.data.as_ref().map(|d| d.avatar.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.user.avatar.clone());
        let profile = if !login.is_empty() { profile_url(self.platform, &login) } else { None };
        let banned = self.data.as_ref().and_then(|d| d.ban.clone());
        let caps = &ctx.caps;
        let reach = &ctx.reach;
        let show_twitch_mod = is_twitch && (reach.ban || reach.timeout || reach.warn || banned.is_some()) && (caps.ban || caps.warn);
        let busy = self.busy;
        let uid = self.user.id.clone();
        let entity = cx.entity();

        // ---- header
        let name_el: AnyElement = match &profile {
            Some(url) => {
                let url = url.clone();
                div()
                    .id("card-name")
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_size(px(14.))
                    .line_height(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(theme::TEXT))
                    .hover(|s| s.underline())
                    .cursor_pointer()
                    .child(SharedString::from(if display.is_empty() { login.clone() } else { display.clone() }))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let _ = this.pane.update(cx, |p, _| p.open_url(&url));
                    }))
                    .into_any_element()
            }
            None => div().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(14.)).line_height(px(20.)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(if display.is_empty() { "unknown".to_string() } else { display.clone() })).into_any_element(),
        };
        let header = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .border_b_1()
            .border_color(c(theme::LINE))
            .p(px(10.))
            // occlude: see notebook_tabs' window buttons (a focusable ancestor would swallow the press)
            .child(div().flex_none().window_control_area(WindowControlArea::Drag).occlude().child(Self::avatar(&avatar_src, &display)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(div().flex().items_center().gap(px(6.)).child(icons::platform_icon(self.platform, 14., true)).child(name_el))
                    .when(!display.is_empty() && !login.is_empty() && display.to_lowercase() != login.to_lowercase(), |d| d.child(div().text_size(px(11.)).line_height(px(16.)).text_color(c(theme::FAINT)).child(SharedString::from(format!("@{login}"))))),
            )
            .child(div().flex_none().h(px(20.)).w(px(4.)).window_control_area(WindowControlArea::Drag).occlude())
            .child(
                div()
                    .id("card-close")
                    .flex_none()
                    .rounded(px(4.))
                    .p(px(4.))
                    .text_color(c(theme::FAINT))
                    .hover(|s| s.text_color(c(theme::TEXT)))
                    .child(icon(icons::X, 16.))
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.close(w, cx))),
            );

        let mut card = div()
            .id("user-card")
            .w_full()
            .when(self.user_sized, |d| d.h_full())
            .flex()
            .flex_col()
            .overflow_hidden()
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(c(theme::TEXT))
            .font_family(theme::FONT)
            .child(header);

        // ---- Twitch account info
        if is_twitch {
            let mut info = div().flex().flex_col().gap(px(4.)).border_b_1().border_color(c(theme::LINE)).p(px(10.)).text_color(c(theme::MUTED));
            if self.relogin {
                info = info.child(div().text_color(c(theme::AMBER_300)).child("Log in again to enable mod tools."));
            } else if self.loading {
                info = info.child(div().h(px(12.)).w(px(160.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1))).child(div().h(px(12.)).w(px(128.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1)));
            } else if let Some(d) = &self.data {
                info = info.child(div().child(SharedString::from(format!("Account created {}", local_date(&d.created_at)))));
                // "Not following" only when we actually confirmed it: a failed lookup omits the line.
                if d.followed_at.is_some() || !d.follow_error {
                    info = info.child(div().child(SharedString::from(match &d.followed_at {
                        Some(f) => format!("Following since {}", local_date(f)),
                        None => "Not following".to_string(),
                    })));
                }
                if let Some((permanent, expires, reason)) = &d.ban {
                    let chip = if *permanent {
                        div().rounded_full().border_1().border_color(a(theme::RED_500, 0.3)).bg(a(theme::RED_500, 0.1)).px(px(8.)).text_size(px(11.)).line_height(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::RED_300)).child(SharedString::from(format!("Banned{}", if reason.is_empty() { String::new() } else { format!(": {reason}") })))
                    } else {
                        div().rounded_full().border_1().border_color(a(theme::AMBER_500, 0.3)).bg(a(theme::AMBER_500, 0.1)).px(px(8.)).text_size(px(11.)).line_height(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::AMBER_400)).child(SharedString::from(format!("Timed out until {}", expires.as_deref().map(local_time).unwrap_or_else(|| "?".into()))))
                    };
                    info = info.child(div().flex().child(chip));
                } else if let Some(e) = &d.ban_error {
                    info = info.child(div().text_color(c(theme::FAINT)).child(SharedString::from(format!("Ban status unknown: {e}"))));
                }
                if let Some(s) = &ctx.suspicious {
                    let det = suspicious_details(s);
                    info = info.child(
                        div().rounded(px(4.)).border_1().border_color(a(theme::ORANGE_500, 0.3)).bg(a(theme::ORANGE_500, 0.1)).px(px(8.)).py(px(4.)).text_color(c(theme::ORANGE_300)).child(SharedString::from(format!(
                            "Twitch flags this user: {}{}",
                            if s.status == "restricted" { "restricted" } else { "monitored" },
                            if det.is_empty() { String::new() } else { format!(" - {det}") }
                        ))),
                    );
                }
            } else {
                info = info.child(div().text_color(c(theme::FAINT)).child("No account info."));
            }
            card = card.child(info);
        }

        // ---- Twitch mod tools
        if show_twitch_mod {
            let mut m = div().flex().flex_col().gap(px(10.)).border_b_1().border_color(c(theme::LINE)).p(px(10.));
            if banned.is_none() && reach.timeout && caps.ban {
                let mut row = div().flex().flex_wrap().gap(px(4.));
                for (label, sec) in TIMEOUT_LADDER {
                    let (u, lbl) = (uid.clone(), label.to_string());
                    row = row.child(Self::ladder_btn(format!("tl-{label}"), label, busy).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        if !this.busy {
                            this.act(json!({"action": "timeout", "userId": u, "duration": sec}), &format!("timed out {lbl}"), |_, _| {}, cx);
                        }
                    })));
                }
                m = m.child(div().child(Self::heading("Timeout")).child(row));
            }
            if self.reason_mode {
                let u = uid.clone();
                m = m.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(div().min_w_0().flex_1().child(self.reason_in.clone()))
                        .child(btn("ban-ok", "Confirm", BtnOpts { primary: true, disabled: busy, ..Default::default() }).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            let r = this.reason_in.read(cx).text().trim().to_string();
                            let mut body = json!({"action": "ban", "userId": u});
                            if !r.is_empty() {
                                body["reason"] = json!(r);
                            }
                            this.act(body, "banned", |_, _| {}, cx);
                        })))
                        .child(btn("ban-cancel", "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.reason_mode = false;
                            this.reason_in.update(cx, |i, cx| i.set_text("", cx));
                            cx.notify();
                        }))),
                );
            } else if self.warn_open {
                let mut panel = div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(theme::LINE))
                    .bg(a(theme::BLACK, 0.2))
                    .p(px(8.))
                    .child(div().text_color(c(theme::MUTED)).child(SharedString::from(format!("Select chat rules the user has violated, or enter a custom reason to warn {}", if display.is_empty() { "this user".to_string() } else { display.clone() }))));
                if !self.rules.is_empty() {
                    let mut list = div().id("warn-rules").max_h(px(112.)).overflow_y_scroll().flex().flex_col().gap(px(4.));
                    for (i, rule) in self.rules.iter().enumerate() {
                        let on = self.selected_rules.contains(rule);
                        let rr = rule.clone();
                        list = list.child(
                            div()
                                .id(("rule", i))
                                .flex()
                                .items_start()
                                .gap(px(8.))
                                .text_color(c(theme::MUTED))
                                .child(
                                    div()
                                        .mt(px(2.))
                                        .flex_none()
                                        .size(px(13.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(2.))
                                        .border_1()
                                        .border_color(if on { c(theme::ACCENT) } else { c(theme::FAINT) })
                                        .when(on, |d| d.bg(c(theme::ACCENT)).text_color(c(theme::WHITE)))
                                        .when(on, |d| d.child(icon(icons::CHECK, 10.))),
                                )
                                .child(div().min_w_0().flex_1().child(SharedString::from(rule.clone())))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    if !this.selected_rules.remove(&rr) {
                                        this.selected_rules.insert(rr.clone());
                                    }
                                    cx.notify();
                                })),
                        );
                    }
                    panel = panel.child(list);
                }
                let can_warn = !busy && (!self.selected_rules.is_empty() || !self.custom_in.read(cx).text().trim().is_empty());
                panel = panel.child(self.custom_in.clone()).child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(btn("warn-ok", "Warn", BtnOpts { primary: true, disabled: !can_warn, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            let ok = !this.busy && (!this.selected_rules.is_empty() || !this.custom_in.read(cx).text().trim().is_empty());
                            if ok {
                                this.submit_warn(cx);
                            }
                        })))
                        .child(btn("warn-cancel", "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.warn_open = false;
                            this.selected_rules.clear();
                            this.custom_in.update(cx, |i, cx| i.set_text("", cx));
                            cx.notify();
                        }))),
                );
                m = m.child(panel);
            } else {
                let mut row = div().flex().flex_wrap().gap(px(6.));
                if reach.warn && caps.warn {
                    row = row.child(btn("warn-open", "Warn", BtnOpts { disabled: busy, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.warn_open = true;
                        this.load_rules(cx);
                        cx.notify();
                    })));
                }
                let ban_btn = |label: &'static str| btn("ban-open", label, BtnOpts { danger: true, disabled: busy, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                    this.reason_mode = true;
                    this.reason_in.read(cx).focus(w);
                    cx.notify();
                }));
                if let Some((permanent, _, _)) = &banned {
                    let (u, perm) = (uid.clone(), *permanent);
                    row = row.child(btn("unban", if perm { "Unban" } else { "Untimeout" }, BtnOpts { disabled: busy, ..Default::default() }).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.act(json!({"action": "unban", "userId": u}), if perm { "unbanned" } else { "untimed out" }, |_, _| {}, cx);
                    })));
                    if !perm && reach.ban && caps.ban {
                        row = row.child(ban_btn("Ban"));
                    }
                } else if reach.ban && caps.ban {
                    row = row.child(ban_btn("Ban"));
                }
                m = m.child(row);
            }
            card = card.child(m);
        }

        // ---- Rumble / Kick mod
        let last = ctx.recent.first().cloned();
        if self.platform == Platform::Rumble && ctx.co_can_moderate {
            if let Some(Row { body: RowBody::Rumble(m), .. }) = last.as_deref() {
                let (sk1, m1, sk2, m2) = (self.sk.clone(), m.clone(), self.sk.clone(), m.clone());
                card = card.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .border_b_1()
                        .border_color(c(theme::LINE))
                        .p(px(10.))
                        .child(btn("r-to", "Timeout 5m", BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            let (sk, m) = (sk1.clone(), m1.clone());
                            let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::RumbleMod { sk, action: "timeout", msg: Box::new(m), seconds: 300 }, cx));
                        })))
                        .child(btn("r-ban", "Ban", BtnOpts::danger()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            let (sk, m) = (sk2.clone(), m2.clone());
                            let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::RumbleMod { sk, action: "ban", msg: Box::new(m), seconds: 300 }, cx));
                        }))),
                );
            }
        }
        if self.platform == Platform::Kick && ctx.co_can_moderate && !self.user.id.is_empty() {
            if let Some(Row { body: RowBody::Kick(m), .. }) = last.as_deref() {
                let mut row = div().mb(px(8.)).flex().flex_wrap().gap(px(4.));
                for (label, sec) in [("1m", 60u32), ("5m", 300), ("10m", 600), ("1h", 3600), ("24h", 86400), ("7d", 604800)] {
                    let (sk, m) = (self.sk.clone(), m.clone());
                    row = row.child(Self::ladder_btn(format!("k-{label}"), label, false).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let (sk, m) = (sk.clone(), m.clone());
                        let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "timeout", msg: Box::new(m), seconds: Some(sec) }, cx));
                    })));
                }
                let (sk1, m1, sk2, m2) = (self.sk.clone(), m.clone(), self.sk.clone(), m.clone());
                card = card.child(
                    div()
                        .border_b_1()
                        .border_color(c(theme::LINE))
                        .p(px(10.))
                        .child(Self::heading("Timeout"))
                        .child(row)
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(btn("k-ban", "Ban", BtnOpts::danger()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    let (sk, m) = (sk1.clone(), m1.clone());
                                    let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "ban", msg: Box::new(m), seconds: None }, cx));
                                })))
                                .child(btn("k-unban", "Unban", BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    let (sk, m) = (sk2.clone(), m2.clone());
                                    let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "unban", msg: Box::new(m), seconds: None }, cx));
                                }))),
                        ),
                );
            }
        }

        // ---- ignore
        if ctx.can_ignore {
            let ignored = ctx.ignored;
            let (platform, ig_login) = (self.platform, super::ignores::ignore_login(&self.user.login, &self.user.name));
            card = card.child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .border_b_1()
                    .border_color(c(theme::LINE))
                    .p(px(10.))
                    .child(btn("ignore", if ignored { "Unignore user" } else { "Ignore user" }, BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let l = ig_login.clone();
                        let _ = this.pane.update(cx, |p, cx| p.set_ignored(platform, &l, !ignored, cx));
                        cx.notify();
                    })))
                    .when(ignored, |d| d.child(div().text_size(px(11.)).text_color(c(theme::FAINT)).child("Messages hidden"))),
            );
        }

        // ---- recent messages
        let mut recent = div()
            .id("card-recent")
            .when(!self.user_sized, |d| d.max_h(px(208.)))
            .when(self.user_sized, |d| d.flex_1().min_h(px(0.)))
            .overflow_y_scroll()
            .p(px(10.))
            .child(Self::heading(&format!("Recent messages in {}", icons::platform_label(self.platform))));
        if ctx.recent.is_empty() {
            recent = recent.child(div().text_color(c(theme::FAINT)).child("No recent messages."));
        } else {
            let mut list = div().flex().flex_col().gap(px(4.)).text_color(c(theme::MUTED));
            for (i, r) in ctx.recent.iter().enumerate() {
                let deleted = match &r.body {
                    RowBody::Msg(m) => m.deleted,
                    RowBody::Rumble(m) => m.deleted,
                    RowBody::Kick(m) => m.deleted,
                    _ => false,
                };
                let can_del = matches!(&r.body, RowBody::Msg(_)) && ctx.caps.delete && ctx.reach.delete && !deleted;
                let mut line = div()
                    .id(("recent", i))
                    .group("recent-row")
                    .relative()
                    .flex()
                    .items_start()
                    .gap(px(6.))
                    .when(deleted, |d| d.opacity(0.5))
                    .child(div().flex_none().text_color(c(theme::FAINT)).child(SharedString::from(fmt_time(r.ts, false))))
                    .child(div().min_w_0().flex_1().child(self.recent_text(r, &ctx)));
                if can_del {
                    if let RowBody::Msg(m) = &r.body {
                        let (sk, mid) = (self.sk.clone(), m.id.clone());
                        line = line.child(
                            div()
                                .id(("recent-del", i))
                                .flex_none()
                                .rounded(px(2.))
                                .p(px(2.))
                                .text_color(c(theme::FAINT))
                                .opacity(0.)
                                .group_hover("recent-row", |s| s.opacity(1.))
                                .hover(|s| s.text_color(c(theme::RED_400)))
                                .child(icon(icons::TRASH, 14.))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    let (sk, mid) = (sk.clone(), mid.clone());
                                    let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::Delete { sk, id: mid }, cx));
                                    cx.notify();
                                })),
                        );
                    }
                }
                list = list.child(line);
            }
            recent = recent.child(list);
        }
        card = card.child(recent);

        let _ = (MouseButton::Left, entity);
        div()
            .id("popout-root")
            .size_full()
            .bg(c(theme::RAISED))
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| {
                if ev.keystroke.key == "escape" {
                    this.close(w, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _w, _cx| {}))
            .child(
                div()
                    .w_full()
                    .when(self.user_sized, |d| d.h_full())
                    .on_children_prepainted({
                        let me = cx.entity();
                        move |bounds: Vec<gpui::Bounds<gpui::Pixels>>, window, cx| {
                            let Some(b) = bounds.first() else { return };
                            let h = f32::from(b.size.height);
                            let me = me.clone();
                            window.defer(cx, move |w, cx| {
                                // The window's height follows the card, until the user drags the window's edge.
                                let cur = f32::from(w.bounds().size.height);
                                let (user_sized, fitted, mismatches) = me.read(cx).fit_state();
                                if user_sized {
                                    return;
                                }
                                // our own resize may take a frame or two to show; a window that stays at another
                                // height than the one we set, with the card unchanged, was resized by hand
                                if fitted > 0.0 && (cur - fitted).abs() >= 3.0 {
                                    if mismatches >= 4 {
                                        me.update(cx, |this, cx| {
                                            this.user_sized = true;
                                            cx.notify();
                                        });
                                        return;
                                    }
                                    me.update(cx, |this, _| this.mismatches += 1);
                                } else {
                                    me.update(cx, |this, _| this.mismatches = 0);
                                }
                                let max = w.display(cx).map(|d| f32::from(d.bounds().size.height) - 40.0).unwrap_or(1000.0);
                                me.update(cx, |this, _| this.fitted_h = h.round().clamp(super::popout::POPOUT_MIN_H, max));
                                super::popout::fit_height(w, cx, h);
                            });
                        }
                    })
                    .child(card),
            )
            .into_any_element()
    }
}
