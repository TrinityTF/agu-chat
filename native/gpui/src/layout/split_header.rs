//! Port of src/renderer/layout/SplitHeader.tsx.
//! The ~26px header on every split: platforms + channel names, connection dots, live pill + stream title,
//! mod-mode toggle and the "⋯" menu.
//! A merged split shows one chip per platform (icon, "2x" when it holds several channels of it, a status dot);
//! names appear when there is room. Clicking a chip splits its channels out; dragging one drops it on any split edge.
//!
//! The Modes button (layout/channel_modes.rs) sits before the mod toggle; it only shows to someone who can moderate.
use std::time::Duration;

use agu_chat_core::protocol::{ConnFrame, Platform};
use gpui::{
    div, prelude::*, px, Animation, AnimationExt, AnyElement, ClickEvent, Context, Window,
};

use crate::icons;
use crate::layout::app::{AguApp, DragGhost, Dialog, SourcesDrag, SplitDrag};
use crate::layout::hotkeys::{hotkey_hint, HotkeyAction};
use crate::layout::tree::{short_name, source_key, Edge, Leaf};
use crate::layout::ui::{tip, MenuEntry, MenuItem};
use crate::theme::{self, c};

/// The dot a chip shows for several channels: the one most worth noticing.
const WORST: [&str; 5] = ["error", "unavailable", "connecting", "offline", "open"];

/// (colour, label) of a `conn` state; `connecting` also pulses.
fn dot(state: &str) -> (u32, &'static str) {
    match state {
        "connecting" => (theme::AMBER_400, "Connecting"),
        "open" => (theme::EMERALD_500, "Connected"),
        "offline" => (theme::ZINC_500, "Offline"),
        "error" => (theme::RED_500, "Error"),
        _ => (theme::ORANGE_500, "Unavailable"),
    }
}

fn state_of(st: Option<&ConnFrame>) -> &str {
    st.map(|s| s.state.as_str()).unwrap_or("connecting")
}

fn pulse(state: &str) -> bool {
    state == "connecting"
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
            let (_, label) = dot(state_of(st.as_ref()));
            format!(
                "{} {}: {}{}",
                icons::platform_label(s.platform),
                short_name(s),
                label,
                st.as_ref().and_then(|s| s.detail.clone()).map(|d| format!(" - {d}")).unwrap_or_default()
            )
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
                let states: Vec<&str> = idxs.iter().map(|&i| state_of(rows[i].1.as_ref())).collect();
                let worst = WORST.iter().find(|w| states.contains(w)).copied().unwrap_or("connecting");
                let keys: Vec<String> = idxs.iter().map(|&i| source_key(&rows[i].0)).collect();
                let names = idxs.iter().map(|&i| short_name(&rows[i].0)).collect::<Vec<_>>().join(", ");
                let tooltip = idxs.iter().map(|&i| dot_tip(i)).chain(std::iter::once("Click to split out, or drag onto a split edge".to_string())).collect::<Vec<_>>().join("\n");
                let multi = idxs.len() > 1;
                let platform = *platform;
                let (color, _) = dot(worst);
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
                        div()
                            .relative()
                            .flex_none()
                            .child(icons::platform_icon(platform, 14., true))
                            .child(
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
                                    .child(div().id(("chip-dot", gi)).size(px(6.)).rounded_full().bg(c(color)).when(pulse(worst), |d| d.opacity(0.75))),
                            )
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
                let state = state_of(st.as_ref());
                let (color, _) = dot(state);
                let dot_el = div().id(("hdr-dot", i)).size(px(8.)).flex_none().rounded_full().bg(c(color));
                let dot_el = if pulse(state) {
                    dot_el
                        .with_animation(("pulse", i), Animation::new(Duration::from_secs(2)).repeat().with_easing(gpui::pulsating_between(0.5, 1.0)), |d, delta| d.opacity(delta))
                        .into_any_element()
                } else {
                    dot_el.into_any_element()
                };
                header = header.child(
                    div()
                        .id(("hdr-src", i))
                        .flex()
                        .min_w_0()
                        .flex_shrink()
                        .items_center()
                        .gap(px(6.))
                        .tooltip(tip(dot_tip(i)))
                        .child(icons::platform_icon(s.platform, 14., true))
                        .child(div().min_w_0().truncate().font_weight(gpui::FontWeight::MEDIUM).child(short_name(s)))
                        .child(dot_el),
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
        header = header.child(modes_view);

        // Mod mode toggle
        let mod_on = leaf.mod_mode;
        let (t1, l1) = (tab_id.clone(), leaf_id.clone());
        header = header.child(
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
        );

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
            let (t2, l2, k2) = (t1.clone(), l1.clone(), key.clone());
            let (t3, l3, k3) = (t1.clone(), l1.clone(), key.clone());
            out.push(MenuEntry::Heading(format!("{}: {}", icons::platform_label(s.platform), short_name(s))));
            out.push(MenuEntry::Item(MenuItem::new("Split out to the right", move |this, _w, cx| this.split_out(&t1, &l1, std::slice::from_ref(&k1), &l1.clone(), Edge::Right, cx))));
            out.push(MenuEntry::Item(MenuItem::new("Split out below", move |this, _w, cx| this.split_out(&t2, &l2, std::slice::from_ref(&k2), &l2.clone(), Edge::Bottom, cx))));
            out.push(MenuEntry::Item(MenuItem::new("Remove from this split", move |this, _w, cx| this.remove_source(&t3, &l3, &k3, cx)).danger()));
        }
        if srcs.len() > 1 {
            let keys: Vec<String> = srcs.iter().map(|s| source_key(s)).collect();
            let (t, l) = (tab_id.to_string(), leaf_id.to_string());
            out.push(MenuEntry::Sep);
            out.push(MenuEntry::Item(MenuItem::new(format!("Split all {} {} out to the right", srcs.len(), icons::platform_label(platform)), move |this, _w, cx| {
                this.split_out(&t, &l, &keys, &l.clone(), Edge::Right, cx)
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
        vec![
            MenuEntry::Item(MenuItem::new("Change channel(s)\u{2026}", move |this, w, cx| this.open_dialog(Dialog::Change { tab_id: t0.clone(), leaf_id: l0.clone() }, w, cx))),
            MenuEntry::Item(MenuItem::new("Add split right", move |this, w, cx| this.open_dialog(Dialog::Split { tab_id: t1.clone(), leaf_id: l1.clone(), below: false }, w, cx))),
            MenuEntry::Item(MenuItem::new("Add split below", move |this, w, cx| this.open_dialog(Dialog::Split { tab_id: t2.clone(), leaf_id: l2.clone(), below: true }, w, cx))),
            MenuEntry::Sep,
            MenuEntry::Item(MenuItem::new("Close split", move |this, _w, cx| this.close_leaf(&t3, &l3, cx)).hint(close_key).danger()),
        ]
    }
}

#[allow(dead_code)]
fn _w(_: &mut Window) {}
