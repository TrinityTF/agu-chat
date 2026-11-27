//! The hype train bar at the top of a Twitch chat, like twitch.tv's: collapsed it is a progress bar ("Hype Train",
//! "Lvl N", time left, percent of the level), expanded it adds what is left to finish the level in Bits or subs,
//! the level's emotes, a few lines of info and the conductors (top sub gifter / top cheerer).
//! The bar itself (the data and its rules are `agu_chat_model::hype_train`).
use std::rc::Rc;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::Platform;
use gpui::{deferred, div, prelude::*, px, relative, AnyElement, ClickEvent, Context, ElementId, FontWeight, SharedString};

use super::chat_pane::ChatPane;
use super::inline::Tip;
use super::mod_caps::relay_api;
use super::pane_render::InlineContentBuilder;
use super::ui_util::token;
use crate::icons::{self, icon};
use crate::theme::{self, a, c};
pub use agu_chat_model::hype_train::*;

/// the open details scroll past this height
pub const DETAILS_MAX_H: f32 = 380.0;

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

