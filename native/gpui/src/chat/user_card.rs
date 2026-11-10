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
    div, point, prelude::*, px, AnyElement, ClickEvent, Context, ElementId, Entity, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, ScrollHandle, SharedString, WeakEntity, Window,
    WindowControlArea,
};
use serde_json::{json, Value};

use super::chat_pane::ChatPane;
use super::inline::{inline_flow, InlineContent};
use super::mod_actions::{self, ToastMsg};
use super::mod_caps::{relay_api, ModCaps, ModReach, TIMEOUT_LADDER};
use super::card_web::{parse_card, parse_comment, parse_messages, show_signin_prompt, sub_line, tabs_for, ts_ms, ActionItem, MsgPage, Tab, WebCard, WebStatus};
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
    /// newest first, at most `RECENT_MAX`
    pub recent: Vec<RowRef>,
    /// every line of the person the pane holds (the Messages count)
    pub recent_total: usize,
    pub readable_colors: bool,
    /// from the person's newest line in this chat
    pub badges: Vec<CardBadge>,
    pub sub: Option<SubInfo>,
    /// who comments are written as: the signed-in Twitch login
    pub me: String,
}

pub const RECENT_MAX: usize = 30;

/// One badge for the card's grid: a picture when the platform gives one, else a glyph chip.
#[derive(Clone, Debug)]
pub struct CardBadge {
    pub url: Option<String>,
    pub title: String,
    pub glyph: String,
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubInfo {
    pub tier: u8,
    pub months: Option<u32>,
}

/// Twitch's `subscriber/<version>` badge: versions from 3000 are tier 3, from 2000 tier 2, the rest tier 1.
/// The months come from `badge-info`, which names the same set ("subscriber/14"; founders: "founder/14").
pub fn sub_info(badges: &str, badge_info: &str) -> Option<SubInfo> {
    let version = |set: &str| badges.split(',').find_map(|b| b.strip_prefix(set).and_then(|r| r.strip_prefix('/')).map(|v| v.to_string()));
    let sub = version("subscriber");
    if sub.is_none() && version("founder").is_none() {
        return None;
    }
    let tier = match sub.and_then(|v| v.parse::<u32>().ok()) {
        Some(v) if v >= 3000 => 3,
        Some(v) if v >= 2000 => 2,
        _ => 1,
    };
    let months = badge_info.split(',').find_map(|b| {
        let (set, n) = b.split_once('/')?;
        if set == "subscriber" || set == "founder" { n.parse::<u32>().ok() } else { None }
    });
    Some(SubInfo { tier, months })
}

/// The badges and subscription of a person's line, whichever platform it is from.
pub fn badges_of(row: &Row, twitch: Option<&super::twitch::badges::BadgeMap>, rumble: &RumbleBadgeDefs) -> (Vec<CardBadge>, Option<SubInfo>) {
    let mut out = Vec::new();
    let mut sub = None;
    match &row.body {
        RowBody::Msg(m) => {
            if let Some(map) = twitch {
                for b in super::twitch::badges::parse_badge_list(&m.badges, map) {
                    out.push(CardBadge { url: Some(b.url), title: b.title, glyph: String::new(), color: 0 });
                }
            }
            sub = sub_info(&m.badges, &m.badge_info);
        }
        RowBody::Kick(m) => {
            for b in &m.badges {
                let title = super::rows::kick_badge_label(b);
                let (glyph, color) = super::rows::kick_badge_style(&b.r#type);
                let glyph = if glyph.is_empty() { b.text.chars().chain(b.r#type.chars()).take(3).collect() } else { glyph.to_string() };
                out.push(CardBadge { url: super::rows::kick_badge_url(b), title, glyph, color });
            }
        }
        RowBody::Rumble(m) => {
            for id in &m.badges {
                if let Some(def) = rumble.get(id) {
                    if let Some(url) = super::rows::rumble_badge_icon(def) {
                        let title = def.pointer("/label/en").and_then(|x| x.as_str()).unwrap_or(id).to_string();
                        out.push(CardBadge { url: Some(url), title, glyph: String::new(), color: 0 });
                    }
                }
            }
        }
        RowBody::YouTube(m) => {
            for b in &m.badges {
                let url = Some(b.url.clone()).filter(|u| !u.is_empty());
                out.push(CardBadge { url, title: b.label.clone(), glyph: b.label.chars().take(3).collect(), color: 0xd4d4d8 });
            }
        }
        _ => {}
    }
    (out, sub)
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// "April 30th, 2016" from an ISO timestamp, in the local time zone.
fn long_date(iso: &str) -> String {
    use chrono::Datelike;
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|d| {
            let d = d.with_timezone(&chrono::Local);
            format!("{} {}, {}", MONTHS[d.month0() as usize], ordinal(d.day()), d.year())
        })
        .unwrap_or_else(|_| "an unknown date".into())
}

/// 90 -> "1m 30s", 600 -> "10m", 86400 -> "1d": the largest unit plus the next one down.
pub fn fmt_secs(total: u64) -> String {
    const UNITS: [(&str, u64); 5] = [("w", 604_800), ("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)];
    for (i, (label, size)) in UNITS.iter().enumerate() {
        if total < *size {
            continue;
        }
        let rest = total % size;
        return match UNITS.get(i + 1) {
            Some((nl, ns)) if rest >= *ns => format!("{}{label} {}{nl}", total / size, rest / ns),
            _ => format!("{}{label}", total / size),
        };
    }
    "0s".into()
}

fn fmt_stamp(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|d| d.format("%b %-d, %-I:%M %p").to_string()).unwrap_or_default()
}

const BADGE_SIDE: f32 = 20.0;

/// One line of the Warnings / Timeouts / Bans tabs: Twitch's own sentence for what happened, and when.
fn history_row(e: &ActionItem) -> gpui::Div {
    let color = match e.kind.as_str() {
        "TIMEOUT" => theme::AMBER_400,
        "BAN" => theme::RED_300,
        "WARN" => theme::ORANGE_300,
        _ => theme::MUTED,
    };
    div()
        .flex()
        .justify_between()
        .items_start()
        .gap(px(8.))
        .child(div().min_w_0().flex_1().text_color(c(color)).child(SharedString::from(e.text.clone())))
        .child(div().flex_none().text_size(px(11.)).text_color(c(theme::FAINT)).child(SharedString::from(fmt_stamp(ts_ms(&e.ts)))))
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

/// Kick's own stand-in picture for an account without one (kick.com serves default-avatar-1..6). Which of the six
/// kick.com picks is not public; the user id keeps it stable per user here.
fn kick_default_avatar(user_id: &str) -> String {
    let n = user_id.parse::<u64>().map(|id| id % 6 + 1).unwrap_or(1);
    format!("https://kick.com/img/default-profile-pictures/default-avatar-{n}.webp")
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
    /// Kick: the Ban button was clicked once and now asks to confirm in the card.
    kick_ban_ask: bool,
    reason_in: Entity<TextInput>,
    busy: bool,
    warn_open: bool,
    rules: Vec<String>,
    selected_rules: BTreeSet<String>,
    custom_in: Entity<TextInput>,
    tab: Tab,
    tab_scroll: ScrollHandle,
    comment_in: Entity<TextInput>,
    /// Twitch's own viewer card (twitch.tv sign-in), when it answered
    web: Option<WebCard>,
    web_status: WebStatus,
    /// why Twitch's own card (badges, mod logs) did not load, shown under the account info
    web_error: Option<String>,
    /// every badge shown, not just the first row
    badges_open: bool,
    /// the logged messages (Twitch, first 50, then more on request)
    msgs: Option<MsgPage>,
    msgs_loading: bool,
    signin_busy: bool,
    comment_busy: bool,
    /// The window size we last fitted (`fit_height`), and whether the user resized the window by hand since:
    /// then the card fills the window (`PopoutFill`) instead of the window following the card, and its size
    /// is remembered for the next card (`window_state::card_size`).
    fitted_h: f32,
    fitted_w: f32,
    user_sized: bool,
    focus: gpui::FocusHandle,
    _subs: Vec<gpui::Subscription>,
}

impl UserCardView {
    /// `user_sized`: the window opens at the user's remembered card size, so the card fills it from the start.
    pub fn new(window: &mut Window, cx: &mut Context<Self>, pane: WeakEntity<ChatPane>, id: String, sk: String, platform: Platform, user: CardUser, user_sized: bool) -> Self {
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
        let comment_in = cx.new(|cx| {
            let mut t = TextInput::new(window, cx, "Add a comment", InputStyle::Chat);
            t.set_max_chars(Some(agu_chat_core::twitch::web::COMMENT_MAX_CHARS));
            t
        });
        let mut subs = vec![cx.observe(&reason_in, |_, _, cx| cx.notify()), cx.observe(&custom_in, |_, _, cx| cx.notify())];
        subs.push(cx.subscribe(&comment_in, |this, _, ev: &TextInputEvent, cx| {
            if matches!(ev, TextInputEvent::Enter) {
                this.add_comment(cx);
            }
        }));
        // A size we didn't set is the user's hand: from then on the window keeps it (no more fitting, which
        // used to snap the edge back mid-drag) and the next card opens at it.
        subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            let s = window.bounds().size;
            let (w, h) = (f32::from(s.width), f32::from(s.height));
            if !this.user_sized {
                if this.fitted_h <= 0.0 || ((h - this.fitted_h).abs() < 3.0 && (w - this.fitted_w).abs() < 3.0) {
                    return;
                }
                this.user_sized = true;
                cx.notify();
            }
            crate::window_state::set_card_size(crate::window_state::CardSize { width: w.round(), height: h.round() });
        }));
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
            kick_ban_ask: false,
            reason_in,
            busy: false,
            warn_open: false,
            rules: vec![],
            selected_rules: BTreeSet::new(),
            custom_in,
            tab: Tab::Messages,
            tab_scroll: ScrollHandle::new(),
            comment_in,
            web: None,
            web_status: WebStatus::Unknown,
            web_error: None,
            badges_open: false,
            msgs: None,
            msgs_loading: false,
            signin_busy: false,
            comment_busy: false,
            fitted_h: 0.0,
            fitted_w: 0.0,
            user_sized,
            focus,
            _subs: subs,
        };
        v.load(cx);
        v
    }

    fn add_comment(&mut self, cx: &mut Context<Self>) {
        let text = self.comment_in.read(cx).text().trim().to_string();
        let Some(ctx) = self.pane_ctx(cx) else { return };
        let target = self.web.as_ref().map(|w| w.user_id.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| self.user.id.clone());
        if text.is_empty() || self.comment_busy || target.is_empty() {
            return;
        }
        self.comment_busy = true;
        cx.notify();
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, "/api/twitch/card/comment", Some("POST"), Some(json!({"targetId": target, "text": text}))).await;
            let _ = this.update(cx, |v, cx| {
                v.comment_busy = false;
                if r.ok {
                    if let Some(l) = v.web.as_mut().and_then(|w| w.logs.as_mut()) {
                        l.comments.push(parse_comment(&r.body["comment"]));
                    }
                    v.comment_in.update(cx, |i, cx| i.set_text("", cx));
                } else {
                    v.toast(Some((format!("Couldn't add the comment: {}", r.text()), true)), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn delete_comment(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(ctx) = self.pane_ctx(cx) else { return };
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, "/api/twitch/card/comment", Some("DELETE"), Some(json!({"commentId": id.clone()}))).await;
            let _ = this.update(cx, |v, cx| {
                if r.ok {
                    if let Some(l) = v.web.as_mut().and_then(|w| w.logs.as_mut()) {
                        l.comments.retain(|c| c.id != id);
                    }
                } else {
                    v.toast(Some((format!("Couldn't delete the comment: {}", r.text()), true)), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The first (or next) 50 logged lines of the person, from Twitch.
    fn load_messages(&mut self, cx: &mut Context<Self>) {
        let Some(ctx) = self.pane_ctx(cx) else { return };
        let Some(w) = &self.web else { return };
        if self.msgs_loading || w.user_id.is_empty() {
            return;
        }
        self.msgs_loading = true;
        let cursor = self.msgs.as_ref().and_then(|m| m.cursor.clone()).unwrap_or_default();
        let mut path = format!("/api/twitch/card/messages?id={}", w.user_id.chars().filter(|c| c.is_ascii_digit()).collect::<String>());
        if !cursor.is_empty() {
            path.push_str(&format!("&cursor={}", cursor.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '.')).collect::<String>()));
        }
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, &path, None, None).await;
            let _ = this.update(cx, |v, cx| {
                v.msgs_loading = false;
                if r.ok {
                    let page = parse_messages(&r.body);
                    match v.msgs.as_mut() {
                        Some(m) => {
                            m.items.extend(page.items);
                            m.more = page.more;
                            m.cursor = page.cursor;
                        }
                        None => v.msgs = Some(page),
                    }
                } else if v.msgs.is_none() {
                    v.msgs = Some(MsgPage::default());
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// "Log in to Twitch": the same login as in Settings (one window: twitch.tv, then authorising the app); the card
    /// follows it until it ends.
    fn start_signin(&mut self, cx: &mut Context<Self>) {
        if self.signin_busy {
            return;
        }
        let Some(ctx) = self.pane_ctx(cx) else { return };
        self.signin_busy = true;
        cx.notify();
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let started = bridge.accounts_login(Platform::Twitch, json!({})).await;
            let mut signed_in = false;
            let mut error = started.as_ref().err().cloned();
            if started.is_ok() {
                for _ in 0..700 {
                    cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                    let st = relay_api(&bridge, &ctx.source, "/api/twitch/web", None, None).await;
                    if !st.ok || st.body["pending"].as_bool().unwrap_or(false) {
                        continue;
                    }
                    signed_in = st.body["signedIn"].as_bool().unwrap_or(false);
                    break;
                }
                if !signed_in {
                    error = bridge.accounts_get().await.get(&Platform::Twitch).and_then(|a| a.error.clone());
                }
            }
            let _ = this.update(cx, |v, cx| {
                v.signin_busy = false;
                if signed_in {
                    v.load(cx);
                } else if let Some(e) = error.filter(|e| !e.is_empty()) {
                    v.toast(Some((e, true)), cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fit_state(&self) -> bool {
        self.user_sized
    }

    fn pane_ctx(&self, cx: &gpui::App) -> Option<CardCtx> {
        self.pane.upgrade().and_then(|p| p.read(cx).card_ctx(&self.sk, &self.user, self.platform))
    }

    fn toast(&self, t: ToastMsg, cx: &mut Context<Self>) {
        let _ = self.pane.update(cx, |p, cx| p.show_toast(t, cx));
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.platform == Platform::Kick && !self.user.name.is_empty() {
            self.load_kick(cx);
            return;
        }
        if self.platform != Platform::Twitch || self.user.id.is_empty() {
            self.loading = false;
            return;
        }
        let Some(ctx) = self.pane_ctx(cx) else { return };
        self.loading = true;
        self.relogin = false;
        let bridge = cx.global::<Services>().bridge.clone();
        let id = self.user.id.clone();
        let whisper = crate::layout::tree::is_whisper(&ctx.source);
        let login: String = if self.user.login.is_empty() { self.user.name.to_lowercase() } else { self.user.login.clone() }.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        cx.spawn(async move |this, cx| {
            // Twitch's own viewer card first (a twitch.tv sign-in); a whisper has no channel to ask about
            let mut web: Option<WebCard> = None;
            let mut status = WebStatus::Unknown;
            let mut web_error: Option<String> = None;
            if !whisper && !login.is_empty() {
                let r = relay_api(&bridge, &ctx.source, &format!("/api/twitch/card?login={login}&id={}", id.chars().filter(|c| c.is_ascii_digit()).collect::<String>()), None, None).await;
                status = WebStatus::from_answer(r.status, &r.body);
                if r.ok {
                    web = Some(parse_card(&r.body));
                } else if !matches!(status, WebStatus::SignedOut | WebStatus::Expired) && r.status != 404 {
                    web_error = Some(format!("Twitch's mod view didn't load: {}", r.text()));
                }
            }
            // Helix: the account itself, and (for a broadcaster) the ban, when Twitch's card has no mod logs
            // (also when Twitch's card left out the follow date: that part of it is behind Twitch's integrity check)
            let helix = if web.as_ref().map(|w| w.logs.is_some() && w.followed_at.is_some()).unwrap_or(false) { None } else { Some(relay_api(&bridge, &ctx.source, &format!("/api/mod/user?id={}", id.replace('%', "%25").replace('&', "%26")), None, None).await) };
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                v.web_status = status;
                v.web = web;
                v.web_error = web_error;
                if let Some(w) = &v.web {
                    let mut d = ModUser {
                        login: w.login.clone(),
                        display_name: w.display.clone(),
                        avatar: w.avatar.clone(),
                        created_at: w.created_at.clone(),
                        followed_at: w.followed_at.clone(),
                        ban: w.ban.as_ref().map(|b| (b.permanent, b.expires_at.clone(), b.reason.clone())),
                        ..Default::default()
                    };
                    if let Some(r) = helix.as_ref().filter(|r| r.ok) {
                        let h = parse_mod_user(&r.body);
                        d.ban = d.ban.or(h.ban);
                        d.followed_at = d.followed_at.or(h.followed_at);
                        d.follow_error = h.follow_error;
                        if d.created_at.is_empty() {
                            d.created_at = h.created_at;
                        }
                        d.ban_error = h.ban_error;
                    }
                    v.data = Some(d);
                    if v.web.as_ref().map(|w| w.logs.is_some()).unwrap_or(false) && v.msgs.is_none() {
                        v.load_messages(cx);
                    }
                } else if let Some(r) = helix {
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
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// After a Kick mod action: read the user's state again once Kick has applied it, so Ban / Unban / Untimeout
    /// follows.
    fn reload_soon(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            for wait in [1500u64, 3500] {
                cx.background_executor().timer(std::time::Duration::from_millis(wait)).await;
                if this.update(cx, |v, cx| v.load_kick(cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Kick: avatar, account age, follow date and ban from kick.com (core `GET /api/kick/user`). A failure just
    /// leaves the card without them.
    fn load_kick(&mut self, cx: &mut Context<Self>) {
        let Some(ctx) = self.pane_ctx(cx) else { return };
        // a reload after a mod action keeps the card as it is until the new state arrives
        self.loading = self.data.is_none();
        let bridge = cx.global::<Services>().bridge.clone();
        let name: String = self.user.name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &ctx.source, &format!("/api/kick/user?name={name}"), None, None).await;
            let _ = this.update(cx, |v, cx| {
                v.loading = false;
                if r.ok {
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
            // a square with softly rounded corners on every platform; a non-square picture (common on Kick) is
            // cropped to its middle instead of shrunk into the box
            let url = if src.ends_with(super::images::SQUARE) { src.to_string() } else { format!("{src}{}", super::images::SQUARE) };
            return ImageBox { url: url.into(), side: 36.0, radius: 6.0 }.into_any_element();
        }
        div()
            .size(px(36.))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
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
        let mut avatar_src = self.data.as_ref().map(|d| d.avatar.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| self.user.avatar.clone());
        if avatar_src.is_empty() && self.platform == Platform::Kick && !self.loading {
            avatar_src = kick_default_avatar(&self.user.id);
        }
        let profile = if !login.is_empty() { profile_url(self.platform, &login) } else { None };
        let whisper = crate::layout::tree::is_whisper(&ctx.source);
        // Twitch's ban status (twitch.tv mod logs), else Helix's (broadcaster only)
        let banned = self.data.as_ref().and_then(|d| d.ban.clone());
        let has_logs = self.web.as_ref().map(|w| w.logs.is_some()).unwrap_or(false);
        let caps = &ctx.caps;
        let reach = &ctx.reach;
        let show_twitch_mod = is_twitch && !whisper && (reach.ban || reach.timeout || reach.warn || banned.is_some()) && (caps.ban || caps.warn);
        let busy = self.busy;
        let uid = self.user.id.clone();
        let entity = cx.entity();

        // "Ignore user" sits at the right edge of the first row of action buttons (the timeout ladder); when no
        // such row is drawn it gets a compact row of its own at the end of the action area.
        let mut ignore_slot: Option<AnyElement> = None;
        if ctx.can_ignore {
            let ignored = ctx.ignored;
            let (platform, ig_login) = (self.platform, super::ignores::ignore_login(&self.user.login, &self.user.name));
            ignore_slot = Some(
                div()
                    .ml_auto()
                    .flex_none()
                    .child(btn("ignore", if ignored { "Unignore user" } else { "Ignore user" }, BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let l = ig_login.clone();
                        let _ = this.pane.update(cx, |p, cx| p.set_ignored(platform, &l, !ignored, cx));
                        cx.notify();
                    })))
                    .into_any_element(),
            );
        }

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
                    .occlude()
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
            // The whole header row is the window's grab handle (`app-drag`), except the name link and the close
            // button (`app-no-drag`, their own hitboxes block it). occlude: see notebook_tabs' window buttons (a
            // focusable ancestor would otherwise swallow the press).
            .window_control_area(WindowControlArea::Drag)
            .occlude()
            .child(div().flex_none().child(Self::avatar(&avatar_src, &display)))
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(div().flex().items_center().gap(px(6.)).child(icons::platform_icon(self.platform, 14., true)).child(name_el))
                    .when(!display.is_empty() && !login.is_empty() && display.to_lowercase() != login.to_lowercase(), |d| d.child(div().text_size(px(11.)).line_height(px(16.)).text_color(c(theme::FAINT)).child(SharedString::from(format!("@{login}"))))),
            )
            .child(
                div()
                    .id("card-close")
                    .flex_none()
                    .rounded(px(4.))
                    .p(px(4.))
                    .text_color(c(theme::FAINT))
                    .hover(|s| s.text_color(c(theme::TEXT)))
                    .occlude()
                    .child(icon(icons::X, 16.))
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.close(w, cx))),
            )
            // the top 4px are the window's top resize edge, not the grab handle: the strip's hitbox blocks the
            // drag area, so the platform falls through to its own top-edge resize
            .relative()
            .child(div().id("card-top-edge").absolute().top_0().left_0().right_0().h(px(4.)).occlude());

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

        // ---- badges, from the person's newest line here
        let badge_list: &[CardBadge] = match &self.web {
            Some(w) if !w.badges.is_empty() => &w.badges,
            _ => &ctx.badges,
        };
        if !badge_list.is_empty() {
            // one row (20px badges, 6px apart, 10px padding) unless opened; the toggle takes the row's last spot
            let per_row = (((window.viewport_size().width.to_f64() as f32 - 22.0 + 6.0) / (BADGE_SIDE + 6.0)).floor() as usize).max(2);
            let overflow = badge_list.len() > per_row;
            let shown = if overflow && !self.badges_open { per_row - 1 } else { badge_list.len() };
            let mut grid = div().flex().flex_wrap().gap(px(6.)).border_b_1().border_color(c(theme::LINE)).p(px(10.));
            for (i, b) in badge_list.iter().take(shown).enumerate() {
                let face: AnyElement = match &b.url {
                    Some(u) => ImageBox { url: u.clone().into(), side: BADGE_SIDE, radius: 3.0 }.into_any_element(),
                    None => div()
                        .h(px(BADGE_SIDE))
                        .min_w(px(BADGE_SIDE))
                        .px(px(3.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(a(b.color, 0.2))
                        .text_size(px(10.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(token(b.color))
                        .child(SharedString::from(b.glyph.clone()))
                        .into_any_element(),
                };
                grid = grid.child(div().id(("badge", i)).flex_none().child(face).tooltip(crate::layout::ui::tip(b.title.clone())));
            }
            if overflow {
                let open = self.badges_open;
                let label = if open { String::new() } else { format!("+{}", badge_list.len() - shown) };
                grid = grid.child(
                    div()
                        .id("badges-toggle")
                        .flex_none()
                        .h(px(BADGE_SIDE))
                        .min_w(px(BADGE_SIDE))
                        .px(px(4.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(c(theme::LINE))
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(theme::MUTED))
                        .cursor_pointer()
                        .hover(|d| d.text_color(c(theme::TEXT)))
                        .when(open, |d| d.child(icon(icons::CHEVRON_UP, 12.)))
                        .when(!open, |d| d.child(SharedString::from(label)))
                        .tooltip(crate::layout::ui::tip(if open { "Show fewer badges".to_string() } else { "Show all badges".to_string() }))
                        .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.badges_open = !this.badges_open;
                            cx.notify();
                        })),
                );
            }
            card = card.child(grid);
        }

        // ---- account info (Twitch: /api/mod/user, Kick: kick.com's channel-user lookup) and what the chat shows
        let info_line = |ic: &'static str, text: String| div().flex().items_center().gap(px(6.)).child(div().flex_none().text_color(c(theme::FAINT)).child(icon(ic, 13.))).child(div().min_w_0().child(SharedString::from(text)));
        let chip = |label: String, fg: u32, border: u32| div().flex().child(div().rounded_full().border_1().border_color(a(border, 0.3)).bg(a(border, 0.1)).px(px(8.)).text_size(px(11.)).line_height(px(16.)).font_weight(FontWeight::SEMIBOLD).text_color(c(fg)).child(SharedString::from(label)));
        let has_account = is_twitch || self.platform == Platform::Kick;
        let mut info = div().flex().flex_col().gap(px(4.)).border_b_1().border_color(c(theme::LINE)).p(px(10.)).text_color(c(theme::MUTED));
        let mut info_rows = 0;
        if has_account && self.relogin {
            info = info.child(div().text_color(c(theme::AMBER_300)).child("Log in again to enable mod tools."));
            info_rows += 1;
        } else if has_account && self.loading {
            info = info.child(div().h(px(12.)).w(px(160.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1))).child(div().h(px(12.)).w(px(128.)).rounded(px(4.)).bg(a(theme::WHITE, 0.1)));
            info_rows += 1;
        } else if has_account {
            if let Some(d) = &self.data {
                if !d.created_at.is_empty() {
                    info = info.child(info_line(icons::CLOCK, format!("Account created on {}", long_date(&d.created_at))));
                    info_rows += 1;
                }
                // "Not following" only when we actually confirmed it: a failed lookup omits the line.
                if d.followed_at.is_some() || !d.follow_error {
                    info = info.child(info_line(icons::STAR, match &d.followed_at {
                        Some(f) => format!("Following since {}", long_date(f)),
                        None => "Not following".to_string(),
                    }));
                    info_rows += 1;
                }
            }
        }
        match (&self.web, &ctx.sub) {
            (Some(w), _) if w.sub.is_some() => {
                let sb = w.sub.as_ref().unwrap();
                info = info.child(info_line(icons::TAG, sub_line(sb.tier, sb.months, sb.gift, sb.prime)));
                info_rows += 1;
            }
            (None, Some(sb)) => {
                info = info.child(info_line(icons::TAG, sub_line(sb.tier, sb.months, false, false)));
                info_rows += 1;
            }
            _ => {}
        }
        if let Some((permanent, expires, reason)) = &banned {
            info = info.child(if *permanent {
                chip(format!("Banned{}", if reason.is_empty() { String::new() } else { format!(": {reason}") }), theme::RED_300, theme::RED_500)
            } else {
                chip(format!("Timed out until {}", expires.as_deref().map(local_time).unwrap_or_else(|| "?".into())), theme::AMBER_400, theme::AMBER_500)
            });
            info_rows += 1;
        } else if let Some(e) = self.data.as_ref().and_then(|d| d.ban_error.as_ref()) {
            info = info.child(div().text_color(c(theme::FAINT)).child(SharedString::from(format!("Ban status unknown: {e}"))));
            info_rows += 1;
        }
        if let Some(e) = &self.web_error {
            info = info.child(div().text_color(c(theme::FAINT)).child(SharedString::from(e.clone())));
            info_rows += 1;
        }
        if let Some(w) = &self.web {
            if !w.same_user {
                info = info.child(div().text_color(c(theme::AMBER_300)).child(SharedString::from(format!("twitch.tv is signed in as {}, not the Twitch account Agu Chat is logged in as.", if w.me_login.is_empty() { "another account".to_string() } else { w.me_login.clone() }))));
                info_rows += 1;
            }
            if let Some(e) = &w.logs_error {
                info = info.child(div().text_color(c(theme::FAINT)).child(SharedString::from(e.clone())));
                info_rows += 1;
            }
        }
        if has_account && !self.loading {
            if let Some(s) = &ctx.suspicious {
                let det = suspicious_details(s);
                info = info.child(
                    div().rounded(px(4.)).border_1().border_color(a(theme::ORANGE_500, 0.3)).bg(a(theme::ORANGE_500, 0.1)).px(px(8.)).py(px(4.)).text_color(c(theme::ORANGE_300)).child(SharedString::from(format!(
                        "Twitch flags this user: {}{}",
                        if s.status == "restricted" { "restricted" } else { "monitored" },
                        if det.is_empty() { String::new() } else { format!(" - {det}") }
                    ))),
                );
                info_rows += 1;
            }
        }
        if has_account && !self.loading && !self.relogin && self.data.is_none() && info_rows == 0 {
            info = info.child(div().text_color(c(theme::FAINT)).child("No account info."));
            info_rows += 1;
        }
        if info_rows > 0 {
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
                row = row.children(ignore_slot.take());
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
                        })))
                        .children(ignore_slot.take()),
                );
            }
        }
        if self.platform == Platform::Kick && ctx.co_can_moderate && !self.user.id.is_empty() {
            if let Some(Row { body: RowBody::Kick(m), .. }) = last.as_deref() {
                // ban state from the card lookup: None free (or unknown), Some(true) banned, Some(false) timed out
                let state = self.data.as_ref().and_then(|d| d.ban.as_ref().map(|b| b.0));
                let mut row = div().mb(px(8.)).flex().flex_wrap().gap(px(4.));
                for (label, sec) in [("1m", 60u32), ("5m", 300), ("10m", 600), ("1h", 3600), ("24h", 86400), ("7d", 604800)] {
                    let (sk, m) = (self.sk.clone(), m.clone());
                    row = row.child(Self::ladder_btn(format!("k-{label}"), label, false).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let (sk, m) = (sk.clone(), m.clone());
                        let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "timeout", msg: Box::new(m), seconds: Some(sec) }, cx));
                        this.reload_soon(cx);
                    })));
                }
                let (sk1, m1, sk2, m2) = (self.sk.clone(), m.clone(), self.sk.clone(), m.clone());
                // One button for the user's state: Ban when free, Unban when banned, Untimeout when timed out (Kick
                // lifts both with the same call). Ban asks once more here in the card.
                let buttons = div().flex().gap(px(6.));
                let buttons = match state {
                    Some(permanent) => buttons.child(btn("k-unban", if permanent { "Unban" } else { "Untimeout" }, BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let (sk, m) = (sk2.clone(), m2.clone());
                        let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "unban", msg: Box::new(m), seconds: None }, cx));
                        this.reload_soon(cx);
                    }))),
                    None if self.kick_ban_ask => buttons
                        .child(btn("k-ban-ok", "Confirm ban", BtnOpts::danger()).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            let (sk, m) = (sk1.clone(), m1.clone());
                            this.kick_ban_ask = false;
                            let _ = this.pane.update(cx, |p, cx| p.run_action(RowAction::KickMod { sk, action: "ban!", msg: Box::new(m), seconds: None }, cx));
                            this.reload_soon(cx);
                        })))
                        .child(btn("k-ban-cancel", "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                            this.kick_ban_ask = false;
                            cx.notify();
                        }))),
                    None => buttons.child(btn("k-ban", "Ban", BtnOpts::danger()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.kick_ban_ask = true;
                        cx.notify();
                    }))),
                };
                let mut section = div().border_b_1().border_color(c(theme::LINE)).p(px(10.));
                if state != Some(true) {
                    row = row.children(ignore_slot.take());
                    section = section.child(Self::heading("Timeout")).child(row);
                }
                card = card.child(section.child(buttons));
            }
        }

        // ---- ignore, when no action row took it
        if let Some(btn_el) = ignore_slot.take() {
            card = card.child(div().flex().items_center().border_b_1().border_color(c(theme::LINE)).p(px(10.)).child(btn_el));
        }

        // ---- tabs: Twitch's own (twitch.tv mod logs) or only the person's lines in this chat
        let tabs = tabs_for(self.platform, whisper, has_logs);
        if !tabs.contains(&self.tab) {
            self.tab = Tab::Messages;
        }
        let logs = self.web.as_ref().and_then(|w| w.logs.clone());
        let mut strip = div().flex().flex_wrap().border_b_1().border_color(c(theme::LINE)).px(px(4.));
        for (i, tab) in tabs.iter().copied().enumerate() {
            let on = self.tab == tab;
            let n = match &logs {
                Some(l) => l.label(tab),
                None => ctx.recent_total.to_string(),
            };
            strip = strip.child(
                div()
                    .id(("card-tab", i))
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(6.))
                    .border_b_2()
                    .border_color(if on { c(theme::ACCENT) } else { a(theme::WHITE, 0.0) })
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .font_weight(if on { FontWeight::SEMIBOLD } else { FontWeight::NORMAL })
                    .text_color(if on { c(theme::TEXT) } else { c(theme::MUTED) })
                    .when(!on, |d| d.hover(|s| s.text_color(c(theme::TEXT))))
                    .cursor_pointer()
                    .child(tab.label())
                    .child(div().text_color(c(theme::FAINT)).child(SharedString::from(n)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.tab = tab;
                        this.tab_scroll.set_offset(point(px(0.), px(0.)));
                        cx.notify();
                    })),
            );
        }
        card = card.child(strip);

        // a moderator without a twitch.tv session (an older login, or none): the other tabs are one login away
        if show_signin_prompt(self.platform, whisper, ctx.caps.is_mod == Some(true) || ctx.caps.is_broadcaster == Some(true), self.web_status) {
            let busy = self.signin_busy;
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(c(theme::LINE))
                    .px(px(10.))
                    .py(px(6.))
                    .child(div().min_w_0().flex_1().text_size(px(11.)).line_height(px(15.)).text_color(c(theme::FAINT)).child("Log in to Twitch again to see warnings, timeouts, bans and mod comments"))
                    .child(btn("web-signin", if busy { "Waiting..." } else { "Log in to Twitch" }, BtnOpts { disabled: busy, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.start_signin(cx)))),
            );
        }

        if self.tab == Tab::Comments {
            let can_add = !self.comment_busy && !self.comment_in.read(cx).text().trim().is_empty();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .pt(px(10.))
                    .child(div().min_w_0().flex_1().child(self.comment_in.clone()))
                    .child(btn("comment-add", "Add", BtnOpts { primary: true, disabled: !can_add, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.add_comment(cx)))),
            );
        }

        let mut body = div()
            .id("card-recent")
            .when(!self.user_sized, |d| d.max_h(px(208.)))
            .when(self.user_sized, |d| d.flex_1().min_h(px(0.)))
            .overflow_y_scroll()
            .track_scroll(&self.tab_scroll)
            .p(px(10.));
        let empty = |t: &str| div().text_color(c(theme::FAINT)).child(SharedString::from(t.to_string()));
        match self.tab {
            Tab::Messages if logs.is_some() => {
                // Twitch's log of the person's lines in this channel (first 50, more on request)
                match &self.msgs {
                    None => body = body.child(empty("Loading...")),
                    Some(m) if m.items.is_empty() => body = body.child(empty("No messages.")),
                    Some(m) => {
                        let mut list = div().flex().flex_col().gap(px(4.)).text_color(c(theme::MUTED));
                        for (i, it) in m.items.iter().enumerate() {
                            let stamp = fmt_stamp(ts_ms(&it.ts));
                            let (txt_color, text) = match it.kind.as_str() {
                                "action" => (theme::FAINT, it.text.clone()),
                                "caught" => (theme::ORANGE_300, format!("{} (held by AutoMod{})", it.text, if it.status.is_empty() { String::new() } else { format!(", {}", it.status.to_lowercase()) })),
                                _ => (theme::MUTED, it.text.clone()),
                            };
                            list = list.child(
                                div()
                                    .id(("tw-msg", i))
                                    .flex()
                                    .items_start()
                                    .gap(px(6.))
                                    .child(div().flex_none().text_size(px(11.)).text_color(c(theme::FAINT)).child(SharedString::from(stamp)))
                                    .child(div().min_w_0().flex_1().text_color(c(txt_color)).child(SharedString::from(text))),
                            );
                        }
                        if m.more {
                            let busy = self.msgs_loading;
                            list = list.child(div().flex().child(btn("msgs-more", if busy { "Loading..." } else { "Load more" }, BtnOpts { disabled: busy, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.load_messages(cx)))));
                        }
                        body = body.child(list);
                    }
                }
            }
            Tab::Messages => {
                if ctx.recent.is_empty() {
                    body = body.child(empty("No recent messages."));
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
                    body = body.child(list);
                }
            }
            Tab::Warnings | Tab::Timeouts | Tab::Bans => {
                let none = match self.tab {
                    Tab::Warnings => "No warnings.",
                    Tab::Timeouts => "No timeouts.",
                    _ => "No bans.",
                };
                let rows: Vec<ActionItem> = logs.as_ref().map(|l| l.history(self.tab).into_iter().cloned().collect()).unwrap_or_default();
                if rows.is_empty() {
                    body = body.child(empty(none));
                } else {
                    body = body.child(div().flex().flex_col().gap(px(8.)).children(rows.iter().map(history_row)));
                }
            }
            Tab::Comments => {
                let mut comments = logs.as_ref().map(|l| l.comments.clone()).unwrap_or_default();
                comments.sort_by(|x, y| ts_ms(&y.ts).cmp(&ts_ms(&x.ts)));
                if comments.is_empty() {
                    body = body.child(empty("No comments yet."));
                } else {
                    let me = self.web.as_ref().map(|w| w.me_login.clone()).unwrap_or_default();
                    let mut list = div().flex().flex_col().gap(px(8.));
                    for (i, cm) in comments.iter().enumerate() {
                        let mine = !me.is_empty() && cm.by.eq_ignore_ascii_case(&me);
                        let id = cm.id.clone();
                        list = list.child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(6.))
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .child(div().text_color(c(theme::TEXT)).child(SharedString::from(cm.text.clone())))
                                        .child(div().text_size(px(11.)).text_color(c(theme::FAINT)).child(SharedString::from(format!("{} - {}", cm.by, fmt_stamp(ts_ms(&cm.ts)))))),
                                )
                                .when(mine, |d| {
                                    d.child(
                                        div()
                                            .id(("comment-del", i))
                                            .flex_none()
                                            .rounded(px(2.))
                                            .p(px(2.))
                                            .text_color(c(theme::FAINT))
                                            .hover(|s| s.text_color(c(theme::RED_400)))
                                            .cursor_pointer()
                                            .child(icon(icons::TRASH, 14.))
                                            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| this.delete_comment(id.clone(), cx))),
                                    )
                                }),
                        );
                    }
                    body = body.child(list);
                }
                body = body.child(div().mt(px(10.)).text_size(px(11.)).text_color(c(theme::FAINT)).child("Shared with this channel's moderators on Twitch."));
            }
        }
        card = card.child(body);

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
                                // The window's height follows the card, until the user drags the window's edge
                                // (the bounds observer in `new` notices that).
                                if me.read(cx).fit_state() {
                                    return;
                                }
                                let max = w.display(cx).map(|d| f32::from(d.bounds().size.height) - 40.0).unwrap_or(1000.0);
                                let cur_w = f32::from(w.bounds().size.width);
                                // recorded before the resize, so the bounds change it causes reads as ours
                                me.update(cx, |this, _| {
                                    this.fitted_h = h.round().clamp(super::popout::POPOUT_MIN_H, max);
                                    this.fitted_w = cur_w;
                                });
                                super::popout::fit_height(w, cx, h);
                            });
                        }
                    })
                    .child(card),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sub_tier_and_months_come_from_the_badges() {
        assert_eq!(sub_info("subscriber/3012,premium/1", "subscriber/14"), Some(SubInfo { tier: 3, months: Some(14) }));
        assert_eq!(sub_info("subscriber/0", "subscriber/70"), Some(SubInfo { tier: 1, months: Some(70) }));
        assert_eq!(sub_info("subscriber/2000", ""), Some(SubInfo { tier: 2, months: None }));
        assert_eq!(sub_info("founder/0", "founder/9"), Some(SubInfo { tier: 1, months: Some(9) }));
        assert_eq!(sub_info("moderator/1,subscribers-club/1", ""), None);
    }

    #[test]
    fn dates_and_durations_read_naturally() {
        assert_eq!([ordinal(1), ordinal(2), ordinal(3), ordinal(4), ordinal(11), ordinal(12), ordinal(21), ordinal(30)], ["1st", "2nd", "3rd", "4th", "11th", "12th", "21st", "30th"]);
        assert_eq!(fmt_secs(600), "10m");
        assert_eq!(fmt_secs(90), "1m 30s");
        assert_eq!(fmt_secs(604_800), "1w");
        assert_eq!(fmt_secs(0), "0s");
    }
}
