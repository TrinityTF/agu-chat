//! The hype train bar at the top of a Twitch chat, like twitch.tv's: collapsed it is a progress bar ("Hype Train",
//! "Lvl N", time left, percent of the level), expanded it adds what is left to finish the level in Bits or subs,
//! the level's emotes, a few lines of info and the conductors (top sub gifter / top cheerer).
//!
//! Data: `GET /api/twitch/hype-train` of the core (public gql.twitch.tv fields, see core `twitch/hype_train.rs`).
//! Each pane asks for its Twitch channels on a timer: every `ACTIVE_POLL` while a train is on, every `IDLE_POLL`
//! otherwise, and a few seconds after a sub or Bits show up in chat (what starts a train). The bar goes away when
//! the core says the train is over, or on its own once the time runs out. Setting `hypeTrain` turns it all off.
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::Platform;
use gpui::{deferred, div, prelude::*, px, relative, AnyElement, ClickEvent, Context, ElementId, FontWeight, SharedString};
use serde_json::Value;

use super::chat_pane::ChatPane;
use super::inline::Tip;
use super::mod_caps::relay_api;
use super::pane_render::InlineContentBuilder;
use super::ui_util::token;
use crate::icons::{self, icon};
use crate::theme::{self, a, c};

pub const ACTIVE_POLL: Duration = Duration::from_secs(5);
pub const IDLE_POLL: Duration = Duration::from_secs(45);
pub const ERROR_POLL: Duration = Duration::from_secs(30);
/// after a sub or Bits in chat: check this soon
pub const SOON: Duration = Duration::from_secs(3);
/// Twitch's own hype train purple, when the channel has no color of its own
pub const DEFAULT_COLOR: u32 = 0x9147ff;
/// the open details scroll past this height
pub const DETAILS_MAX_H: f32 = 380.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HypeEmote {
    pub id: String,
    pub token: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Conductor {
    /// "SUBS" | "BITS"
    pub source: String,
    pub name: String,
    pub avatar: String,
    pub quantity: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HypeTrain {
    pub id: String,
    pub level: u64,
    pub percent: u64,
    pub bits: u64,
    pub subs: u64,
    /// unix ms; None when Twitch sent no end time
    pub expires_ms: Option<i64>,
    pub emotes: Vec<HypeEmote>,
    pub conductors: Vec<Conductor>,
    /// sources whose top contributor gets the conductor badge ("BITS", "SUBS")
    pub conductor_rewards: Vec<String>,
    pub treasure: bool,
    pub golden_kappa: bool,
    pub shared: bool,
    pub color: Option<u32>,
    pub record_level: u64,
}

fn u(v: &Value, k: &str) -> u64 {
    v[k].as_u64().unwrap_or(0)
}

fn st(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or("").to_string()
}

/// The core's answer -> the train, None when there is none going.
pub fn parse(v: &Value) -> Option<HypeTrain> {
    if v["active"].as_bool() != Some(true) {
        return None;
    }
    let list = |k: &str| v[k].as_array().cloned().unwrap_or_default();
    Some(HypeTrain {
        id: st(v, "id"),
        level: u(v, "level").max(1),
        percent: u(v, "percent").min(100),
        bits: u(v, "bits"),
        subs: u(v, "subs"),
        expires_ms: v["expiresAt"].as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp_millis()),
        emotes: list("emotes").iter().filter(|e| !st(e, "url").is_empty()).map(|e| HypeEmote { id: st(e, "id"), token: st(e, "token"), url: st(e, "url") }).collect(),
        conductors: list("conductors").iter().map(|c| Conductor { source: st(c, "source"), name: st(c, "name"), avatar: st(c, "avatar"), quantity: u(c, "quantity") }).collect(),
        conductor_rewards: list("conductorRewards").iter().filter_map(|s| s.as_str().map(String::from)).collect(),
        treasure: v["treasure"].as_bool().unwrap_or(false),
        golden_kappa: v["goldenKappa"].as_bool().unwrap_or(false),
        shared: v["shared"].as_bool().unwrap_or(false),
        color: v["color"].as_str().and_then(|h| u32::from_str_radix(h, 16).ok()),
        record_level: v.pointer("/record/level").and_then(|l| l.as_u64()).unwrap_or(0),
    })
}

/// "4:40": minutes and seconds left, never below 0:00.
pub fn time_left(expires_ms: i64, now_ms: i64) -> String {
    let s = ((expires_ms - now_ms) as f64 / 1000.0).ceil().max(0.0) as i64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Over: the time ran out (a level-up would have moved it; a short grace covers clock skew and the next answer).
pub fn expired(t: &HypeTrain, now_ms: i64) -> bool {
    t.expires_ms.map(|e| now_ms > e + 3000).unwrap_or(false)
}

/// The bar's fill: the channel's color, darkened when white text would not read on it.
pub fn fill_color(color: Option<u32>) -> u32 {
    let c = color.unwrap_or(DEFAULT_COLOR);
    let (r, g, b) = (((c >> 16) & 0xff) as f32, ((c >> 8) & 0xff) as f32, (c & 0xff) as f32);
    let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
    if lum <= 0.55 {
        return c;
    }
    let k = 0.55 / lum;
    let d = |x: f32| ((x * k).round() as u32).min(255);
    (d(r) << 16) | (d(g) << 8) | d(b)
}

/// "1,234"
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", thousands(n), if n == 1 { one } else { many })
}

/// The lines under "Info".
pub fn info_lines(t: &HypeTrain) -> Vec<String> {
    let mut v = Vec::new();
    let subs = t.conductor_rewards.iter().any(|s| s == "SUBS");
    let bits = t.conductor_rewards.iter().any(|s| s == "BITS");
    match (subs, bits) {
        (true, true) => v.push("The top sub gifter and the top cheerer get a conductor badge".to_string()),
        (true, false) => v.push("The top sub gifter gets a conductor badge".to_string()),
        (false, true) => v.push("The top cheerer gets a conductor badge".to_string()),
        _ => {}
    }
    if t.golden_kappa {
        v.push("Golden Kappa Train".to_string());
    }
    if t.treasure {
        v.push("Treasure Train".to_string());
    }
    if t.shared {
        v.push("Shared with other channels".to_string());
    }
    if t.record_level > 0 {
        v.push(format!("Channel record: Level {}", t.record_level));
    }
    v
}

/// A live chat frame that can start or move a train: a sub / gift / Bits.
pub fn contributes(f: &Value) -> bool {
    match f["type"].as_str() {
        Some("chat") => f["msg"]["bits"].as_f64().or_else(|| f["message"]["bits"].as_f64()).map(|b| b > 0.0).unwrap_or(false),
        Some("chatnotice") => matches!(f["notice"]["msgId"].as_str(), Some("sub" | "resub" | "subgift" | "submysterygift" | "anonsubgift" | "anonsubmysterygift" | "giftpaidupgrade" | "primepaidupgrade")),
        _ => false,
    }
}

/// One Twitch source's bar: the train and when to ask next.
#[derive(Default)]
pub struct HypeState {
    pub train: Option<HypeTrain>,
    pub next: Option<Instant>,
    pub busy: bool,
}

impl ChatPane {
    fn hype_sources(&self) -> Vec<(String, agu_chat_core::protocol::SourceRef)> {
        self.store.sources.iter().filter(|s| s.source.platform == Platform::Twitch && !s.source.channel.starts_with("whisper:")).map(|s| (s.sk.clone(), s.source.clone())).collect()
    }

    /// Start the pane's one-second hype train loop when it should run (setting on, a Twitch channel in the pane).
    pub(super) fn ensure_hype_tick(&mut self, cx: &mut Context<Self>) {
        if self.hype_tick.is_some() || !self.settings.hype_train || self.hype_sources().is_empty() {
            return;
        }
        self.hype_tick = Some(cx.spawn(async move |this, cx| loop {
            if this.update(cx, |p, cx| p.hype_step(cx)).unwrap_or(false) {
                cx.background_executor().timer(Duration::from_secs(1)).await;
            } else {
                break;
            }
        }));
    }

    /// A sub or Bits in this source's chat: look for a train soon.
    pub(super) fn hype_poke(&mut self, sk: &str) {
        if let Some(s) = self.hype.get_mut(sk) {
            let soon = Instant::now() + SOON;
            if s.next.map(|n| n > soon).unwrap_or(true) {
                s.next = Some(soon);
            }
        }
    }

    /// One tick: drop what is over, ask for what is due, redraw the countdown. false stops the loop.
    fn hype_step(&mut self, cx: &mut Context<Self>) -> bool {
        let sources = if self.settings.hype_train { self.hype_sources() } else { Vec::new() };
        let had = self.hype.values().any(|s| s.train.is_some());
        self.hype.retain(|k, _| sources.iter().any(|(sk, _)| sk == k));
        if sources.is_empty() {
            self.hype.clear();
            self.hype_tick = None;
            if had {
                cx.notify();
            }
            return false;
        }
        let now = Instant::now();
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut shown = false;
        for (sk, src) in sources {
            let s = self.hype.entry(sk.clone()).or_default();
            if s.train.as_ref().map(|t| expired(t, now_ms)).unwrap_or(false) {
                s.train = None;
                s.next = Some(now);
                cx.notify();
            }
            shown |= s.train.is_some();
            if s.busy || s.next.map(|n| n > now).unwrap_or(false) {
                continue;
            }
            s.busy = true;
            let bridge = self.bridge_clone();
            cx.spawn(async move |this, cx| {
                let r = relay_api(&bridge, &src, "/api/twitch/hype-train", None, None).await;
                let _ = this.update(cx, |p, cx| {
                    let Some(s) = p.hype.get_mut(&sk) else { return };
                    s.busy = false;
                    if !r.ok {
                        s.next = Some(Instant::now() + ERROR_POLL);
                        return;
                    }
                    let t = parse(&r.body);
                    s.next = Some(Instant::now() + if t.is_some() { ACTIVE_POLL } else { IDLE_POLL });
                    if s.train != t {
                        if t.is_none() {
                            p.hype_open.remove(&sk);
                        }
                        let s = p.hype.get_mut(&sk).unwrap();
                        s.train = t;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        if shown {
            cx.notify(); // the countdown
        }
        true
    }

    /// The bars to draw above the messages, one per Twitch channel with a train on.
    pub(super) fn hype_bars(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !self.settings.hype_train {
            return Vec::new();
        }
        self.ensure_hype_tick(cx);
        let multi = self.hype_sources().len() > 1;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut out = Vec::new();
        for (sk, src) in self.hype_sources() {
            let Some(t) = self.hype.get(&sk).and_then(|s| s.train.clone()) else { continue };
            if expired(&t, now_ms) {
                continue;
            }
            out.push(self.hype_bar(&sk, if multi { Some(src.channel.as_str()) } else { None }, &t, now_ms, cx));
        }
        out
    }

    fn hype_bar(&mut self, sk: &str, channel: Option<&str>, t: &HypeTrain, now_ms: i64, cx: &mut Context<Self>) -> AnyElement {
        // AGU_HYPE_OPEN: start expanded (screenshots without clicking)
        let open = self.hype_open.contains(sk) != std::env::var("AGU_HYPE_OPEN").is_ok();
        let fill = fill_color(t.color);
        let left = t.expires_ms.map(|e| time_left(e, now_ms)).unwrap_or_default();
        let id = |p: &str| ElementId::from(SharedString::from(format!("hype-{p}-{sk}")));

        // the bar: a fill as wide as the level's progress, the text over it
        let title = match channel {
            Some(ch) => format!("Hype Train · #{ch}"),
            None => "Hype Train".to_string(),
        };
        let sk_toggle = sk.to_string();
        let bar = div()
            .id(id("bar"))
            .relative()
            .h(px(52.))
            .w_full()
            .cursor_pointer()
            .overflow_hidden()
            .rounded_t(px(6.))
            .when(!open, |d| d.rounded_b(px(6.)))
            .bg(c(0x26262c))
            .child(div().absolute().top_0().left_0().h_full().w(relative(t.percent as f32 / 100.0)).bg(c(fill)))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .items_center()
                    .px(px(12.))
                    .gap(px(8.))
                    .text_color(c(theme::WHITE))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(px(13.)).line_height(px(17.)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(title)))
                            .child(div().text_size(px(17.)).line_height(px(22.)).child(SharedString::from(format!("Lvl {}", t.level)))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .flex_col()
                            .items_end()
                            .child(div().text_size(px(13.)).line_height(px(17.)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(left)))
                            .child(div().text_size(px(17.)).line_height(px(22.)).font_weight(FontWeight::SEMIBOLD).child(SharedString::from(format!("{}%", t.percent)))),
                    )
                    .child(div().flex_none().w(px(20.)).flex().justify_center().child(icon(if open { icons::CHEVRON_UP } else { icons::CHEVRON }, 16.0))),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                if !this.hype_open.remove(&sk_toggle) {
                    this.hype_open.insert(sk_toggle.clone());
                }
                cx.notify();
            }));

        let mut card = div().id(id("card")).relative().flex_none().mx(px(6.)).mt(px(6.)).rounded(px(6.)).border_1().border_color(c(theme::LINE)).bg(c(theme::RAISED)).shadow_lg().child(bar);
        if open {
            // like twitch.tv: the details hang over the messages instead of pushing them away, and scroll when tall
            let details = div()
                .id(id("details"))
                .absolute()
                .top(px(52.))
                .left(px(-1.))
                .right(px(-1.))
                .max_h(px(DETAILS_MAX_H))
                .overflow_y_scroll()
                .rounded_b(px(6.))
                .border_1()
                .border_t_0()
                .border_color(c(theme::LINE))
                .bg(c(theme::RAISED))
                .shadow_xl()
                .occlude()
                .child(self.hype_details(t, cx));
            card = card.child(deferred(details).with_priority(2));
        }
        card.into_any_element()
    }

    fn hype_details(&mut self, t: &HypeTrain, cx: &mut Context<Self>) -> AnyElement {
        let heading = |s: &'static str| div().mt(px(10.)).mb(px(6.)).text_size(px(13.)).line_height(px(17.)).font_weight(FontWeight::BOLD).text_color(c(theme::TEXT)).child(s);
        let mut d = div().px(px(10.)).pt(px(10.)).pb(px(10.)).flex().flex_col();

        // what is left of the level
        let mut need = InlineContentBuilder::new(13.0);
        need.icon(icons::BITS_100, 14.0, token(theme::PURPLE_300));
        need.gap(4.0);
        need.text(&plural(t.bits, "Bit", "Bits"), token(theme::TEXT), FontWeight::BOLD);
        need.text(" or ", token(theme::MUTED), FontWeight::NORMAL);
        need.icon(icons::GIFT, 14.0, token(theme::PURPLE_300));
        need.gap(4.0);
        need.text(&plural(t.subs, "Sub", "Subs"), token(theme::TEXT), FontWeight::BOLD);
        d = d.child(
            div()
                .rounded(px(6.))
                .bg(a(theme::WHITE, 0.06))
                .px(px(10.))
                .py(px(8.))
                .flex()
                .flex_col()
                .items_center()
                .child(self.flow(&need.finish(), Rc::new(vec![]), cx))
                .child(div().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::MUTED)).child(SharedString::from(format!("remaining to complete level {}", t.level)))),
        );

        // the level's emotes
        if !t.emotes.is_empty() {
            let mut em = InlineContentBuilder::new(13.0).ratio(2.6);
            for (i, e) in t.emotes.iter().enumerate() {
                if i > 0 {
                    em.gap(6.0);
                }
                em.image(&e.url, 32.0, 0.0, Some(Rc::new(Tip::new(e.url.clone(), e.token.clone(), "Hype Train emote"))));
            }
            d = d.child(heading("Unlockable Emotes")).child(div().rounded(px(6.)).bg(a(theme::WHITE, 0.04)).px(px(8.)).py(px(4.)).child(self.flow(&em.finish(), Rc::new(vec![]), cx)));
        }

        // info
        let info = info_lines(t);
        if !info.is_empty() {
            d = d.child(heading("Info"));
            for line in info {
                let ic = if line.contains("conductor") { icons::CROWN } else if line.starts_with("Channel record") { icons::TROPHY } else { icons::FLAME };
                d = d.child(
                    div()
                        .mb(px(4.))
                        .rounded(px(6.))
                        .bg(a(theme::WHITE, 0.04))
                        .px(px(10.))
                        .py(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .line_height(px(16.))
                        .text_color(c(theme::TEXT))
                        .child(div().flex_none().text_color(c(theme::AMBER_400)).child(icon(ic, 14.0)))
                        .child(div().flex_1().min_w_0().child(SharedString::from(line))),
                );
            }
        }

        // conductors
        if !t.conductors.is_empty() {
            d = d.child(heading("Top Contributors"));
            for co in &t.conductors {
                let mut f = InlineContentBuilder::new(13.0).ratio(1.9);
                if !co.avatar.is_empty() {
                    f.image(&co.avatar, 24.0, 12.0, None);
                    f.gap(8.0);
                }
                f.text(&co.name, token(theme::TEXT), FontWeight::SEMIBOLD);
                let what = if co.source == "BITS" { plural(co.quantity, "Bit", "Bits") } else { plural(co.quantity, "Sub", "Subs") };
                f.text(&format!("  {what}"), token(theme::MUTED), FontWeight::NORMAL);
                d = d.child(div().mb(px(2.)).child(self.flow(&f.finish(), Rc::new(vec![]), cx)));
            }
        }
        d.into_any_element()
    }
}

/// per pane: one state per Twitch source key
pub type HypeMap = HashMap<String, HypeState>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer() -> Value {
        json!({"active": true, "id": "t1", "level": 2, "percent": 17, "bits": 3300, "subs": 7, "expiresAt": "2026-11-19T01:00:07Z",
            "emotes": [{"id": "e1", "token": "KittyHype", "url": "https://static-cdn.jtvnw.net/emoticons/v2/e1/default/dark/2.0"}, {"id": "e2", "token": "x", "url": ""}],
            "conductors": [{"source": "BITS", "login": "c", "name": "Cheerer", "avatar": "", "quantity": 500}],
            "conductorRewards": ["BITS", "SUBS"], "treasure": true, "goldenKappa": false, "shared": false, "color": "ffbf00", "record": {"level": 16}})
    }

    #[test]
    fn parses_the_core_answer() {
        let t = parse(&answer()).unwrap();
        assert_eq!(t.level, 2);
        assert_eq!(t.percent, 17);
        assert_eq!((t.bits, t.subs), (3300, 7));
        assert_eq!(t.expires_ms, Some(chrono::DateTime::parse_from_rfc3339("2026-11-19T01:00:07Z").unwrap().timestamp_millis()));
        assert_eq!(t.emotes.len(), 1, "an emote without a picture is left out");
        assert_eq!(t.conductors[0].name, "Cheerer");
        assert_eq!(t.color, Some(0xffbf00));
        assert_eq!(t.record_level, 16);
        assert!(parse(&json!({"active": false})).is_none());
        assert!(parse(&Value::Null).is_none());
    }

    #[test]
    fn countdown() {
        assert_eq!(time_left(280_000, 0), "4:40");
        assert_eq!(time_left(5_200, 0), "0:06");
        assert_eq!(time_left(0, 10_000), "0:00");
        assert_eq!(time_left(3_600_000, 0), "60:00");
    }

    #[test]
    fn ends_when_time_runs_out() {
        let t = HypeTrain { expires_ms: Some(10_000), ..Default::default() };
        assert!(!expired(&t, 10_000));
        assert!(!expired(&t, 12_000));
        assert!(expired(&t, 13_001));
        assert!(!expired(&HypeTrain::default(), i64::MAX));
    }

    #[test]
    fn fill_keeps_dark_colors_and_darkens_light_ones() {
        assert_eq!(fill_color(None), DEFAULT_COLOR);
        assert_eq!(fill_color(Some(0xd64545)), 0xd64545);
        let f = fill_color(Some(0xffbf00));
        assert_ne!(f, 0xffbf00);
        let (r, g, b) = (((f >> 16) & 0xff) as f32, ((f >> 8) & 0xff) as f32, (f & 0xff) as f32);
        assert!((0.299 * r + 0.587 * g + 0.114 * b) / 255.0 <= 0.56);
        assert!(r > g && g > b, "the hue stays");
    }

    #[test]
    fn info_from_flags() {
        let t = parse(&answer()).unwrap();
        assert_eq!(info_lines(&t), vec!["The top sub gifter and the top cheerer get a conductor badge", "Treasure Train", "Channel record: Level 16"]);
        assert!(info_lines(&HypeTrain::default()).is_empty());
    }

    #[test]
    fn subs_and_bits_poke_the_poll() {
        assert!(contributes(&json!({"type": "chatnotice", "notice": {"msgId": "subgift"}})));
        assert!(contributes(&json!({"type": "chat", "msg": {"bits": 100}})));
        assert!(!contributes(&json!({"type": "chat", "msg": {"text": "hi"}})));
        assert!(!contributes(&json!({"type": "chatnotice", "notice": {"msgId": "raid"}})));
        assert!(!contributes(&json!({"type": "conn"})));
    }

    #[test]
    fn numbers() {
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(plural(1, "Sub", "Subs"), "1 Sub");
        assert_eq!(plural(3300, "Bit", "Bits"), "3,300 Bits");
    }
}
