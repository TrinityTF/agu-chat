//! Port of src/renderer/chat/ModActionPopover.tsx: the lightweight confirm anchored at a row's mod button
//! (ban / timeout / warn / delete). Drawn over the pane at the clicked button (below it, or above it in the lower
//! half of the pane); Escape or a click outside closes it. The Ban / Warn mod-mode buttons open it.
use std::collections::BTreeSet;

use gpui::{
    anchored, deferred, div, prelude::*, px, ClickEvent, Context, Corner, ElementId, Entity, FontWeight, KeyDownEvent, MouseDownEvent, Pixels, Point,
    SharedString, WeakEntity, Window,
};
use serde_json::{json, Value};

use super::chat_pane::ChatPane;
use super::mod_actions;
use super::mod_caps::{relay_api, TIMEOUT_LADDER};
use super::services::Services;
use super::types::*;
use super::ui_util::cbtn;
use crate::layout::ui::BtnOpts;
use crate::text_input::{InputStyle, TextInput};
use crate::theme::{self, a, c};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PopKind {
    Ban,
    Timeout,
    Warn,
    Delete,
}

impl PopKind {
    pub fn parse(s: &str) -> PopKind {
        match s {
            "ban" => PopKind::Ban,
            "timeout" => PopKind::Timeout,
            "warn" => PopKind::Warn,
            _ => PopKind::Delete,
        }
    }
}

/// `ModPopoverState`: where it hangs (window coordinates of the button's corner) and what it is about.
pub struct ModPopoverView {
    pane: WeakEntity<ChatPane>,
    sk: String,
    msg: ChatMsg,
    kind: PopKind,
    pos: Point<Pixels>,
    above: bool,
    busy: bool,
    reason: Entity<TextInput>,
    rules: Vec<String>,
    selected: BTreeSet<String>,
    focus: gpui::FocusHandle,
}

impl ModPopoverView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, pane: WeakEntity<ChatPane>, sk: String, msg: ChatMsg, kind: PopKind, pos: Point<Pixels>, above: bool) -> Self {
        let placeholder = if kind == PopKind::Warn { "Custom reason" } else { "Reason (optional)" };
        let reason = cx.new(|cx| {
            let mut t = TextInput::new(window, cx, placeholder, InputStyle::Chat);
            t.set_max_chars(Some(500));
            t
        });
        let focus = cx.focus_handle();
        let mut v = ModPopoverView { pane, sk, msg, kind, pos, above, busy: false, reason, rules: vec![], selected: BTreeSet::new(), focus };
        // `autoFocus` on the ban reason; the popover itself takes focus otherwise so Escape reaches it
        if kind == PopKind::Ban {
            v.reason.read(cx).focus(window);
        } else {
            window.focus(&v.focus);
        }
        cx.observe(&v.reason, |_, _, cx| cx.notify()).detach();
        if kind == PopKind::Warn {
            // a beat later: the pane is mid-render while it builds us and can't be read yet
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |v, cx| v.load_rules(cx));
            })
            .detach();
        }
        v
    }

    fn load_rules(&mut self, cx: &mut Context<Self>) {
        let Some(src) = self.pane.upgrade().and_then(|p| p.read(cx).ref_of(&self.sk)) else { return };
        let bridge = cx.global::<Services>().bridge.clone();
        cx.spawn(async move |this, cx| {
            let r = relay_api(&bridge, &src, "/api/chat/rules", None, None).await;
            let rules: Vec<String> = if r.ok { r.body.get("rules").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect()).unwrap_or_default() } else { vec![] };
            let _ = this.update(cx, |v, cx| {
                v.rules = rules;
                cx.notify();
            });
        })
        .detach();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let _ = self.pane.update(cx, |p, cx| {
            p.mod_pop = None;
            cx.notify();
        });
    }

    /// `run`: the mod call; on success a deleted message is marked and the popover goes.
    fn run(&mut self, body: Value, ok: &str, cx: &mut Context<Self>) {
        let Some(src) = self.pane.upgrade().and_then(|p| p.read(cx).ref_of(&self.sk)) else { return };
        self.busy = true;
        cx.notify();
        let bridge = cx.global::<Services>().bridge.clone();
        let ok = ok.to_string();
        let is_delete = body.get("action").and_then(|a| a.as_str()) == Some("delete");
        cx.spawn(async move |this, cx| {
            let (done, t) = mod_actions::twitch_action(&bridge, &src, body, Some(ok)).await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                let (sk, id) = (v.sk.clone(), v.msg.id.clone());
                let _ = v.pane.update(cx, |p, cx| {
                    p.show_toast(t, cx);
                    if done && is_delete {
                        p.mark_deleted(&sk, &id, cx);
                    }
                });
                if done {
                    v.close(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for ModPopoverView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.msg.user.name.clone();
        let title = match self.kind {
            PopKind::Ban => format!("Ban {name}?"),
            PopKind::Timeout => format!("Timeout {name}"),
            PopKind::Warn => format!("Warn {name}"),
            PopKind::Delete => "Delete this message?".to_string(),
        };
        let uid = self.msg.user.id.clone();
        let busy = self.busy;
        let recent: Vec<String> = if matches!(self.kind, PopKind::Ban | PopKind::Timeout) {
            self.pane
                .upgrade()
                .map(|p| {
                    let rows = p.read(cx).rows_of_user(&self.sk, &uid);
                    let n = rows.len();
                    rows.into_iter().skip(n.saturating_sub(5)).collect()
                })
                .unwrap_or_default()
        } else {
            vec![]
        };

        let mut p = div()
            .id("mod-popover")
            .occlude()
            .w(px(256.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(10.))
            .shadow_2xl()
            .text_size(px(12.))
            .line_height(px(16.))
            .text_color(c(theme::TEXT))
            .font_family(theme::FONT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _w, cx| {
                if ev.keystroke.key == "escape" {
                    this.close(cx);
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _w, cx| this.close(cx)))
            .child(div().font_weight(FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child(SharedString::from(title)));

        if !recent.is_empty() {
            let mut list = div().id("pop-recent").max_h(px(80.)).overflow_y_scroll().flex().flex_col().gap(px(2.)).text_color(c(theme::FAINT));
            for t in recent {
                list = list.child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(t)));
            }
            p = p.child(list);
        }

        if self.kind == PopKind::Timeout {
            let mut row = div().flex().flex_wrap().gap(px(4.));
            for (label, sec) in TIMEOUT_LADDER {
                let u = uid.clone();
                row = row.child(
                    div()
                        .id(ElementId::from(SharedString::from(format!("pt-{label}"))))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(c(theme::LINE))
                        .px(px(6.))
                        .py(px(2.))
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(c(theme::MUTED))
                        .when(busy, |d| d.opacity(0.4))
                        .when(!busy, |d| d.hover(|s| s.border_color(a(theme::AMBER_500, 0.5)).text_color(c(theme::AMBER_300))))
                        .child(label)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            if !this.busy {
                                this.run(json!({"action": "timeout", "userId": u, "duration": sec}), &format!("timed out {label}"), cx);
                            }
                        })),
                );
            }
            p = p.child(row);
        }

        if self.kind == PopKind::Warn {
            let mut w = div().flex().flex_col().gap(px(8.));
            if !self.rules.is_empty() {
                let mut list = div().id("pop-rules").max_h(px(96.)).overflow_y_scroll().flex().flex_col().gap(px(4.));
                for (i, rule) in self.rules.iter().enumerate() {
                    let on = self.selected.contains(rule);
                    let rr = rule.clone();
                    list = list.child(
                        div()
                            .id(("prule", i))
                            .flex()
                            .items_start()
                            .gap(px(6.))
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
                                    .when(on, |d| d.bg(c(theme::ACCENT)).text_color(c(theme::WHITE)).child(crate::icons::icon(crate::icons::CHECK, 10.))),
                            )
                            .child(div().min_w_0().flex_1().child(SharedString::from(rule.clone())))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                if !this.selected.remove(&rr) {
                                    this.selected.insert(rr.clone());
                                }
                                cx.notify();
                            })),
                    );
                }
                w = w.child(list);
            }
            p = p.child(w.child(self.reason.clone()));
        }
        if self.kind == PopKind::Ban {
            p = p.child(self.reason.clone());
        }

        let reason_empty = self.reason.read(cx).text().trim().is_empty();
        let mut buttons = div().flex().gap(px(6.));
        if self.kind != PopKind::Timeout {
            let kind = self.kind;
            let disabled = busy || (kind == PopKind::Warn && self.selected.is_empty() && reason_empty);
            let (u, mid) = (uid.clone(), self.msg.id.clone());
            buttons = buttons.child(cbtn("pop-ok", "Confirm", BtnOpts { danger: true, disabled, ..Default::default() }).on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                let reason = this.reason.read(cx).text().trim().to_string();
                if this.busy || (kind == PopKind::Warn && this.selected.is_empty() && reason.is_empty()) {
                    return;
                }
                match kind {
                    PopKind::Ban => {
                        let mut body = json!({"action": "ban", "userId": u});
                        if !reason.is_empty() {
                            body["reason"] = json!(reason);
                        }
                        this.run(body, "banned", cx);
                    }
                    PopKind::Warn => {
                        let mut parts: Vec<String> = this.selected.iter().cloned().collect();
                        parts.push(reason);
                        let text: String = parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("; ").chars().take(500).collect();
                        this.run(json!({"action": "warn", "userId": u, "reason": text}), "warned", cx);
                    }
                    _ => this.run(json!({"action": "delete", "messageId": mid}), "deleted", cx),
                }
            })));
        }
        buttons = buttons.child(cbtn("pop-cancel", "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.close(cx))));
        p = p.child(buttons);

        // below the button, or above it (`translateY(-100%)`) in the lower half of the pane
        let corner = if self.above { Corner::BottomLeft } else { Corner::TopLeft };
        deferred(anchored().position(self.pos).anchor(corner).snap_to_window_with_margin(gpui::Edges::all(px(4.))).child(p)).with_priority(4)
    }
}
