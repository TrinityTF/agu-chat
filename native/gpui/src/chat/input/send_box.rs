//! Port of src/renderer/chat/input/SendBox.tsx (+ EmotePicker.tsx): send, slash commands, colon / Tab / "@" / "/"
//! autocomplete, the emote picker, the reply bar, the /clear and /raid confirm and the target selector, over a
//! textarea (`TextInput` in Area mode). One box per split; sends go through the bridge like `relayApi`.
//!
//! The pane builds `SendSource`s each frame and hands them over with `set_props`; the box owns its text, history
//! and dropdown state. Toasts and "clear the reply" go back to the pane.
use std::collections::HashMap;
use std::sync::Arc;

use agu_chat_core::protocol::{Platform, SourceRef};
use gpui::{
    div, point, prelude::*, px, relative, AnyElement, ClickEvent, Context, ElementId, Entity, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    ScrollHandle, SharedString, WeakEntity, Window,
};
use serde_json::json;

use super::autocomplete::*;
use super::inventory::{merge_inventories, Inventory, PickerItem, PickerProvider, PickerSection};
use super::slash::{confirmed_command, is_me, parse_command, parse_user_arg, run_slash_command, split_args, Confirmed};
use crate::chat::chat_pane::ChatPane;
use crate::chat::mod_caps::{relay_api, ModCaps};
use crate::chat::pane_render::ImageBox;
use crate::chat::rows::ReplyTarget;
use crate::chat::services::Services;
use crate::icons::{self, icon};
use crate::chat::ui_util::cbtn as btn;
use crate::layout::tree::{is_whisper, short_name};
use crate::layout::ui::BtnOpts;
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

const SENDABLE: [Platform; 4] = [Platform::Twitch, Platform::Rumble, Platform::Kick, Platform::X];
const ALL: &str = "*";
const HISTORY_MAX: usize = 100;

/// `SendSource`
#[derive(Clone)]
pub struct SendSource {
    pub sk: String,
    pub source: SourceRef,
    /// The conn frame says this account can post here.
    pub can_send: bool,
    pub read_only_reason: Option<String>,
    /// Connection still being established (no conn frame yet, or "connecting").
    pub pending: bool,
    pub caps: ModCaps,
    pub inventory: Arc<Inventory>,
}

impl PartialEq for SendSource {
    fn eq(&self, o: &Self) -> bool {
        self.sk == o.sk
            && self.can_send == o.can_send
            && self.read_only_reason == o.read_only_reason
            && self.pending == o.pending
            && self.caps == o.caps
            && Arc::ptr_eq(&self.inventory, &o.inventory)
    }
}

pub enum SendBoxEvent {}

pub struct SendBox {
    pane: WeakEntity<ChatPane>,
    /// The autocomplete / mention lists' scroll: the selected row is kept in view as Tab / arrows move it.
    auto_scroll: gpui::ScrollHandle,
    mention_scroll: gpui::ScrollHandle,
    /// The selection the lists were last scrolled to (start of the word, row): only a move scrolls, so the mouse
    /// wheel still works on the list.
    auto_at: Option<(usize, usize)>,
    mention_at: Option<(usize, usize)>,
    pub input: Entity<TextInput>,
    picker_q: Entity<TextInput>,
    sources: Vec<SendSource>,
    focused: bool,
    reply: Option<ReplyTarget>,
    sending: bool,
    picker_open: bool,
    picker_btn_hover: bool,
    picker_scroll: ScrollHandle,
    auto: Option<AutoState>,
    mentions: Mentions,
    confirm: Option<Confirmed>,
    target: String,
    target_open: bool,
    history: Vec<String>,
    hist_idx: Option<usize>,
    draft: String,
    skip_changed: u32,
    aids_done: bool,
    prev_reply_id: Option<String>,
    prev_focused: bool,
    _subs: Vec<gpui::Subscription>,
}

/// `postText`: the error to show, or None when it went out.
pub async fn post_text(bridge: &Arc<dyn agu_chat_core::protocol::AguBridge>, source: &SourceRef, path: &str, text: &str, reply_to: Option<&str>) -> Option<String> {
    let mut body = json!({"text": text});
    if let Some(r) = reply_to {
        body["replyTo"] = json!(r);
    }
    let r = relay_api(bridge, source, path, Some("POST"), Some(body)).await;
    if r.ok {
        return None;
    }
    let msg = match &r.body {
        serde_json::Value::Object(o) => o.get("error").map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.to_string())).unwrap_or_else(|| r.text()),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    };
    Some(if r.status == 403 && msg == "relogin" {
        "Log in again to grant chat permissions".into()
    } else if msg.is_empty() {
        "send failed".into()
    } else {
        msg
    })
}

fn send_path(p: Platform) -> Option<&'static str> {
    match p {
        Platform::Twitch => Some("/api/chat/send"),
        Platform::Rumble => Some("/api/rumble/send"),
        Platform::Kick => Some("/api/kick/send"),
        Platform::X => Some("/api/x/send"),
        _ => None,
    }
}

impl SendBox {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, pane: WeakEntity<ChatPane>) -> Self {
        let input = cx.new(|cx| {
            let mut i = TextInput::new(window, cx, "", InputStyle::Area);
            i.set_max_chars(Some(500));
            i
        });
        let picker_q = cx.new(|cx| TextInput::new(window, cx, "Search emotes...", InputStyle::Chat));
        let mut subs = Vec::new();
        subs.push(cx.subscribe_in(&input, window, |this, _i, ev: &TextInputEvent, window, cx| match ev {
            TextInputEvent::Changed => this.handle_change(cx),
            TextInputEvent::Enter => this.send(window, cx),
            TextInputEvent::Escape => {
                if this.reply.is_some() {
                    this.clear_reply(cx);
                }
            }
            _ => {}
        }));
        subs.push(cx.observe(&picker_q, |_, _, cx| cx.notify()));
        SendBox {
            pane,
            input,
            picker_q,
            sources: vec![],
            focused: false,
            reply: None,
            sending: false,
            picker_open: false,
            picker_btn_hover: false,
            picker_scroll: ScrollHandle::new(),
            auto: None,
            mentions: Mentions::default(),
            confirm: None,
            target: String::new(),
            target_open: false,
            history: vec![],
            hist_idx: None,
            draft: String::new(),
            skip_changed: 0,
            aids_done: false,
            prev_reply_id: None,
            auto_scroll: gpui::ScrollHandle::new(),
            mention_scroll: gpui::ScrollHandle::new(),
            auto_at: None,
            mention_at: None,
            prev_focused: false,
            _subs: subs,
        }
    }

    // ---- props ------------------------------------------------------------------------------------------

    pub fn set_props(&mut self, sources: Vec<SendSource>, focused: bool, reply: Option<ReplyTarget>, window: &mut Window, cx: &mut Context<Self>) {
        let reply_id = reply.as_ref().map(|r| r.id.clone());
        let changed = sources != self.sources || reply.as_ref().map(|r| (&r.id, &r.user, &r.text)) != self.reply.as_ref().map(|r| (&r.id, &r.user, &r.text));
        // Picking Reply puts the caret in the box.
        if reply_id.is_some() && reply_id != self.prev_reply_id {
            self.input.read(cx).focus(window);
        }
        if focused && !self.prev_focused {
            self.input.read(cx).focus(window);
        }
        self.prev_reply_id = reply_id;
        self.prev_focused = focused;
        self.focused = focused;
        if changed {
            self.sources = sources;
            self.reply = reply;
            // Keep the selected target valid as connections come and go.
            let eff = self.effective();
            if eff != self.target && self.reply.is_none() {
                self.target = eff;
            }
            let (ph, disabled) = (self.placeholder(), self.sendable().is_empty());
            self.input.update(cx, |i, cx| {
                i.set_placeholder(&ph, cx);
                i.set_disabled(disabled, cx);
            });
            cx.notify();
        }
    }

    fn sendable(&self) -> Vec<&SendSource> {
        self.sources.iter().filter(|s| SENDABLE.contains(&s.source.platform) && s.can_send).collect()
    }

    /// What "All chats" reaches: a whisper goes to one person, never along with a broadcast.
    fn broadcast(&self) -> Vec<&SendSource> {
        self.sendable().into_iter().filter(|s| !is_whisper(&s.source)).collect()
    }

    /// A reply pins the target: its id only means something in the chat it came from.
    fn effective(&self) -> String {
        let sendable = self.sendable();
        if let Some(r) = &self.reply {
            if let Some(s) = self.sources.iter().find(|s| s.sk == r.sk).filter(|s| s.can_send) {
                return s.sk.clone();
            }
        }
        if self.target == ALL && self.broadcast().len() > 1 {
            return ALL.into();
        }
        sendable.iter().find(|s| s.sk == self.target).or(sendable.first()).map(|s| s.sk.clone()).unwrap_or_default()
    }

    fn targets(&self) -> Vec<&SendSource> {
        let eff = self.effective();
        let sendable = self.sendable();
        if eff == ALL {
            self.broadcast()
        } else {
            sendable.into_iter().filter(|s| s.sk == eff).collect()
        }
    }

    fn single(&self) -> Option<&SendSource> {
        let t = self.targets();
        if t.len() == 1 {
            Some(t[0])
        } else {
            None
        }
    }

    fn inventory(&self) -> Inventory {
        let t = self.targets();
        if t.is_empty() {
            Inventory::default()
        } else {
            merge_inventories(&t.iter().map(|s| (*s.inventory).clone()).collect::<Vec<_>>())
        }
    }

    fn placeholder(&self) -> String {
        let sendable = self.sendable();
        if sendable.is_empty() {
            return self
                .sources
                .iter()
                .find_map(|s| s.read_only_reason.clone().filter(|r| !r.is_empty()))
                .unwrap_or_else(|| if self.sources.iter().any(|s| s.pending) { "Connecting...".into() } else { "Chat is read-only".into() });
        }
        if let Some(r) = &self.reply {
            return format!("Reply to @{}", r.user);
        }
        if self.effective() == ALL {
            return format!("Send to {}", sendable.iter().map(|s| icons::platform_label(s.source.platform)).collect::<Vec<_>>().join(" + "));
        }
        match self.single() {
            Some(s) if is_whisper(&s.source) => format!("Whisper {}", short_name(&s.source)),
            Some(s) => format!("Message {}{}", if s.source.platform == Platform::Twitch { "#" } else { "" }, s.source.channel),
            None => "Message ".into(),
        }
    }

    // ---- pane access ------------------------------------------------------------------------------------

    fn toast(&self, text: impl Into<String>, warn: bool, cx: &mut Context<Self>) {
        let text = text.into();
        let _ = self.pane.update(cx, |p, cx| p.toast(text, warn, cx));
    }

    fn clear_reply(&mut self, cx: &mut Context<Self>) {
        let _ = self.pane.update(cx, |p, cx| {
            p.reply_target = None;
            cx.notify();
        });
    }

    fn chatters(&self, cx: &gpui::App) -> Vec<String> {
        let sks: Vec<String> = self.targets().iter().map(|s| s.sk.clone()).collect();
        self.pane.upgrade().map(|p| p.read(cx).chatters(&sks)).unwrap_or_default()
    }

    // ---- state helpers ----------------------------------------------------------------------------------

    fn text(&self, cx: &gpui::App) -> String {
        self.input.read(cx).text()
    }

    /// Write the box' text without the change handler re-opening the dropdowns (React state, not onChange).
    fn put(&mut self, text: &str, caret: usize, cx: &mut Context<Self>) {
        self.skip_changed += 1;
        self.input.update(cx, |i, cx| i.replace_all(text, caret, cx));
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.input.update(cx, |i, cx| i.set_text("", cx));
        self.auto = None;
        self.mentions.close();
        self.clear_reply(cx);
        cx.notify();
    }

    fn remember(&mut self, t: &str) {
        if self.history.last().map(|l| l.as_str()) != Some(t) {
            self.history.push(t.to_string());
        }
        if self.history.len() > HISTORY_MAX {
            let n = self.history.len() - HISTORY_MAX;
            self.history.drain(..n);
        }
        self.hist_idx = None;
        self.draft.clear();
    }

    fn insert_emote(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.skip_changed += 1;
        let ins = format!(" {name} ");
        self.input.update(cx, |i, cx| i.insert_at_caret(&ins, window, cx));
        self.input.read(cx).focus(window);
    }

    // ---- typing -----------------------------------------------------------------------------------------

    fn handle_change(&mut self, cx: &mut Context<Self>) {
        if self.skip_changed > 0 {
            self.skip_changed -= 1;
            cx.notify();
            return;
        }
        let val = self.text(cx);
        let pos = self.input.read(cx).caret();
        self.hist_idx = None;
        let inventory = self.inventory();
        let caps = self.single().filter(|s| s.source.platform == Platform::Twitch).map(|s| s.caps.clone());
        // A whisper is plain text: "/" starts nothing there.
        let slash_ok = self.single().map(|s| s.source.platform == Platform::Twitch && !is_whisper(&s.source)).unwrap_or(false);
        let mut mentions = std::mem::take(&mut self.mentions);
        let chat_cache: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
        let app: &gpui::App = cx;
        let names = || chat_cache.get_or_init(|| self.chatters(app)).clone();
        mentions.refresh(&val, pos, &names);
        let ctx = ChangeCtx { inventory: &inventory.inventory, slash_ok, twitch_caps: caps.as_ref(), names: &names };
        let auto = auto_for_change(&val, pos, self.auto.as_ref(), &ctx);
        self.mentions = mentions;
        self.auto = auto;
        cx.notify();
    }

    fn commit_auto_pick(&mut self, choice: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(auto) = self.auto.take() else { return };
        let text = self.text(cx);
        let pick = choice.and_then(|i| auto.matches.get(i).cloned());
        if let Some((t, caret)) = commit_auto(&text, &auto, pick.as_ref()) {
            self.put(&t, caret, cx);
        }
        self.input.read(cx).focus(window);
        cx.notify();
    }

    fn apply_mention(&mut self, choice: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if let Some((t, caret)) = self.mentions.apply(&text, choice.as_deref()) {
            self.put(&t, caret, cx);
        }
        self.input.read(cx).focus(window);
        cx.notify();
    }

    /// handleKeyDown, for the keys the dropdowns and history own (runs before the text field sees them).
    fn on_capture_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let key = ks.key.as_str();
        let m = ks.modifiers;
        if m.control || m.alt {
            return;
        }
        // While the mention list is open it owns Tab/arrows/Enter/Escape.
        if self.mentions.state.is_some() {
            match key {
                "tab" | "enter" => self.apply_mention(None, window, cx),
                "up" => self.mentions.mv(-1),
                "down" => self.mentions.mv(1),
                "escape" => self.mentions.close(),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(auto) = self.auto.clone() {
            match key {
                "tab" | "enter" => {
                    self.commit_auto_pick(None, window, cx);
                }
                "up" | "down" => {
                    let n = auto.matches.len();
                    let idx = (auto.idx + n + if key == "up" { n - 1 } else { 1 }) % n;
                    if auto.mode == AutoMode::Cmd {
                        if let Some(a) = &mut self.auto {
                            a.idx = idx;
                        }
                    } else {
                        let text = self.text(cx);
                        if let Some((t, pos, next)) = preview_auto(&text, &auto, idx) {
                            self.put(&t, pos, cx);
                            self.auto = Some(next);
                        }
                    }
                }
                "escape" => self.auto = None,
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match key {
            "tab" => {
                let text = self.text(cx);
                let pos = self.input.read(cx).caret();
                let inventory = self.inventory();
                let chat_cache: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
                let app: &gpui::App = cx;
                let names = || chat_cache.get_or_init(|| self.chatters(app)).clone();
                let ctx = ChangeCtx { inventory: &inventory.inventory, slash_ok: false, twitch_caps: None, names: &names };
                let auto = auto_for_tab(&text, pos, &ctx);
                if auto.is_some() {
                    self.auto = auto;
                }
                cx.stop_propagation();
                cx.notify();
            }
            // Up/Down recall: only from the first / last line so multi-line editing still moves the caret.
            "up" | "down" if !m.shift => {
                let text = self.text(cx);
                let pos = self.input.read(cx).caret();
                if key == "up" && !text[..pos].contains('\n') && !self.history.is_empty() {
                    match self.hist_idx {
                        None => {
                            self.draft = text;
                            self.hist_idx = Some(self.history.len() - 1);
                        }
                        Some(i) if i > 0 => self.hist_idx = Some(i - 1),
                        _ => {}
                    }
                    let v = self.history[self.hist_idx.unwrap()].clone();
                    self.put(&v, v.len(), cx);
                    cx.stop_propagation();
                } else if key == "down" && self.hist_idx.is_some() && !text[pos..].contains('\n') {
                    let i = self.hist_idx.unwrap();
                    self.hist_idx = if i + 1 < self.history.len() { Some(i + 1) } else { None };
                    let v = self.hist_idx.map(|i| self.history[i].clone()).unwrap_or_else(|| self.draft.clone());
                    self.put(&v, v.len(), cx);
                    cx.stop_propagation();
                }
                cx.notify();
            }
            _ => {}
        }
    }

    // ---- sending ----------------------------------------------------------------------------------------

    fn send(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let raw = self.text(cx);
        let t = raw.trim().to_string();
        let targets: Vec<SendSource> = self.targets().into_iter().cloned().collect();
        if t.is_empty() || self.sending || targets.is_empty() {
            return;
        }
        let body: String = t.chars().take(500).collect();
        let bridge = cx.global::<Services>().bridge.clone();
        let single = if targets.len() == 1 { Some(targets[0].clone()) } else { None };

        // Slash commands (everything but "/me") run against one Twitch source.
        if t.starts_with('/') && !is_me(&t) {
            if let Some(s) = single.as_ref().filter(|s| s.source.platform == Platform::Twitch && !is_whisper(&s.source)) {
                let (cmd, rest) = parse_command(&t).unwrap_or_default();
                if cmd == "clear" {
                    self.confirm = Some(Confirmed::Clear);
                    cx.notify();
                    return;
                }
                if cmd == "raid" {
                    let login = parse_user_arg(split_args(&rest).first().copied().unwrap_or(""));
                    if login.is_empty() {
                        self.toast("Usage: /raid <user>", true, cx);
                    } else {
                        self.confirm = Some(Confirmed::Raid { login });
                        cx.notify();
                    }
                    return;
                }
                self.sending = true;
                cx.notify();
                let (src, sk) = (s.source.clone(), s.sk.clone());
                let (last, pinned) = self.pane.upgrade().map(|p| p.read(cx).message_ids(&sk)).unwrap_or((None, None));
                cx.spawn(async move |this, cx| {
                    let res = run_slash_command(&bridge, &src, &t, last, pinned).await;
                    let _ = this.update(cx, |sb, cx| {
                        sb.sending = false;
                        match res {
                            Ok((ok, toast)) => {
                                if let Some((tx, warn)) = toast {
                                    sb.toast(tx, warn, cx);
                                }
                                if ok {
                                    let sent = sb.text(cx).trim().to_string();
                                    sb.remember(&sent);
                                    sb.clear(cx);
                                }
                            }
                            Err(e) => sb.toast(e, true, cx),
                        }
                        cx.notify();
                    });
                })
                .detach();
                return;
            }
            if self.effective() == ALL {
                self.toast("Commands run on a single Twitch chat: pick it in the target menu", true, cx);
                return;
            }
        }

        self.sending = true;
        cx.notify();
        let reply = self.reply.clone();
        cx.spawn(async move |this, cx| {
            // Sequential, not parallel: rate limits are per platform, and one failing must not lose the message
            // that already reached another.
            let mut failures: Vec<String> = Vec::new();
            for s in &targets {
                let Some(path) = send_path(s.source.platform) else { continue };
                // A reply id only means something in the chat it came from.
                let reply_to = reply.as_ref().filter(|r| r.sk == s.sk).map(|r| r.id.clone());
                if let Some(err) = post_text(&bridge, &s.source, path, &body, reply_to.as_deref()).await {
                    failures.push(if targets.len() > 1 { format!("{}: {err}", icons::platform_label(s.source.platform)) } else { err });
                }
            }
            let _ = this.update(cx, |sb, cx| {
                sb.sending = false;
                if !failures.is_empty() {
                    sb.toast(failures.join(" - "), true, cx);
                } else {
                    let sent = sb.text(cx).trim().to_string();
                    sb.remember(&sent);
                    sb.clear(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn execute_confirm(&mut self, cx: &mut Context<Self>) {
        let (Some(c), Some(s)) = (self.confirm.clone(), self.single().cloned()) else { return };
        self.sending = true;
        let bridge = cx.global::<Services>().bridge.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let res = confirmed_command(&bridge, &s.source, &c).await;
            let _ = this.update(cx, |sb, cx| {
                sb.sending = false;
                sb.confirm = None;
                match res {
                    Ok(t) => {
                        if let Some((tx, warn)) = t {
                            sb.toast(tx, warn, cx);
                        }
                        sb.clear(cx);
                    }
                    Err(e) => sb.toast(e, true, cx),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- test aids --------------------------------------------------------------------------------------

    /// `AGU_SHOT=emotepicker|autocomplete|colon|mention|confirm` (+ `AGU_SEND_TEXT`): open a state without input.
    fn aids(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.aids_done || self.sendable().is_empty() {
            return;
        }
        let Ok(shot) = std::env::var("AGU_SHOT") else { return };
        if !matches!(shot.as_str(), "emotepicker" | "autocomplete" | "colon" | "mention" | "confirm" | "typing") {
            return;
        }
        self.aids_done = true;
        let text = std::env::var("AGU_SEND_TEXT").ok();
        match shot.as_str() {
            "emotepicker" => self.picker_open = true,
            "autocomplete" => {
                let t = text.unwrap_or_else(|| "/".into());
                self.put(&t, t.len(), cx);
                self.skip_changed -= 1;
                self.handle_change(cx);
            }
            "colon" => {
                let t = text.unwrap_or_else(|| "hi :kap".into());
                self.put(&t, t.len(), cx);
                self.skip_changed -= 1;
                self.handle_change(cx);
            }
            "mention" => {
                let t = text.unwrap_or_else(|| "hi @".into());
                self.put(&t, t.len(), cx);
                self.skip_changed -= 1;
                self.handle_change(cx);
            }
            "confirm" => self.confirm = Some(Confirmed::Raid { login: "somebody".into() }),
            _ => {
                let t = text.unwrap_or_else(|| "a message being typed".into());
                self.put(&t, t.len(), cx);
            }
        }
        self.input.read(cx).focus(window);
        cx.notify();
    }

    // ---- render pieces ----------------------------------------------------------------------------------

    fn picker_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let inv = self.inventory();
        let q = self.picker_q.read(cx).text().to_lowercase();
        let sections: Vec<PickerSection> = if q.is_empty() {
            inv.sections.clone()
        } else {
            inv.sections
                .iter()
                .map(|s| PickerSection { items: s.items.iter().filter(|it| it.name.to_lowercase().contains(&q)).cloned().collect(), ..s.clone() })
                .filter(|s| !s.items.is_empty())
                .collect()
        };
        // Only offer a jump for providers actually present: a tab that scrolls nowhere is worse than no tab.
        let all_tabs = [(PickerProvider::Twitch, "Twitch"), (PickerProvider::SevenTv, "7TV"), (PickerProvider::Bttv, "BTTV"), (PickerProvider::Ffz, "FFZ"), (PickerProvider::Rumble, "Rumble"), (PickerProvider::Kick, "Kick")];

        // Rows of the scrolling area, each with a fixed height, so only the visible ones are built (the images
        // of thousands of emotes are not even requested): label 19px, an emote row 34px (32 + the 2px gap), 8px under a section.
        enum Row<'a> {
            Label(&'a str),
            Emotes(&'a [PickerItem]),
            End,
        }
        let mut rows: Vec<(Row, f32)> = Vec::new();
        let mut starts: Vec<(PickerProvider, f32)> = Vec::new();
        let mut y = 0.0f32;
        for s in &sections {
            starts.push((s.provider, y));
            rows.push((Row::Label(&s.label), 19.0));
            y += 19.0;
            for chunk in s.items.chunks(8) {
                rows.push((Row::Emotes(chunk), 34.0));
                y += 34.0;
            }
            rows.push((Row::End, 6.0));
            y += 6.0;
        }
        let total = y;
        let off = -f32::from(self.picker_scroll.offset().y);
        let (lo, hi) = (off - 100.0, off + 256.0 + 100.0);
        let mut top_pad = 0.0f32;
        let mut bottom_pad = 0.0f32;
        let mut visible: Vec<AnyElement> = Vec::new();
        let mut cy = 0.0f32;
        let entity = cx.entity();
        for (r, h) in &rows {
            let (y0, y1) = (cy, cy + h);
            cy = y1;
            if y1 < lo {
                top_pad += h;
                continue;
            }
            if y0 > hi {
                bottom_pad += h;
                continue;
            }
            visible.push(match r {
                Row::Label(l) => div().h(px(19.)).pb(px(4.)).text_size(px(10.)).line_height(px(15.)).font_weight(FontWeight::SEMIBOLD).text_color(c(theme::FAINT)).child(crate::chat::inline::tracked(&l.to_uppercase())).into_any_element(),
                Row::Emotes(items) => {
                    let mut row = div().flex().gap(px(2.)).mb(px(2.)).h(px(32.));
                    for it in items.iter() {
                        let name = it.insert.clone().unwrap_or_else(|| it.name.clone());
                        let e = entity.clone();
                        row = row.child(
                            div()
                                .id(ElementId::from(SharedString::from(it.key.clone())))
                                .flex_1()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .hover(|s| s.bg(c(theme::RAISED_2)))
                                .child(ImageBox { url: it.img.clone().into(), side: 24.0, radius: 0.0 })
                                .on_click(move |_: &ClickEvent, w, cx| {
                                    let name = name.clone();
                                    e.update(cx, |sb, cx| sb.insert_emote(&name, w, cx));
                                }),
                        );
                    }
                    for _ in items.len()..8 {
                        row = row.child(div().flex_1());
                    }
                    row.into_any_element()
                }
                Row::End => div().h(px(6.)).into_any_element(),
            });
        }
        let _ = total;
        let mut tabs = div().mb(px(6.)).flex().flex_wrap().gap(px(4.));
        for (p, label) in all_tabs {
            if !inv.sections.iter().any(|s| s.provider == p) {
                continue;
            }
            let target = starts.iter().find(|(sp, _)| *sp == p).map(|(_, y)| *y);
            let sc = self.picker_scroll.clone();
            tabs = tabs.child(
                div()
                    .id(ElementId::from(SharedString::from(format!("ptab-{label}"))))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(theme::LINE))
                    .px(px(6.))
                    .py(px(1.))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(c(theme::MUTED))
                    .hover(|s| s.border_color(a(theme::WHITE, 0.25)).text_color(c(theme::TEXT)))
                    .child(label)
                    .on_click(move |_: &ClickEvent, _w, _cx| {
                        if let Some(y) = target {
                            sc.set_offset(point(px(0.), px(-y)));
                        }
                    }),
            );
        }
        let list = div()
            .id("picker-scroll")
            .max_h(px(256.))
            .overflow_y_scroll()
            .track_scroll(&self.picker_scroll)
            .child(div().h(px(top_pad)).flex_none())
            .children(visible)
            .child(div().h(px(bottom_pad)).flex_none());
        let list = if sections.is_empty() {
            div().py(px(24.)).flex().justify_center().text_size(px(12.)).line_height(px(16.)).text_color(c(theme::FAINT)).child("No emotes found.").into_any_element()
        } else {
            list.into_any_element()
        };
        div()
            .id("emote-picker")
            .absolute()
            .bottom(relative(1.))
            .left(px(6.))
            .mb(px(4.))
            .w(px(320.))
            .occlude()
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .p(px(8.))
            .shadow_2xl()
            .text_size(px(12.))
            .on_mouse_down_out(cx.listener(|this, _: &MouseDownEvent, _w, cx| {
                if !this.picker_btn_hover {
                    this.picker_open = false;
                    cx.notify();
                }
            }))
            .child(div().mb(px(6.)).child(self.picker_q.clone()))
            .child(tabs)
            .child(list)
            .into_any_element()
    }

    fn target_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let eff = self.effective();
        let sendable_n = self.broadcast().len();
        let mut menu = div()
            .id("target-menu")
            .absolute()
            .bottom(relative(1.))
            .left_0()
            .mb(px(4.))
            .w(px(176.))
            .occlude()
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .py(px(4.))
            .shadow_2xl()
            .text_size(px(12.))
            .line_height(px(16.));
        for s in self.sources.iter().filter(|s| SENDABLE.contains(&s.source.platform)) {
            let sk = s.sk.clone();
            let can = s.can_send;
            let sel = eff == s.sk;
            menu = menu.child(
                div()
                    .id(ElementId::from(SharedString::from(format!("tgt-{}", s.sk))))
                    .flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(4.))
                    .text_color(if sel { c(theme::TEXT) } else { c(theme::MUTED) })
                    .when(can, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
                    .when(!can, |d| d.opacity(0.5))
                    .child(icons::source_icon(&s.source, 14., true))
                    .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(if is_whisper(&s.source) { short_name(&s.source) } else { s.source.channel.clone() })))
                    .when(!can, |d| d.child(div().text_size(px(10.)).text_color(c(theme::FAINT)).child("read-only")))
                    .on_click(cx.listener(move |this, _: &ClickEvent, w, cx| {
                        if can {
                            this.target = sk.clone();
                            this.target_open = false;
                            this.input.read(cx).focus(w);
                            cx.notify();
                        }
                    })),
            );
        }
        if sendable_n > 1 {
            menu = menu.child(
                div()
                    .id("tgt-all")
                    .flex()
                    .w_full()
                    .items_center()
                    .gap(px(8.))
                    .px(px(10.))
                    .py(px(4.))
                    .text_color(if eff == ALL { c(theme::TEXT) } else { c(theme::MUTED) })
                    .hover(|s| s.bg(c(theme::RAISED_2)))
                    .child(div().w(px(14.)).flex().justify_center().text_size(px(10.)).font_weight(FontWeight::BOLD).child("All"))
                    .child(div().flex_1().child("All chats"))
                    .on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                        this.target = ALL.into();
                        this.target_open = false;
                        this.input.read(cx).focus(w);
                        cx.notify();
                    })),
            );
        }
        menu.into_any_element()
    }
}

impl Render for SendBox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.aids(window, cx);
        let disabled = self.sendable().is_empty();
        let eff = self.effective();
        let single_source = self.single().map(|s| s.source.clone());
        let show_target = self.sources.iter().filter(|s| SENDABLE.contains(&s.source.platform)).count() > 1 || self.sendable().len() > 1;
        let has_text = !self.input.read(cx).text().trim().is_empty();

        let mut root = div().flex_none().border_t_1().border_color(c(theme::LINE)).bg(c(theme::RAISED));
        if let Some(cf) = &self.confirm {
            let text = match cf {
                Confirmed::Clear => "Clear the entire chat? This can't be undone.".to_string(),
                Confirmed::Raid { login } => format!("Start raiding @{login}?"),
            };
            let sending = self.sending;
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(a(theme::RED_500, 0.3))
                    .bg(a(theme::RED_500, 0.1))
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(c(theme::RED_300))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(text)))
                    .child(btn("confirm-yes", "Confirm", BtnOpts { danger: true, disabled: sending, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.execute_confirm(cx))))
                    .child(btn("confirm-no", "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        this.confirm = None;
                        cx.notify();
                    }))),
            );
        }
        if let Some(r) = &self.reply {
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(c(theme::LINE))
                    .px(px(8.))
                    .py(px(4.))
                    .text_size(px(12.))
                    .line_height(px(16.))
                    .text_color(c(theme::MUTED))
                    .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(format!("\u{21a9} Replying to @{}: {}", r.user, r.text))))
                    .child(
                        div()
                            .id("reply-x")
                            .flex_none()
                            .rounded(px(2.))
                            .p(px(2.))
                            .hover(|s| s.text_color(c(theme::TEXT)))
                            .child(icon(icons::X, 14.0))
                            .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| this.clear_reply(cx))),
                    ),
            );
        }

        let entity = cx.entity();
        let pick_auto = {
            let e = entity.clone();
            move |i: usize, w: &mut Window, cx: &mut gpui::App| {
                e.update(cx, |sb, cx| sb.commit_auto_pick(Some(i), w, cx));
            }
        };
        let pick_mention = {
            let e = entity.clone();
            move |i: usize, w: &mut Window, cx: &mut gpui::App| {
                e.update(cx, |sb, cx| {
                    let name = sb.mentions.state.as_ref().and_then(|s| s.matches.get(i).cloned());
                    sb.apply_mention(name, w, cx);
                });
            }
        };

        let mut row = div().relative().flex().items_end().gap(px(6.)).p(px(6.));
        // a list that closed starts over at its top when it opens again
        if self.auto.is_none() {
            self.auto_at = None;
        }
        if self.mentions.state.is_none() {
            self.mention_at = None;
        }
        if !disabled {
            if let Some(a) = &self.auto {
                if self.auto_at != Some((a.start, a.idx)) {
                    self.auto_at = Some((a.start, a.idx));
                    self.auto_scroll.scroll_to_item(a.idx);
                }
                row = row.child(autocomplete_dropdown(a, &self.auto_scroll, pick_auto));
            }
            if let Some(m) = &self.mentions.state {
                if self.mention_at != Some((m.start, m.idx)) {
                    self.mention_at = Some((m.start, m.idx));
                    self.mention_scroll.scroll_to_item(m.idx);
                }
                row = row.child(mention_dropdown(m, &self.mention_scroll, pick_mention));
            }
            if self.picker_open {
                row = row.child(self.picker_el(cx));
            }
        }
        row = row.child(
            div()
                .id("emote-btn")
                .flex_none()
                .rounded(px(4.))
                .border_1()
                .border_color(c(theme::LINE))
                .p(px(6.))
                .text_color(c(theme::MUTED))
                .when(disabled, |d| d.opacity(0.4))
                .when(!disabled, |d| d.hover(|s| s.border_color(a(theme::WHITE, 0.25)).text_color(c(theme::TEXT))))
                .on_hover(cx.listener(|this, h: &bool, _w, _cx| this.picker_btn_hover = *h))
                .child(icon(icons::SMILE, 16.0))
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| {
                    if this.sendable().is_empty() {
                        return;
                    }
                    this.picker_open = !this.picker_open;
                    this.auto = None;
                    if this.picker_open {
                        this.picker_q.update(cx, |q, cx| q.set_text("", cx));
                        this.picker_q.read(cx).focus(w);
                    }
                    cx.notify();
                })),
        );
        if show_target {
            let mut t = div().relative().flex_none().flex();
            if self.target_open && !disabled {
                t = t.child(self.target_menu(cx));
            }
            let tip = if eff == ALL { "Sending to all chats".to_string() } else { self.single().map(|s| format!("Sending to {}: {}", icons::platform_label(s.source.platform), s.source.channel)).unwrap_or_else(|| "Send target".into()) };
            let _ = tip;
            t = t.child(
                div()
                    .id("target-btn")
                    .flex()
                    .items_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(c(theme::LINE))
                    .px(px(6.))
                    .py(px(6.))
                    .when(disabled, |d| d.opacity(0.4))
                    .when(!disabled, |d| d.hover(|s| s.border_color(a(theme::WHITE, 0.25))))
                    .child(if eff == ALL {
                        div().text_size(px(12.)).line_height(px(16.)).font_weight(FontWeight::BOLD).text_color(c(theme::MUTED)).child("All").into_any_element()
                    } else {
                        match &single_source {
                            Some(s) => icons::source_icon(s, 16., true),
                            None => icons::platform_icon(Platform::Twitch, 16., true),
                        }
                        .into_any_element()
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                        if !this.sendable().is_empty() {
                            this.target_open = !this.target_open;
                            cx.notify();
                        }
                    })),
            );
            row = row.child(t);
        }
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_capture_key(ev, w, cx)))
                .child(self.input.clone()),
        );
        let can_send = !(disabled || self.sending || !has_text);
        row = row.child(
            div()
                .id("send-btn")
                .flex_none()
                .flex()
                .items_center()
                .map(|mut d| {
                    d.style().align_self = Some(gpui::AlignSelf::Stretch);
                    d
                })
                .rounded(px(4.))
                .bg(c(theme::ACCENT))
                .px(px(12.))
                .py(px(4.))
                .text_size(px(12.))
                .line_height(px(16.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(c(theme::WHITE))
                .when(!can_send, |d| d.opacity(0.4))
                .when(can_send, |d| d.hover(|s| s.bg(c(theme::ACCENT_STRONG))))
                .child(if self.sending { "..." } else { "Send" })
                .on_click(cx.listener(|this, _: &ClickEvent, w, cx| this.send(w, cx))),
        );
        let _ = (MouseButton::Left, HashMap::<u8, u8>::new());
        root.child(row)
    }
}
