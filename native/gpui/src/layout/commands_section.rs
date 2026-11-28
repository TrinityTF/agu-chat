//! Settings > Commands: the user's custom commands (a short name that sends saved text), with import from
//! Chatterino or from a commands.json copied from another computer. The logic is in
//! `agu_chat_model::input::custom_commands`; this is the page.
use agu_chat_core::protocol::CustomCommand;
use agu_chat_model::input::custom_commands::{chatterino_commands_file, COMMANDS_HELP, clean_name, command_error, merge, parse_chatterino};
use gpui::{div, prelude::*, px, AnyElement, ClickEvent, Context, PathPromptOptions, Window};
use serde_json::json;

use super::settings_dialog::SettingsDialog;
use super::settings_widgets::{eid, txt};
use super::ui::{btn, toggle, BtnOpts};
use crate::icons;
use crate::text_input::InputStyle;
use crate::theme::{self, a, c};

const HELP_END: &str = " Shift+Enter starts a new line.";

impl SettingsDialog {
    fn set_status(&mut self, text: impl Into<String>, warn: bool, cx: &mut Context<Self>) {
        self.cmd_status = Some((text.into(), warn));
        cx.notify();
    }

    /// Adds the command typed in the "new" row.
    pub(super) fn add_command(&mut self, cx: &mut Context<Self>) {
        let name = self.text_of("cmd.new.name", cx);
        let text = self.text_of("cmd.new.text", cx);
        let list = self.settings.custom_commands.clone();
        if let Some(e) = command_error(&name, &text, list.iter().map(|c| c.name.as_str())) {
            self.err.insert("cmd.new".into(), e.into());
            cx.notify();
            return;
        }
        self.err.remove("cmd.new");
        let mut v = list;
        v.push(CustomCommand { name: clean_name(&name), text, in_menu: self.cmd_new_menu });
        self.set(json!({ "customCommands": v }), cx);
        self.cmd_new_menu = false;
        self.cmd_status = None;
        self.set_text_of("cmd.new.name", "", cx);
        self.set_text_of("cmd.new.text", "", cx);
    }

    /// Saves the edited name or text of row `i` (on blur). A bad value shows its reason and is not saved.
    pub(super) fn commit_command(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut v = self.settings.custom_commands.clone();
        let Some(cur) = v.get(i).cloned() else { return };
        let name = self.text_of(&format!("cmd.{i}.name"), cx);
        let text = self.text_of(&format!("cmd.{i}.text"), cx);
        let key = format!("cmd.{i}");
        let others = v.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, c)| c.name.clone()).collect::<Vec<_>>();
        if let Some(e) = command_error(&name, &text, others.iter().map(|s| s.as_str())) {
            self.err.insert(key, e.into());
            cx.notify();
            return;
        }
        self.err.remove(&key);
        let next = CustomCommand { name: clean_name(&name), text, in_menu: cur.in_menu };
        if next == cur {
            cx.notify();
            return;
        }
        v[i] = next;
        self.set(json!({ "customCommands": v }), cx);
    }

    fn toggle_command_menu(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut v = self.settings.custom_commands.clone();
        if let Some(c) = v.get_mut(i) {
            c.in_menu = !c.in_menu;
            self.set(json!({ "customCommands": v }), cx);
        }
    }

    fn remove_command(&mut self, i: usize, cx: &mut Context<Self>) {
        let mut v = self.settings.custom_commands.clone();
        if i < v.len() {
            v.remove(i);
            self.err.remove(&format!("cmd.{i}"));
            self.set(json!({ "customCommands": v }), cx);
        }
    }

    /// Merges the commands of a Chatterino-format file's text into the list; the status says how many were new.
    fn import_commands(&mut self, json_text: &str, source: &str, cx: &mut Context<Self>) {
        match parse_chatterino(json_text) {
            Err(e) => self.set_status(e, true, cx),
            Ok(incoming) => {
                let (merged, added) = merge(&self.settings.custom_commands, incoming);
                if added == 0 {
                    self.set_status("No new commands to import", false, cx);
                } else {
                    self.set(json!({ "customCommands": merged }), cx);
                    let s = if source.is_empty() { String::new() } else { format!(" from {source}") };
                    self.set_status(format!("Imported {added} command{}{s}", if added == 1 { "" } else { "s" }), false, cx);
                }
            }
        }
    }

    fn import_chatterino(&mut self, cx: &mut Context<Self>) {
        let Some(path) = chatterino_commands_file() else { return };
        match std::fs::read_to_string(&path) {
            Ok(t) => self.import_commands(&t, "Chatterino", cx),
            Err(_) => self.set_status("Could not read the Chatterino commands file", true, cx),
        }
    }

    fn import_from_file(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Choose a commands.json".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let text = std::fs::read_to_string(&path);
            let _ = this.update(cx, |this, cx| match text {
                Ok(t) => this.import_commands(&t, "", cx),
                Err(_) => this.set_status("Could not read that file", true, cx),
            });
        })
        .detach();
    }

    fn command_editor(&mut self, i: usize, last: bool, cmd: &CustomCommand, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let name = self.input(&format!("cmd.{i}.name"), "/name", InputStyle::Field, &cmd.name, true, false, window, cx);
        let text = self.input(&format!("cmd.{i}.text"), "Text to send", InputStyle::Area, &cmd.text, false, false, window, cx);
        let err = self.err.get(&format!("cmd.{i}")).cloned();
        let in_menu = cmd.in_menu;
        div()
            .py(px(6.))
            .when(!last, |d| d.border_b_1().border_color(a(theme::LINE, 0.6)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().w(px(140.)).flex_none().child(name))
                    .child(div().flex_1())
                    .child(txt(11.).text_color(c(theme::MUTED)).child("Show in message menu"))
                    .child(toggle(eid(format!("cmd-menu-{i}")), in_menu, false).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_command_menu(i, cx))))
                    .child(
                        self.icon_btn(format!("cmd-rm-{i}"), icons::TRASH, 14., true, false)
                            .tooltip(super::ui::tip("Delete"))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.remove_command(i, cx))),
                    ),
            )
            .child(div().mt(px(4.)).child(text))
            .children(err.map(|e| txt(11.).mt(px(4.)).text_color(c(theme::RED_400)).child(e)))
            .into_any_element()
    }

    pub(super) fn render_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let list = self.settings.custom_commands.clone();
        let name = self.input("cmd.new.name", "/name", InputStyle::Field, "", true, false, window, cx);
        let text = self.input("cmd.new.text", "Text to send", InputStyle::Area, "", false, false, window, cx);
        let new_menu = self.cmd_new_menu;
        let new_err = self.err.get("cmd.new").cloned();
        let add = btn(eid("cmd-add"), "Add", BtnOpts { primary: true, small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_command(cx)));
        let mut imports = div().mb(px(8.)).flex().flex_wrap().items_center().gap(px(6.));
        if chatterino_commands_file().is_some() {
            imports = imports.child(btn(eid("cmd-imp-ct"), "Import from Chatterino", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_chatterino(cx))));
        }
        imports = imports.child(btn(eid("cmd-imp-file"), "Import from file...", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.import_from_file(cx))));
        if let Some((s, warn)) = self.cmd_status.clone() {
            imports = imports.child(txt(11.).text_color(c(if warn { theme::RED_400 } else { theme::MUTED })).child(s));
        }
        let n = list.len();
        let rows: Vec<AnyElement> = list.iter().enumerate().map(|(i, cmd)| self.command_editor(i, i + 1 == n, cmd, window, cx)).collect();
        let card = || div().mb(px(12.)).rounded(px(4.)).border_1().border_color(c(theme::LINE)).bg(c(theme::RAISED)).p(px(12.));
        let sv = serde_json::to_value(&self.settings).unwrap_or_default();
        let menu_button = self.bool_row(&sv, "commandsButton", "Commands button on messages", Some("The ⋯ button next to copy, pin and reply, with the commands marked In menu"), false, true, cx);
        div()
            .child(txt(13.).mb(px(8.)).mt(px(4.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child("Custom commands"))
            .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child(format!("{COMMANDS_HELP}{HELP_END}")))
            .child(imports)
            .child(div().mb(px(12.)).child(menu_button))
            .child(
                card()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(div().w(px(140.)).flex_none().child(name))
                            .child(div().flex_1())
                            .child(txt(11.).text_color(c(theme::MUTED)).child("Show in message menu"))
                            .child(toggle(eid("cmd-menu-new"), new_menu, false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.cmd_new_menu = !this.cmd_new_menu;
                                cx.notify();
                            })))
                            .child(add),
                    )
                    .child(div().mt(px(4.)).child(text))
                    .children(new_err.map(|e| txt(11.).mt(px(4.)).text_color(c(theme::RED_400)).child(e))),
            )
            .child(card().child(if rows.is_empty() { div().text_color(c(theme::FAINT)).child("No custom commands.").into_any_element() } else { div().children(rows).into_any_element() }))
            .into_any_element()
    }
}
