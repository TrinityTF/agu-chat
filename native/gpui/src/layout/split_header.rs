//! Port of src/renderer/layout/SplitHeader.tsx.
//! The ~26px header on every split: platforms + channel names, live pill + stream title, mod-mode toggle and the
//! "⋯" menu.
//! Each platform logo says two things: it is in colour while that chat is connected and grey (pulsing while it
//! connects) when it is not, and a small green dot at its top right means the channel is live. A platform that
//! does not know whether the channel is live shows no dot. Hovering says which, and why a chat is not connected.
//! A merged split shows one chip per platform (logo, "2x" when it holds several channels of it); names appear when
//! there is room. Clicking a chip splits its channels out; dragging one drops it on any split edge.
//!
//! The Modes button (layout/channel_modes.rs) sits before the mod toggle; it only shows to someone who can moderate.
use std::time::Duration;

use agu_chat_core::protocol::{ConnFrame, Platform, SourceRef};
use gpui::{
    div, prelude::*, px, Animation, AnimationExt, AnyElement, ClickEvent, Context, Window,
};

use crate::icons;
use crate::layout::app::{AguApp, DragGhost, Dialog, SourcesDrag, SplitDrag};
use crate::layout::hotkeys::{hotkey_hint, HotkeyAction};
use crate::layout::tree::{is_whisper, short_name, source_key, Leaf};
use crate::layout::ui::{tip, MenuEntry, MenuItem};
use crate::theme::{self, c};

fn state_of(st: Option<&ConnFrame>) -> &str {
    st.map(|s| s.state.as_str()).unwrap_or("connecting")
}

/// The chat is connected: messages arrive. Only the `open` state is; connecting, offline, error and unavailable
/// all mean no chat is being read.
pub fn is_connected(st: Option<&ConnFrame>) -> bool {
    state_of(st) == "open"
}

/// The channel is live right now (as far as its platform says; unknown is not live).
pub fn is_live(st: Option<&ConnFrame>) -> bool {
    st.map(|s| s.live == Some(true)).unwrap_or(false)
}

/// What hovering a logo says about one source: "Connected · Live", or "Not connected: <why>".
pub fn conn_label(st: Option<&ConnFrame>) -> String {
    let live = match st.and_then(|s| s.live) {
        Some(true) => " · Live",
        Some(false) => " · Not live",
        None => "",
    };
    if is_connected(st) {
        return format!("Connected{live}");
    }
    let why = st.and_then(|s| s.detail.clone()).filter(|d| !d.trim().is_empty()).unwrap_or_else(|| {
        match state_of(st) {
            "connecting" => "Connecting",
            "offline" => "Not live",
            "error" => "Error",
            _ => "Unavailable",
        }
        .to_string()
    });
    // "Not live" twice says nothing new
    let live = if why == "Not live" && live == " · Not live" { "" } else { live };
    format!("Not connected: {why}{live}")
}

/// A source's logo as the header shows it: in colour when connected, grey (pulsing while it connects) when not,
/// with a green dot at its top right while the channel is live.
fn source_mark(id: (&'static str, usize), s: &SourceRef, connected: bool, live: bool, pulsing: bool) -> gpui::Div {
    let logo = if connected { icons::source_icon(s, 14., true) } else { icons::source_icon(s, 14., false).color(c(theme::FAINT)) };
    let logo = div().id(id).flex_none().child(logo);
    let logo = if pulsing {
        logo.with_animation(("mark-pulse", id.1), Animation::new(Duration::from_secs(2)).repeat().with_easing(gpui::pulsating_between(0.4, 1.0)), |d, delta| d.opacity(delta))
            .into_any_element()
    } else {
        logo.into_any_element()
    };
    div().relative().flex_none().child(logo).when(live, |d| {
        d.child(
            div()
                .absolute()
                .right(px(-3.))
                .top(px(-3.))
                .size(px(8.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(c(theme::RAISED))
                .child(div().size(px(6.)).rounded_full().bg(c(theme::EMERALD_500))),
        )
    })
}

impl AguApp {
    pub fn render_header(&mut self, tab_id: &str, leaf: &Leaf, focused: bool, settings_button: bool, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let status = self.panes.get(&leaf.id).map(|s| s.view.read(cx).status.clone()).unwrap_or_default();
        let rows: Vec<_> = leaf.sources.iter().map(|s| (s.clone(), status.get(&source_key(s)).cloned())).collect();
        let live = rows.iter().find(|(_, st)| st.as_ref().map(|s| s.live == Some(true)).unwrap_or(false));
        let title = live
            .and_then(|(_, st)| st.as_ref()?.title.clone())
            .or_else(|| rows.iter().find_map(|(_, st)| st.as_ref().and_then(|s| s.title.clone()).filter(|t| !t.is_empty())))
            .unwrap_or_default()
            .trim()
            .to_string();

        let merged = rows.len() > 1;
        let mut groups: Vec<(Platform, Vec<usize>)> = Vec::new();
        for (i, (s, _)) in rows.iter().enumerate() {
            match groups.iter_mut().find(|(p, _)| *p == s.platform) {
                Some((_, v)) => v.push(i),
                None => groups.push((s.platform, vec![i])),
            }
        }
        let dot_tip = |i: usize| {
            let (s, st) = &rows[i];
            format!("{} {}: {}", icons::platform_label(s.platform), short_name(s), conn_label(st.as_ref()))
        };

        let tab_id = tab_id.to_string();
        let leaf_id = leaf.id.clone();
        let entity = cx.entity();
        let drag_label: gpui::SharedString = leaf.sources.iter().map(short_name).collect::<Vec<_>>().join(", ").into();

        let mut header = div()
            .id(gpui::SharedString::from(format!("hdr-{leaf_id}")))
            .flex()
            .h(px(26.))
            .flex_none()
            .items_center()
            .gap(px(8.))
            .border_b_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .pl(px(8.))
            .pr(px(4.))
            .text_size(px(12.))
            .text_color(if focused { c(theme::TEXT) } else { c(theme::MUTED) })
            .on_drag(SplitDrag { leaf_id: leaf_id.clone(), label: drag_label.clone() }, {
                let entity = entity.clone();
                move |d: &SplitDrag, _offset, _w, cx| {
                    let id = d.leaf_id.clone();
                    entity.update(cx, |this, cx| {
                        this.sv.dragging = Some(crate::layout::split_view::Dragging::Split { leaf_id: id });
                        cx.notify();
                    });
                    cx.new(|_| DragGhost(d.label.clone()))
                }
            });

        if merged {
            let mut chips = div().flex().min_w_0().flex_shrink().items_center().gap(px(6.)).overflow_hidden();
            for (gi, (platform, idxs)) in groups.iter().enumerate() {
                // in colour only when every channel of the platform is connected; the dot when any is live
                let connected = idxs.iter().all(|&i| is_connected(rows[i].1.as_ref()));
                let live = idxs.iter().any(|&i| is_live(rows[i].1.as_ref()));
                let pulsing = !connected && idxs.iter().any(|&i| state_of(rows[i].1.as_ref()) == "connecting");
                let keys: Vec<String> = idxs.iter().map(|&i| source_key(&rows[i].0)).collect();
                let names = idxs.iter().map(|&i| short_name(&rows[i].0)).collect::<Vec<_>>().join(", ");
                let tooltip = idxs.iter().map(|&i| dot_tip(i)).chain(std::iter::once("Click to split out or open the stream, or drag onto a split edge".to_string())).collect::<Vec<_>>().join("\n");
                let multi = idxs.len() > 1;
                let platform = *platform;
                let chip_key = format!("chip:{leaf_id}:{}", platform.as_str());
                let ck = chip_key.clone();
                let (lid, tid) = (leaf_id.clone(), tab_id.clone());
                let chip = div()
                    .id(gpui::SharedString::from(chip_key.clone()))
                    .relative()
                    .flex()
                    .min_w_0()
                    .flex_none()
                    .items_center()
                    .gap(px(4.))
                    .rounded(px(4.))
                    .px(px(4.))
                    .py(px(2.))
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .tooltip(tip(tooltip))
                    .on_drag(SourcesDrag { leaf_id: leaf_id.clone(), keys: keys.clone(), label: names.clone().into() }, {
                        let entity = entity.clone();
                        move |d: &SourcesDrag, _offset, _w, cx| {
                            let (l, k) = (d.leaf_id.clone(), d.keys.clone());
                            entity.update(cx, |this, cx| {
                                this.sv.dragging = Some(crate::layout::split_view::Dragging::Sources { leaf_id: l, keys: k });
                                cx.notify();
                            });
                            cx.new(|_| DragGhost(d.label.clone()))
                        }
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        let entries = this.chip_entries(&tid, &lid, platform);
                        let anchor = this.anchor(&ck);
                        this.toggle_menu(&ck, anchor, false, entries, cx);
                    }))
                    .child(
                        source_mark(("chip-logo", gi), &rows[idxs[0]].0, connected, live, pulsing)
                            .when(multi, |d| {
                                d.child(
                                    div()
                                        .absolute()
                                        .bottom(px(-4.))
                                        .right(px(-8.))
                                        .rounded(px(2.))
                                        .bg(c(theme::RAISED_2))
                                        .px(px(2.))
                                        .text_size(px(8.))
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .line_height(px(10.))
                                        .text_color(c(theme::TEXT))
                                        .child(format!("{}x", idxs.len())),
                                )
                            }),
                    )
                    .when(width >= 672.0, |d| {
                        d.child(div().max_w(px(110.)).truncate().font_weight(gpui::FontWeight::MEDIUM).when(multi, |n| n.pl(px(6.))).child(names.clone()))
                    })
                    .child(self.probe(chip_key, cx));
                chips = chips.child(chip);
            }
            header = header.child(chips);
        } else {
            for (i, (s, st)) in rows.iter().enumerate() {
                let st = st.as_ref();
                let mark = source_mark(("hdr-logo", i), s, is_connected(st), is_live(st), state_of(st) == "connecting");
                header = header.child(
                    div()
                        .id(("hdr-src", i))
                        .flex()
                        .min_w_0()
                        .flex_shrink()
                        .items_center()
                        .gap(px(6.))
                        .tooltip(tip(dot_tip(i)))
                        .child(mark)
                        .child(div().min_w_0().truncate().font_weight(gpui::FontWeight::MEDIUM).child(short_name(s))),
                );
            }
        }

        if live.is_some() {
            header = header.child(
                div()
                    .flex_none()
                    .rounded(px(2.))
                    .bg(c(theme::RED_600))
                    .px(px(4.))
                    .text_size(px(9.))
                    .font_weight(gpui::FontWeight::BOLD)
                    .line_height(px(14.))
                    .text_color(c(theme::WHITE))
                    .child(crate::chat::inline::tracked("LIVE")),
            );
        }
        header = if !title.is_empty() {
            header.child(div().id("hdr-title").min_w_0().flex_1().truncate().text_size(px(11.)).text_color(c(theme::FAINT)).tooltip(tip(title.clone())).child(title))
        } else {
            header.child(div().flex_1())
        };

        // Modes button (ChannelModes.tsx): its own entity per split, nothing to show unless the account can moderate
        let sig = leaf.sources.iter().map(source_key).collect::<Vec<_>>().join("|");
        let modes_view = match self.modes.get(&leaf_id) {
            Some((s, v)) if *s == sig => v.clone(),
            _ => {
                let v = cx.new(|cx| crate::layout::channel_modes::ModesButton::new(&leaf.sources, cx));
                self.modes.insert(leaf_id.clone(), (sig, v.clone()));
                v
            }
        };
        // Whispers have no moderation: nothing to toggle or set.
        let whispers_only = leaf.sources.iter().all(is_whisper);
        if !whispers_only {
            header = header.child(modes_view);
        }

        // Mod mode toggle: only where this account can moderate (the streamer or a mod)
        let mod_on = leaf.mod_mode;
        let can_mod = self.panes.get(&leaf.id).map(|s| s.view.read(cx).can_moderate_here()).unwrap_or(false);
        let (t1, l1) = (tab_id.clone(), leaf_id.clone());
        header = header.when(!whispers_only && can_mod, |h| h.child(
            div()
                .id("hdr-mod")
                .flex()
                .size(px(20.))
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .when(mod_on, |d| d.bg(c(theme::ACCENT)).text_color(c(theme::WHITE)))
                .when(!mod_on, |d| d.text_color(c(theme::FAINT)).hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT))))
                .tooltip(tip(if mod_on { "Mod mode on" } else { "Mod mode off" }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| this.toggle_mod(&t1, &l1, cx)))
                .child(icons::icon(icons::SWORD, 14.)),
        ));

        // Split menu
        let menu_key = format!("split-menu:{leaf_id}");
        let (t2, l2, mk) = (tab_id.clone(), leaf_id.clone(), menu_key.clone());
        header = header.child(
            div()
                .id("hdr-menu")
                .relative()
                .flex()
                .size(px(20.))
                .flex_none()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .text_color(c(theme::FAINT))
                .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                .tooltip(tip("Split menu"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                    let entries = this.split_menu_entries(&t2, &l2);
                    let anchor = this.anchor(&mk);
                    this.toggle_menu(&mk, anchor, true, entries, cx);
                }))
                .child(icons::icon(icons::DOTS_H, 14.))
                .child(self.probe(menu_key, cx)),
        );

        if settings_button {
            let hint = hotkey_hint(&self.hotkey_table, HotkeyAction::OpenSettings);
            header = header.child(
                div()
                    .id("hdr-settings")
                    .flex()
                    .size(px(20.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .text_color(c(theme::FAINT))
                    .hover(|s| s.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                    .tooltip(tip(match hint {
                        Some(k) => format!("Settings ({k})"),
                        None => "Settings".to_string(),
                    }))
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.open_dialog(Dialog::Settings, w, cx)))
                    .child(icons::icon(icons::GEAR, 14.)),
            );
        }
        header.into_any_element()
    }

    fn chip_entries(&self, tab_id: &str, leaf_id: &str, platform: Platform) -> Vec<MenuEntry> {
        let tab = self.layout.tabs.iter().find(|t| t.id == tab_id);
        let Some(leaf) = tab.and_then(|t| crate::layout::tree::find_leaf(t.root.as_ref(), leaf_id)) else { return vec![] };
        let srcs: Vec<_> = leaf.sources.iter().filter(|s| s.platform == platform).collect();
        let mut out = Vec::new();
        for s in &srcs {
            let key = source_key(s);
            let (t1, l1, k1) = (tab_id.to_string(), leaf_id.to_string(), key.clone());
            let (t3, l3, k3) = (t1.clone(), l1.clone(), key.clone());
            out.push(MenuEntry::Heading(format!("{}: {}", icons::platform_label(s.platform), short_name(s))));
            // where it goes is picked on the splits, with the overlay a drag shows
            out.push(MenuEntry::Item(MenuItem::new("Split out\u{2026}", move |this, _w, cx| this.start_pick(&t1, &l1, vec![k1.clone()], cx))));
            if let Some(url) = stream_url(s) {
                out.push(MenuEntry::Item(MenuItem::new("Stream link", move |this, _w, _cx| this.bridge.open_external(url.clone()))));
            }
            out.push(MenuEntry::Item(MenuItem::new("Remove from this split", move |this, _w, cx| this.remove_source(&t3, &l3, &k3, cx)).danger()));
        }
        if srcs.len() > 1 {
            let keys: Vec<String> = srcs.iter().map(|s| source_key(s)).collect();
            let (t, l) = (tab_id.to_string(), leaf_id.to_string());
            out.push(MenuEntry::Sep);
            out.push(MenuEntry::Item(MenuItem::new(format!("Split all {} {} out\u{2026}", srcs.len(), icons::platform_label(platform)), move |this, _w, cx| {
                this.start_pick(&t, &l, keys.clone(), cx)
            })));
        }
        out
    }

    pub fn split_menu_entries(&self, tab_id: &str, leaf_id: &str) -> Vec<MenuEntry> {
        let close_key = hotkey_hint(&self.hotkey_table, HotkeyAction::CloseSplit);
        let (t0, l0) = (tab_id.to_string(), leaf_id.to_string());
        let (t1, l1) = (t0.clone(), l0.clone());
        let (t2, l2) = (t0.clone(), l0.clone());
        let (t3, l3) = (t0.clone(), l0.clone());
        let l4 = l0.clone();
        vec![
            MenuEntry::Item(MenuItem::new("Change channel(s)\u{2026}", move |this, w, cx| this.open_dialog(Dialog::Change { tab_id: t0.clone(), leaf_id: l0.clone() }, w, cx))),
            MenuEntry::Item(MenuItem::new("Add split right", move |this, w, cx| this.open_dialog(Dialog::Split { tab_id: t1.clone(), leaf_id: l1.clone(), below: false }, w, cx))),
            MenuEntry::Item(MenuItem::new("Add split below", move |this, w, cx| this.open_dialog(Dialog::Split { tab_id: t2.clone(), leaf_id: l2.clone(), below: true }, w, cx))),
            MenuEntry::Sep,
            MenuEntry::Item(MenuItem::new("Reload emotes", move |this, _w, cx| this.reload_emotes(&l4, cx))),
            MenuEntry::Sep,
            MenuEntry::Item(MenuItem::new("Close split", move |this, _w, cx| this.close_leaf(&t3, &l3, cx)).hint(close_key).danger()),
        ]
    }
}

/// The page a source's stream plays on, for "Stream link"; None for a whisper conversation (no stream) or a channel
/// that does not parse. Built from the same parsers the platforms normalise channels with.
pub fn stream_url(s: &SourceRef) -> Option<String> {
    use agu_chat_core::{kick::vendor::kick::parse_kick_source, rumble::vendor::rumble::parse_rumble_source, youtube::vendor::youtube::parse_youtube_source};
    let ch = s.channel.trim();
    match s.platform {
        Platform::Twitch => {
            if is_whisper(s) {
                return None;
            }
            let login = if ch.to_lowercase().contains("twitch.tv/") { ch.split(['?', '#']).next().unwrap_or("").trim_end_matches('/').rsplit('/').next().unwrap_or("") } else { ch };
            let login = login.trim_start_matches(['#', '@']).to_lowercase();
            let ok = (1..=25).contains(&login.len()) && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            ok.then(|| format!("https://www.twitch.tv/{login}"))
        }
        Platform::Kick => parse_kick_source(ch).map(|slug| format!("https://kick.com/{slug}")),
        Platform::Youtube => parse_youtube_source(ch).map(|y| match y.kind {
            "video" => format!("https://www.youtube.com/watch?v={}", y.id),
            "channel" => format!("https://www.youtube.com/channel/{}/live", y.id),
            _ => format!("https://www.youtube.com/{}/live", y.id), // a handle: "@name"
        }),
        Platform::Rumble => parse_rumble_source(ch).map(|r| match r.space {
            Some(space) => format!("https://rumble.com/{space}/{}", r.id),
            None => format!("https://rumble.com/embed/{}/", r.id), // a video id
        }),
        Platform::X => agu_chat_core::x::normalize_channel(ch).ok().map(|h| match h.strip_prefix("broadcast:") {
            Some(id) => format!("https://x.com/i/broadcasts/{id}"),
            None => format!("https://x.com/{h}"),
        }),
    }
}

#[allow(dead_code)]
fn _w(_: &mut Window) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(v: serde_json::Value) -> ConnFrame {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn only_open_is_connected() {
        assert!(is_connected(Some(&conn(serde_json::json!({"state": "open"})))));
        for s in ["connecting", "offline", "error", "unavailable"] {
            assert!(!is_connected(Some(&conn(serde_json::json!({"state": s})))), "{s}");
        }
        assert!(!is_connected(None));
    }

    #[test]
    fn live_is_separate_from_connected() {
        // reconnecting while the stream runs: grey logo, live dot
        let f = conn(serde_json::json!({"state": "connecting", "live": true, "detail": "Reconnecting to YouTube... (live chat poll returned 503)"}));
        assert!(!is_connected(Some(&f)) && is_live(Some(&f)));
        // a connected chat on an offline channel (Twitch, Kick): colour, no dot
        let f = conn(serde_json::json!({"state": "open", "live": false}));
        assert!(is_connected(Some(&f)) && !is_live(Some(&f)));
        // a platform that does not say: no dot
        assert!(!is_live(Some(&conn(serde_json::json!({"state": "open"})))));
        assert!(!is_live(None));
    }

    #[test]
    fn stream_urls() {
        let u = |platform: Platform, channel: &str| stream_url(&SourceRef { platform, channel: channel.into() });
        // Twitch: the login, from whatever was typed
        assert_eq!(u(Platform::Twitch, "esfandtv").as_deref(), Some("https://www.twitch.tv/esfandtv"));
        assert_eq!(u(Platform::Twitch, "#EsfandTV").as_deref(), Some("https://www.twitch.tv/esfandtv"));
        assert_eq!(u(Platform::Twitch, "https://www.twitch.tv/EsfandTV/").as_deref(), Some("https://www.twitch.tv/esfandtv"));
        // a whisper conversation has no stream
        let whisper = agu_chat_core::twitch::whisper::channel_for("bob");
        assert!(is_whisper(&SourceRef { platform: Platform::Twitch, channel: whisper.clone() }));
        assert_eq!(u(Platform::Twitch, &whisper), None);
        assert_eq!(u(Platform::Twitch, "has space"), None);
        // Kick: the slug as its URL spells it
        assert_eq!(u(Platform::Kick, "some_user").as_deref(), Some("https://kick.com/some-user"));
        assert_eq!(u(Platform::Kick, "https://kick.com/Foo").as_deref(), Some("https://kick.com/foo"));
        // YouTube: a handle's or a channel id's live page, a pinned video's watch page
        assert_eq!(u(Platform::Youtube, "@esfandtv").as_deref(), Some("https://www.youtube.com/@esfandtv/live"));
        assert_eq!(u(Platform::Youtube, "lofigirl").as_deref(), Some("https://www.youtube.com/@lofigirl/live"));
        assert_eq!(u(Platform::Youtube, "UCSJ4gkVC6NrvII8umztf0Ow").as_deref(), Some("https://www.youtube.com/channel/UCSJ4gkVC6NrvII8umztf0Ow/live"));
        assert_eq!(u(Platform::Youtube, "https://www.youtube.com/watch?v=jfKfPfyJRdk").as_deref(), Some("https://www.youtube.com/watch?v=jfKfPfyJRdk"));
        assert_eq!(u(Platform::Youtube, ""), None);
        // Rumble: the channel page in its namespace, or a video's player
        assert_eq!(u(Platform::Rumble, "c/foo").as_deref(), Some("https://rumble.com/c/foo"));
        assert_eq!(u(Platform::Rumble, "user/foobar").as_deref(), Some("https://rumble.com/user/foobar"));
        assert_eq!(u(Platform::Rumble, "Foo").as_deref(), Some("https://rumble.com/c/Foo"));
        assert_eq!(u(Platform::Rumble, "v7capuk").as_deref(), Some("https://rumble.com/embed/v7capuk/"));
        // X: the profile, or a broadcast
        assert_eq!(u(Platform::X, "esfandtv").as_deref(), Some("https://x.com/esfandtv"));
        assert_eq!(u(Platform::X, "@EsfandTV").as_deref(), Some("https://x.com/esfandtv"));
        assert_eq!(u(Platform::X, "broadcast:1DGleVdEapOJL").as_deref(), Some("https://x.com/i/broadcasts/1DGleVdEapOJL"));
        assert_eq!(u(Platform::X, ""), None);
    }

    #[test]
    fn hover_text() {
        assert_eq!(conn_label(Some(&conn(serde_json::json!({"state": "open", "live": true, "detail": "Live"})))), "Connected · Live");
        assert_eq!(conn_label(Some(&conn(serde_json::json!({"state": "open", "live": false})))), "Connected · Not live");
        assert_eq!(conn_label(Some(&conn(serde_json::json!({"state": "open"})))), "Connected");
        assert_eq!(
            conn_label(Some(&conn(serde_json::json!({"state": "connecting", "live": true, "detail": "Reconnecting to YouTube... (live chat poll returned 503)"})))),
            "Not connected: Reconnecting to YouTube... (live chat poll returned 503) · Live"
        );
        assert_eq!(conn_label(Some(&conn(serde_json::json!({"state": "offline", "live": false, "detail": "Not live"})))), "Not connected: Not live");
        assert_eq!(conn_label(Some(&conn(serde_json::json!({"state": "error"})))), "Not connected: Error");
        assert_eq!(conn_label(None), "Not connected: Connecting");
    }
}
