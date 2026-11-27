//! The "Commands" menu of a message: the custom commands the user marked "Show in message menu", run with that
//! message's sender, id and text filled in. Opened from the Commands button of the message's hover tools.
use agu_chat_core::protocol::{CustomCommand, Platform, SourceRef};
use agu_chat_model::input::custom_commands::{expand_for_message, CommandContext};
use gpui::{anchored, deferred, div, point, prelude::*, px, ClickEvent, Context, Corner, ElementId, FontWeight, Pixels, Point, SharedString, Window};

use super::chat_pane::ChatPane;
use super::PxF;
use super::rows::ReplyTarget;
use super::types::RowBody;
use crate::theme::{self, c};

pub(super) struct CmdPick {
    pub target: ReplyTarget,
    pub pos: Point<Pixels>,
}

impl ChatPane {
    /// The commands shown in a message's menu.
    pub(super) fn menu_commands(&self) -> Vec<CustomCommand> {
        self.settings.custom_commands.iter().filter(|c| c.in_menu).cloned().collect()
    }

    pub(super) fn cmd_pick_el(&mut self, window: &Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let pick = self.cmd_pick.as_ref()?;
        let (target, pos) = (pick.target.clone(), pick.pos);
        let cmds = self.menu_commands();
        let above = pos.y.f() > window.viewport_size().height.f() / 2.0;
        let mut menu = div()
            .id("cmd-pick")
            .occlude()
            .w(px(232.))
            .max_h(px(320.))
            .overflow_y_scroll()
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
                this.cmd_pick = None;
                cx.notify();
            }))
            .child(div().px(px(8.)).py(px(4.)).text_size(px(11.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::FAINT)).child("COMMANDS"));
        for (i, cmd) in cmds.into_iter().enumerate() {
            let target = target.clone();
            menu = menu.child(
                div()
                    .id(ElementId::from(SharedString::from(format!("cmd-pick-{i}"))))
                    .w_full()
                    .rounded(px(4.))
                    .px(px(8.))
                    .py(px(5.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .child(SharedString::from(cmd.name.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                        this.cmd_pick = None;
                        this.run_menu_command(&cmd, &target, cx);
                        cx.notify();
                    })),
            );
        }
        let corner = if above { Corner::BottomLeft } else { Corner::TopLeft };
        let at = point(pos.x, if above { pos.y - px(12.) } else { pos.y + px(12.) });
        Some(deferred(anchored().position(at).anchor(corner).snap_to_window_with_margin(gpui::Edges::all(px(4.))).child(menu)).with_priority(4).into_any_element())
    }

    /// Sends the command's text for that message to its chat, the way the message box would.
    fn run_menu_command(&mut self, cmd: &CustomCommand, t: &ReplyTarget, cx: &mut Context<Self>) {
        let Some(source): Option<SourceRef> = self.store.source(&t.sk).map(|s| s.source.clone()) else { return };
        // the sender's login (a Twitch display name can differ from it)
        let login = self
            .store
            .rows()
            .iter()
            .rev()
            .find_map(|r| match &r.body {
                RowBody::Msg(m) if r.sk == t.sk && m.id == t.id && !m.user.login.is_empty() => Some(m.user.login.clone()),
                _ => None,
            })
            .unwrap_or_else(|| t.user.clone());
        let ctx = CommandContext { channel: source.channel.clone(), me: self.co_login(source.platform), user: login, msg_id: t.id.clone(), msg_text: t.text.clone() };
        let lines = expand_for_message(cmd, &ctx);
        if lines.is_empty() {
            self.toast("That command has no text", true, cx);
            return;
        }
        let bridge = self.bridge.clone();
        let (last, pinned) = if source.platform == Platform::Twitch { self.message_ids(&t.sk) } else { (None, None) };
        let tgts = vec![(source, None)];
        cx.spawn(async move |this, cx| {
            let out = super::input::custom_send::run_lines(&bridge, &tgts, &lines, last, pinned).await;
            let _ = this.update(cx, |p, cx| {
                for (tx, warn) in out.toasts {
                    p.toast(tx, warn, cx);
                }
            });
        })
        .detach();
    }
}
