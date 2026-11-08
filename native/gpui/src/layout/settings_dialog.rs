//! Port of src/renderer/layout/SettingsDialog.tsx: Accounts, Appearance/chat, Highlights (highlights_section.rs),
//! Ignores, Hotkeys, Mod buttons, Updates, About. Every control reads and writes through the bridge exactly like
//! the Electron dialog (`settings_set` with a camelCase patch), so settings.json stays compatible both ways.
//!
//! React's local `useState` drafts become fields here; text fields are `TextInput` entities kept in `inputs` by
//! key and re-synced from the saved value whenever that value changes (the `useEffect` re-sync of the JS).
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agu_chat_core::protocol::{AccountState, AccountsState, AguBridge, ApiInit, AppInfo, SourceRef, DevicePrompt, IgnoredPhrase, ModButton, Platform, Settings, UpdaterState};
use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Focusable, KeyDownEvent, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, Render, ScrollHandle, SharedString, Subscription, Window,
};
use serde_json::{json, Value};

use super::channel_dialog::DialogEvent;
use super::hotkeys::{action_label, event_to_binding, find_conflicts, hotkey_actions, is_usable_binding, resolve_hotkeys, to_overrides, HotkeyAction, HotkeyTable, HOTKEY_RECORDING};
use super::settings_widgets::{checkbox, eid, select_button, select_list, select_row, slider, txt};
use super::ui::{btn, modal, toggle, BtnOpts, CloseFn};
use crate::chat::ignores::{ignore_login, phrase_error, toggle_ignored_user};
use crate::chat::mod_buttons::{
    default_icon, default_mod_buttons, describe_mod_button, format_duration, new_mod_button_id, normalize_mod_buttons, parse_duration, MAX_COMMAND, MAX_LABEL, MAX_MOD_BUTTONS,
    MOD_BUTTON_ICONS,
};
use crate::icons;
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

pub const REPO_URL: &str = "https://github.com/TrinityTF/agu-chat";
const TEMPLATE_HELP: &str = "{user} login, {name} display name, {user-id}, {msg-id}, {channel}, {msg} message text";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Accounts,
    Appearance,
    Highlights,
    Ignores,
    Hotkeys,
    ModButtons,
    Updates,
    About,
}

const SECTIONS: [(Section, &str); 8] = [
    (Section::Accounts, "Accounts"),
    (Section::Appearance, "Appearance and chat"),
    (Section::Highlights, "Highlights"),
    (Section::Ignores, "Ignores"),
    (Section::Hotkeys, "Hotkeys"),
    (Section::ModButtons, "Mod buttons"),
    (Section::Updates, "Updates"),
    (Section::About, "About"),
];

impl Section {
    /// `AGU_SHOT=settings:<name>`
    pub fn from_name(s: &str) -> Option<Section> {
        Some(match s.to_lowercase().as_str() {
            "accounts" => Section::Accounts,
            "appearance" => Section::Appearance,
            "highlights" => Section::Highlights,
            "ignores" => Section::Ignores,
            "hotkeys" => Section::Hotkeys,
            "modbuttons" => Section::ModButtons,
            "updates" => Section::Updates,
            "about" => Section::About,
            _ => return None,
        })
    }
}

pub(super) struct InputSlot {
    pub ent: Entity<TextInput>,
    /// the saved value the text was last synced to
    pub baseline: String,
    _sub: Subscription,
}

pub(super) struct SliderDrag {
    pub key: &'static str,
    pub min: f32,
    pub max: f32,
    pub step: f32,
}

#[derive(Default)]
pub(super) struct HkPending {
    pub action: Option<HotkeyAction>,
    pub binding: String,
    pub other: Option<HotkeyAction>,
}

pub struct SettingsDialog {
    pub(super) bridge: Arc<dyn AguBridge>,
    pub(super) section: Section,
    pub(super) settings: Settings,
    accounts: Option<AccountsState>,
    /// state of the twitch.tv web sign-in (`/api/twitch/web`)
    web: Value,
    updater: UpdaterState,
    info: AppInfo,
    pub(super) inputs: HashMap<String, InputSlot>,
    /// the open `<select>`
    pub(super) open_select: Option<String>,
    pub(super) drag: Option<SliderDrag>,
    pub(super) slider_bounds: HashMap<&'static str, Rc<Cell<Bounds<Pixels>>>>,
    pub(super) err: HashMap<String, String>,
    // accounts
    prompts: HashMap<Platform, DevicePrompt>,
    twitch_changing: bool,
    busy: bool,
    rumble_need_code: bool,
    copied: bool,
    // ignores
    ignore_platform: Platform,
    new_phrase: IgnoredPhrase,
    // hotkeys
    recording: Option<HotkeyAction>,
    hk_hint: String,
    hk_pending: HkPending,
    // highlights
    pub(super) color_open: Option<String>,
    pub(super) focus_rule: Option<String>,
    pub(super) focus: FocusHandle,
    scroll: ScrollHandle,
    _subs: Vec<Subscription>,
    _unsubs: Vec<crate::layout::app::Unsub>,
}

impl EventEmitter<DialogEvent> for SettingsDialog {}

impl Drop for SettingsDialog {
    fn drop(&mut self) {
        HOTKEY_RECORDING.set(false);
    }
}

pub(super) fn merge(into: &mut Value, patch: &Value) {
    if let (Some(a), Some(b)) = (into.as_object_mut(), patch.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}

/// Width of `text` at 12px Segoe UI.
fn text_width(cx: &App, text: &str) -> f32 {
    let ts = cx.text_system();
    let id = ts.resolve_font(&gpui::font(theme::FONT));
    text.chars().map(|ch| ts.advance(id, px(12.), ch).map(|a| f32::from(a.width)).unwrap_or(6.0)).sum()
}

fn err_msg(e: impl ToString) -> String {
    e.to_string()
}

impl SettingsDialog {
    pub fn new(bridge: Arc<dyn AguBridge>, initial: Section, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = bridge.settings_get();
        let updater = bridge.updater_get();
        let info = bridge.app_info();
        let mut unsubs = Vec::new();

        let (mut rx, u) = crate::layout::app::stream::<Settings>(|cb| bridge.on_settings(cb));
        unsubs.push(u);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(s) = rx.next().await {
                if this.update(cx, |this, cx| {
                    this.settings = s;
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let (mut rx, u) = crate::layout::app::stream::<UpdaterState>(|cb| bridge.on_updater(cb));
        unsubs.push(u);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(s) = rx.next().await {
                if this.update(cx, |this, cx| {
                    this.updater = s;
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let (mut rx, u) = crate::layout::app::stream::<AccountsState>(|cb| bridge.on_accounts(cb));
        unsubs.push(u);
        cx.spawn(async move |this, cx| {
            use futures::StreamExt;
            while let Some(s) = rx.next().await {
                if this.update(cx, |this, cx| {
                    this.accounts = Some(s);
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let fut = bridge.accounts_get();
        cx.spawn(async move |this, cx| {
            let s = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.accounts = Some(s);
                cx.notify();
            });
        })
        .detach();
        // the twitch.tv web sign-in follows its window and the card's own expiry
        let poll = bridge.clone();
        cx.spawn(async move |this, cx| loop {
            let r = poll.api(SourceRef { platform: Platform::Twitch, channel: String::new() }, "/api/twitch/web".to_string(), ApiInit::default()).await;
            if r.status == 200 {
                if this.update(cx, |this, cx| {
                    if this.web != r.body {
                        this.web = r.body;
                        cx.notify();
                    }
                })
                .is_err()
                {
                    break;
                }
            }
            cx.background_executor().timer(Duration::from_secs(2)).await;
        })
        .detach();
        // The device-code countdown (`useNow`) ticks once a second while a code is on screen.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this.update(cx, |this, cx| {
                if !this.prompts.is_empty() || this.accounts.as_ref().map(|a| a.values().any(|s| s.pending.is_some())).unwrap_or(false) {
                    cx.notify();
                }
            })
            .is_err()
            {
                break;
            }
        })
        .detach();

        SettingsDialog {
            bridge,
            section: initial,
            settings,
            accounts: None,
            web: Value::Null,
            updater,
            info,
            inputs: HashMap::new(),
            open_select: None,
            drag: None,
            slider_bounds: HashMap::new(),
            err: HashMap::new(),
            prompts: HashMap::new(),
            twitch_changing: false,
            busy: false,
            rumble_need_code: false,
            copied: false,
            ignore_platform: Platform::Twitch,
            new_phrase: IgnoredPhrase { pattern: String::new(), regex: false, case_sensitive: false, action: "hide".into(), replacement: String::new() },
            recording: None,
            hk_hint: String::new(),
            hk_pending: HkPending::default(),
            color_open: None,
            focus_rule: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _subs: Vec::new(),
            _unsubs: unsubs,
        }
    }

    // ---- settings I/O (useLiveSettings) -------------------------------------------------------------

    /// `setSettings(patch)`: optimistic local merge, then the bridge's answer replaces it.
    pub(super) fn set(&mut self, patch: Value, cx: &mut Context<Self>) {
        let mut cur = serde_json::to_value(&self.settings).unwrap_or_default();
        merge(&mut cur, &patch);
        if let Ok(s) = serde_json::from_value::<Settings>(cur) {
            self.settings = s;
        }
        cx.notify();
        let fut = self.bridge.settings_set(patch);
        cx.spawn(async move |this, cx| {
            let s = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.settings = s;
                cx.notify();
            });
        })
        .detach();
    }

    fn set_section(&mut self, s: Section, cx: &mut Context<Self>) {
        self.section = s;
        self.open_select = None;
        self.color_open = None;
        self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        cx.notify();
    }

    // ---- text inputs ----------------------------------------------------------------------------------

    /// The `TextInput` of `key`, created on first use with `saved` as its text and re-synced when `saved` changes.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn input(&mut self, key: &str, placeholder: &str, style: InputStyle, saved: &str, mono: bool, masked: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextInput> {
        if let Some(slot) = self.inputs.get_mut(key) {
            if slot.baseline != saved {
                slot.baseline = saved.to_string();
                let ent = slot.ent.clone();
                ent.update(cx, |t, cx| t.set_text(saved, cx));
            }
            let ent = slot.ent.clone();
            ent.update(cx, |t, cx| t.set_placeholder(placeholder, cx));
            return ent;
        }
        let ent = cx.new(|cx| {
            let mut t = TextInput::new(window, cx, placeholder, style);
            t.set_mono(mono);
            t.set_masked(masked);
            t
        });
        ent.update(cx, |t, cx| t.set_text(saved, cx));
        let k = key.to_string();
        let sub = cx.subscribe(&ent, move |this, _e, ev: &TextInputEvent, cx| this.input_event(&k, ev, cx));
        self.inputs.insert(key.to_string(), InputSlot { ent: ent.clone(), baseline: saved.to_string(), _sub: sub });
        ent
    }

    pub(super) fn text_of(&self, key: &str, cx: &App) -> String {
        self.inputs.get(key).map(|s| s.ent.read(cx).text()).unwrap_or_default()
    }

    pub(super) fn set_text_of(&mut self, key: &str, text: &str, cx: &mut Context<Self>) {
        if let Some(s) = self.inputs.get_mut(key) {
            s.baseline = text.to_string();
            let ent = s.ent.clone();
            ent.update(cx, |t, cx| t.set_text(text, cx));
        }
    }

    fn input_event(&mut self, key: &str, ev: &TextInputEvent, cx: &mut Context<Self>) {
        match ev {
            TextInputEvent::Changed => {
                // the Mod buttons "text" box saves on every keystroke (onChange)
                if let Some(id) = key.strip_prefix("mb.").and_then(|r| r.strip_suffix(".label")) {
                    let text = self.text_of(key, cx);
                    self.mod_label_changed(id, &text, cx);
                }
                cx.notify();
            }
            TextInputEvent::Enter => self.commit(key, true, cx),
            TextInputEvent::Blur => self.commit(key, false, cx),
            _ => {}
        }
    }

    fn commit(&mut self, key: &str, enter: bool, cx: &mut Context<Self>) {
        if key == "twitch.cid" {
            if enter {
                let v = self.text_of(key, cx);
                if !v.trim().is_empty() {
                    self.save_client_id(v.trim().to_string(), cx);
                }
            }
        } else if key == "ign.user" {
            if enter {
                self.add_ignored_user(cx);
            }
        } else if let Some(rest) = key.strip_prefix("ph.") {
            let mut it = rest.splitn(2, '.');
            let (idx, _field) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
            if idx == "new" {
                if enter {
                    self.add_phrase(cx);
                }
            } else if let Ok(i) = idx.parse::<usize>() {
                self.commit_phrase(i, cx);
            }
        } else if let Some(id) = key.strip_prefix("mb.").and_then(|r| r.strip_suffix(".secs")) {
            let text = self.text_of(key, cx);
            let id = id.to_string();
            self.commit_mod_secs(&id, &text, cx);
        } else if let Some(id) = key.strip_prefix("mb.").and_then(|r| r.strip_suffix(".cmd")) {
            let text = self.text_of(key, cx);
            let id = id.to_string();
            self.commit_mod_cmd(&id, &text, cx);
        } else if let Some(id) = key.strip_prefix("hl.").and_then(|r| r.strip_suffix(".pat")) {
            let id = id.to_string();
            self.commit_rule_pattern(&id, cx);
        } else if let Some(id) = key.strip_prefix("color.") {
            let id = id.to_string();
            self.commit_rule_color(&id, cx);
        } else if enter && key.starts_with("rumble.") {
            self.rumble_submit(cx);
        }
    }

    // ---- shared look ------------------------------------------------------------------------------------

    fn h(text: &str) -> gpui::Div {
        txt(13.).mb(px(8.)).mt(px(4.)).font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(theme::TEXT)).child(text.to_string())
    }

    pub(super) fn card() -> gpui::Div {
        div().mb(px(12.)).rounded(px(4.)).border_1().border_color(c(theme::LINE)).bg(c(theme::RAISED)).p(px(12.))
    }

    fn err_line(text: &str) -> gpui::Div {
        txt(11.).mt(px(8.)).text_color(c(theme::RED_400)).child(text.to_string())
    }

    /// `Row`: label (+ hint) left, control right, a hairline under it (not under the section's last row).
    pub(super) fn row(label: &str, hint: Option<&str>, control: impl IntoElement, last: bool) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.))
            .py(px(6.))
            .when(!last, |d| d.border_b_1().border_color(a(theme::LINE, 0.6)))
            .child(
                div()
                    .min_w_0()
                    .child(div().child(label.to_string()))
                    .children(hint.map(|h| txt(11.).text_color(c(theme::FAINT)).child(h.to_string()))),
            )
            .child(div().flex_none().child(control))
    }

    pub(super) fn link(&self, id: &str, url: &str, text: &str, cx: &mut Context<Self>) -> AnyElement {
        let (bridge, url) = (self.bridge.clone(), url.to_string());
        div()
            .id(eid(id))
            .underline()
            .text_color(c(theme::ACCENT_SOFT))
            .hover(|s| s.text_color(c(theme::TEXT)))
            .child(text.to_string())
            .on_click(cx.listener(move |_, _: &ClickEvent, _, _| bridge.open_external(url.clone())))
            .into_any_element()
    }

    pub(super) fn select(&self, id: &str, current: &str, opts: &[(&str, &str)], small: bool, cx: &mut Context<Self>) -> AnyElement {
        let label = opts.iter().find(|(v, _)| *v == current).map(|(_, l)| *l).unwrap_or(current);
        // a <select> is as wide as its widest option plus the arrow
        let widest = opts.iter().map(|(_, l)| text_width(cx, l)).fold(0f32, f32::max);
        let width = (widest + if small { 12. } else { 16. } + 2. + 20.).ceil();
        let open = self.open_select.as_deref() == Some(id);
        let sid = id.to_string();
        let button = select_button(eid(format!("sel-{id}")), label, width, small, open).on_click(cx.listener({
            let sid = sid.clone();
            move |this, _: &ClickEvent, _, cx| {
                this.open_select = if this.open_select.as_deref() == Some(sid.as_str()) { None } else { Some(sid.clone()) };
                cx.notify();
            }
        }));
        let mut wrap = div().relative().child(button);
        if open {
            let rows = opts
                .iter()
                .map(|(v, l)| {
                    let (sid, v2) = (sid.clone(), v.to_string());
                    select_row(eid(format!("opt-{id}-{v}")), l, *v == current)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.open_select = None;
                            this.picked(&sid, &v2, cx);
                        }))
                        .into_any_element()
                })
                .collect();
            wrap = wrap.child(select_list(width, rows));
        }
        wrap.into_any_element()
    }

    pub(super) fn picked(&mut self, id: &str, value: &str, cx: &mut Context<Self>) {
        match id {
            "pauseChat" | "emoteSize" | "deletedMessages" => self.set(json!({ id: value }), cx),
            "ign.platform" => {
                self.ignore_platform = Platform::ALL.iter().copied().find(|p| p.as_str() == value).unwrap_or(Platform::Twitch);
                cx.notify();
            }
            "ign.newaction" => {
                self.new_phrase.action = value.to_string();
                cx.notify();
            }
            "mb.action" => {}
            _ => {
                if let Some(rest) = id.strip_prefix("ph.") {
                    if let Some(i) = rest.strip_suffix(".action").and_then(|i| i.parse::<usize>().ok()) {
                        self.edit_phrase(i, |p| p.action = value.to_string(), cx);
                    }
                } else if let Some(mid) = id.strip_prefix("mb.").and_then(|r| r.strip_suffix(".action")) {
                    let mid = mid.to_string();
                    self.set_mod_action(&mid, value, cx);
                } else {
                    self.picked_highlights(id, value, cx);
                }
            }
        }
    }

    pub(super) fn bool_row(&mut self, sv: &Value, key: &'static str, label: &str, hint: Option<&str>, disabled: bool, last: bool, cx: &mut Context<Self>) -> gpui::Div {
        let on = sv[key].as_bool().unwrap_or(false);
        let mut t = toggle(eid(format!("tg-{key}")), on, disabled);
        if !disabled {
            t = t.cursor_pointer().on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set(json!({ key: !on }), cx)));
        }
        // the control sits in an inline line box: its row is 2px taller than the toggle alone
        Self::row(label, hint, div().min_h(px(20.)).flex().items_center().child(t), last)
    }

    pub(super) fn slider_row(&mut self, key: &'static str, value: f32, min: f32, max: f32, step: f32, suffix: &str, cx: &mut Context<Self>) -> gpui::Div {
        let bounds = self.slider_bounds.entry(key).or_insert_with(|| Rc::new(Cell::new(Bounds::default()))).clone();
        let frac = (value - min) / (max - min);
        let s = slider(eid(format!("sl-{key}")), frac, bounds).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                this.drag = Some(SliderDrag { key, min, max, step });
                this.slider_to(ev.position.x, cx);
            }),
        );
        div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(s)
            .child(div().w(px(32.)).flex().justify_end().text_color(c(theme::MUTED)).child(format!("{}{suffix}", value.round() as i64)))
    }

    fn slider_to(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some(d) = &self.drag else { return };
        let Some(b) = self.slider_bounds.get(d.key).map(|b| b.get()) else { return };
        let thumb = 14.0;
        let w = (f32::from(b.size.width) - thumb).max(1.0);
        let f = ((f32::from(x) - f32::from(b.origin.x) - thumb / 2.) / w).clamp(0.0, 1.0);
        let raw = d.min + f * (d.max - d.min);
        let v = (raw / d.step).round() * d.step;
        let v = v.clamp(d.min, d.max);
        let key = d.key;
        let cur = serde_json::to_value(&self.settings).ok().and_then(|s| s[key].as_f64()).unwrap_or(f64::NAN) as f32;
        if (cur - v).abs() > f32::EPSILON || cur.is_nan() {
            self.set(json!({ key: v as i64 }), cx);
        }
    }

    // ---- accounts ---------------------------------------------------------------------------------------

    fn account_title(platform: Platform) -> gpui::Div {
        div()
            .mb(px(8.))
            .flex()
            .items_center()
            .justify_between()
            .child(txt(13.).flex().items_center().gap(px(8.)).font_weight(gpui::FontWeight::SEMIBOLD).child(icons::platform_icon(platform, 16., true)).child(icons::platform_label(platform)))
    }

    fn logout(&mut self, platform: Platform, cx: &mut Context<Self>) {
        let fut = self.bridge.accounts_logout(platform);
        cx.spawn(async move |_this, _cx| {
            fut.await;
        })
        .detach();
    }

    fn connected_line(&self, platform: Platform, st: &AccountState, cx: &mut Context<Self>) -> AnyElement {
        let name = st.user.as_ref().map(|u| if !u.name.is_empty() { u.name.clone() } else { u.login.clone() }).filter(|s| !s.is_empty()).unwrap_or_else(|| "unknown".into());
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .child(div().min_w_0().truncate().child("Logged in as ").child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(name)))
            .child(btn(eid(format!("logout-{}", platform.as_str())), "Log out", BtnOpts::danger()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.logout(platform, cx))))
            .into_any_element()
    }

    fn device_code(&mut self, platform: Platform, p: &DevicePrompt, cx: &mut Context<Self>) -> AnyElement {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let left = ((p.expires_at - now) as f64 / 1000.0).round().max(0.0) as i64;
        let code = p.user_code.clone();
        let spaced: String = code.chars().map(|c| c.to_string()).collect::<Vec<_>>().join("\u{2009}");
        let uri = p.verification_uri.clone();
        let host: String = uri.trim_start_matches("https://").trim_start_matches("http://").split(['?', '#']).next().unwrap_or("").to_string();
        let copied = self.copied;
        div()
            .rounded(px(4.))
            .border_1()
            .border_color(a(theme::ACCENT, 0.5))
            .bg(theme::mention())
            .p(px(12.))
            .child(div().mb(px(4.)).text_color(c(theme::MUTED)).child("Open the page below and enter this code:"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .rounded(px(4.))
                            .bg(c(theme::APP))
                            .px(px(12.))
                            .py(px(6.))
                            .text_size(px(26.))
                            .line_height(px(39.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(c(theme::TEXT))
                            .font_family("Consolas")
                            .child(spaced),
                    )
                    .child(
                        btn(eid("dc-copy"), "", BtnOpts::default())
                            .child(div().flex().items_center().gap(px(4.)).child(icons::icon(icons::COPY, 12.)).child(if copied { "Copied" } else { "Copy" }))
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                super::ui::copy_text(cx, &code);
                                this.copied = true;
                                cx.notify();
                                cx.spawn(async move |this, cx| {
                                    cx.background_executor().timer(Duration::from_millis(1500)).await;
                                    let _ = this.update(cx, |this, cx| {
                                        this.copied = false;
                                        cx.notify();
                                    });
                                })
                                .detach();
                            })),
                    ),
            )
            .child(
                div()
                    .mt(px(12.))
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(8.))
                    .child(btn(eid("dc-open"), format!("Open {host}"), BtnOpts::primary()).on_click({
                        let (bridge, uri) = (self.bridge.clone(), uri.clone());
                        cx.listener(move |_, _: &ClickEvent, _, _| bridge.open_external(uri.clone()))
                    }))
                    .child(btn(eid("dc-cancel"), "Cancel", BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.cancel_login(platform, cx))))
                    .child(txt(11.).text_color(c(theme::FAINT)).child(if left > 0 {
                        format!("Waiting for you to approve… code expires in {}:{:02}", left / 60, left % 60)
                    } else {
                        "This code has expired. Cancel and try again.".to_string()
                    })),
            )
            .into_any_element()
    }

    fn cancel_login(&mut self, platform: Platform, cx: &mut Context<Self>) {
        self.prompts.remove(&platform);
        let fut = self.bridge.accounts_cancel(platform);
        cx.spawn(async move |_this, _cx| {
            fut.await;
        })
        .detach();
        cx.notify();
    }

    fn login(&mut self, platform: Platform, args: Value, cx: &mut Context<Self>) {
        self.err.remove(platform.as_str());
        if platform == Platform::Twitch {
            self.busy = true;
        }
        let fut = self.bridge.accounts_login(platform, args);
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(v) => {
                        if let Ok(p) = serde_json::from_value::<DevicePrompt>(v.clone()) {
                            this.prompts.insert(platform, p);
                        }
                        if platform == Platform::Rumble {
                            this.rumble_login_result(&v, cx);
                        }
                    }
                    Err(e) => {
                        this.err.insert(platform.as_str().to_string(), err_msg(e));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn save_client_id(&mut self, v: String, cx: &mut Context<Self>) {
        self.set(json!({ "twitchClientId": v.trim() }), cx);
        self.set_text_of("twitch.cid", "", cx);
        self.twitch_changing = false;
        cx.notify();
    }

    fn twitch_account(&mut self, st: &AccountState, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if st.connected {
            self.prompts.remove(&Platform::Twitch);
        }
        let pending = st.pending.clone().or_else(|| self.prompts.get(&Platform::Twitch).cloned());
        let own_id = self.settings.twitch_client_id.clone();
        let err = self.err.get("twitch").cloned().or_else(|| st.error.clone()).unwrap_or_default();
        let body: AnyElement = if st.connected {
            self.connected_line(Platform::Twitch, st, cx)
        } else if let Some(p) = pending {
            self.device_code(Platform::Twitch, &p, cx)
        } else {
            let busy = self.busy;
            let login = btn(eid("tw-login"), "Log in with Twitch", BtnOpts { primary: true, disabled: busy, ..Default::default() });
            let login = if busy { login } else { login.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.login(Platform::Twitch, json!({}), cx))) };
            let advanced: AnyElement = if self.twitch_changing {
                let input = self.input("twitch.cid", "Your app's Client ID", InputStyle::Field, "", false, false, window, cx);
                let can_save = !self.text_of("twitch.cid", cx).trim().is_empty();
                let save = btn(eid("tw-save"), "Save", BtnOpts { primary: true, disabled: !can_save, ..Default::default() });
                let save = if can_save {
                    save.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        let v = this.text_of("twitch.cid", cx);
                        this.save_client_id(v, cx);
                    }))
                } else {
                    save
                };
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.))
                    .child(div().flex_1().min_w_0().child(input))
                    .child(save)
                    .child(btn(eid("tw-cancel"), "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.twitch_changing = false;
                        cx.notify();
                    })))
                    .into_any_element()
            } else if !own_id.is_empty() {
                let short: String = own_id.chars().take(6).collect();
                txt(11.)
                    .flex()
                    .text_color(c(theme::FAINT))
                    .child(format!("Using your own Twitch app ({short}…). "))
                    .child(
                        div()
                            .id("tw-default")
                            .underline()
                            .hover(|s| s.text_color(c(theme::TEXT)))
                            .child("Use Agu Chat's")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_client_id(String::new(), cx))),
                    )
                    .into_any_element()
            } else {
                txt(11.)
                    .text_color(c(theme::FAINT))
                    .child(
                        div()
                            .id("tw-adv")
                            .underline()
                            .hover(|s| s.text_color(c(theme::TEXT)))
                            .child("Advanced: log in through your own Twitch app")
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.twitch_changing = true;
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(12.))
                        .child(login)
                        .child(txt(11.).text_color(c(theme::FAINT)).child("You approve it on twitch.tv; your password never goes through Agu Chat.")),
                )
                .child(advanced)
                .into_any_element()
        };
        let web_block = self.twitch_web_block(cx);
        Self::card().child(Self::account_title(Platform::Twitch)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).child(web_block).into_any_element()
    }

    /// Calls core's twitch.tv web sign-in routes (`/api/twitch/web...`); the answer is the sign-in's state.
    fn web_call(&mut self, path: &'static str, post: bool, cx: &mut Context<Self>) {
        let fut = self.bridge.api(SourceRef { platform: Platform::Twitch, channel: String::new() }, path.to_string(), ApiInit { method: post.then(|| "POST".to_string()), body: post.then(|| json!({})) });
        cx.spawn(async move |this, cx| {
            let r = fut.await;
            let _ = this.update(cx, |this, cx| {
                if r.status == 200 && r.body.get("signedIn").is_some() {
                    this.web = r.body;
                } else if r.status != 200 {
                    this.err.insert("twitch-web".into(), r.body.get("error").and_then(|e| e.as_str()).unwrap_or("Could not reach the sign-in").to_string());
                }
                cx.notify();
            });
        })
        .detach();
        if post {
            let again = self.bridge.api(SourceRef { platform: Platform::Twitch, channel: String::new() }, "/api/twitch/web".to_string(), ApiInit::default());
            cx.spawn(async move |this, cx| {
                let r = again.await;
                let _ = this.update(cx, |this, cx| {
                    if r.status == 200 {
                        this.web = r.body;
                    }
                    cx.notify();
                });
            })
            .detach();
        }
    }

    /// The twitch.tv web sign-in (separate from the Twitch login above): what the user card reads Twitch's own
    /// mod logs, comments and badges with.
    fn twitch_web_block(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let w = self.web.clone();
        let flag = |k: &str| w.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
        let text = |k: &str| w.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let err = self.err.get("twitch-web").cloned().unwrap_or_else(|| text("error"));
        let head = txt(12.).font_weight(gpui::FontWeight::SEMIBOLD).child("twitch.tv (mod logs)");
        let body: AnyElement = if flag("signedIn") {
            let mut col = div().flex().flex_col().gap(px(6.)).child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .child(div().min_w_0().truncate().child("Signed in as ").child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(text("login"))))
                    .child(btn(eid("web-out"), "Sign out", BtnOpts::danger()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.err.remove("twitch-web");
                        this.web_call("/api/twitch/web/logout", true, cx);
                    }))),
            );
            if w.get("sameUser").and_then(|v| v.as_bool()) == Some(false) {
                col = col.child(txt(11.).text_color(c(theme::AMBER_300)).child(format!("This is not the Twitch account Agu Chat is logged in as ({}). Mod logs only work for the account that moderates.", text("appLogin"))));
            }
            col.into_any_element()
        } else if flag("pending") {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(div().text_color(c(theme::MUTED)).child("Waiting for you to sign in on twitch.tv in the sign-in window..."))
                .child(btn(eid("web-cancel"), "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.web_call("/api/twitch/web/cancel", true, cx))))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(8.))
                .child(btn(eid("web-in"), "Sign in", BtnOpts::primary()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.err.remove("twitch-web");
                    this.web_call("/api/twitch/web/login", true, cx);
                })))
                .child(txt(11.).text_color(c(theme::FAINT)).child(if flag("expired") { "Your twitch.tv sign-in expired. Sign in again to see warnings, timeouts, bans and mod comments on the user card." } else { "Shows Twitch's own warnings, timeouts, bans and shared mod comments on the user card, like the 7TV extension does. You sign in on twitch.tv inside Agu Chat; only the session is kept, encrypted on this PC." }))
                .into_any_element()
        };
        div().mt(px(14.)).pt(px(12.)).border_t_1().border_color(c(theme::LINE)).child(div().mb(px(8.)).child(head)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).into_any_element()
    }


    fn rumble_login_result(&mut self, v: &Value, cx: &mut Context<Self>) {
        self.set_text_of("rumble.pass", "", cx);
        if v["twoFactor"].as_bool().unwrap_or(false) {
            self.rumble_need_code = true;
        } else {
            self.rumble_need_code = false;
            self.set_text_of("rumble.code", "", cx);
        }
    }

    fn rumble_submit(&mut self, cx: &mut Context<Self>) {
        let (u, p, code) = (self.text_of("rumble.user", cx), self.text_of("rumble.pass", cx), self.text_of("rumble.code", cx));
        let ok = if self.rumble_need_code { !code.trim().is_empty() } else { !u.trim().is_empty() && !p.is_empty() };
        if self.busy || !ok {
            return;
        }
        self.busy = true;
        let args = if self.rumble_need_code { json!({ "code": code.trim() }) } else { json!({ "username": u.trim(), "password": p }) };
        self.login(Platform::Rumble, args, cx);
        self.busy = true;
    }

    fn rumble_account(&mut self, st: &AccountState, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if st.unsupported {
            return self.other_account(Platform::Rumble, st, cx);
        }
        let err = self.err.get("rumble").cloned().or_else(|| st.error.clone()).unwrap_or_default();
        let body: AnyElement = if st.connected {
            self.connected_line(Platform::Rumble, st, cx)
        } else {
            let need = self.rumble_need_code;
            let fields: AnyElement = if need {
                let code = self.input("rumble.code", "Verification code", InputStyle::Field, "", false, false, window, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_color(c(theme::MUTED)).child("Rumble sent a verification code. Enter it to finish logging in."))
                    .child(code)
                    .into_any_element()
            } else {
                let user = self.input("rumble.user", "Rumble username", InputStyle::Field, "", false, false, window, cx);
                let pass = self.input("rumble.pass", "Password", InputStyle::Field, "", false, true, window, cx);
                div().flex().flex_col().gap(px(8.)).child(user).child(pass).into_any_element()
            };
            let (u, p, code) = (self.text_of("rumble.user", cx), self.text_of("rumble.pass", cx), self.text_of("rumble.code", cx));
            let disabled = self.busy || if need { code.trim().is_empty() } else { u.trim().is_empty() || p.is_empty() };
            let submit = btn(eid("ru-submit"), if need { "Verify" } else { "Log in" }, BtnOpts { primary: true, disabled, ..Default::default() });
            let submit = if disabled { submit } else { submit.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.rumble_submit(cx))) };
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(fields)
                .child(div().flex().items_center().gap(px(12.)).child(submit).children(need.then(|| {
                    btn(eid("ru-back"), "Back", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.rumble_need_code = false;
                        this.set_text_of("rumble.code", "", cx);
                        cx.notify();
                    }))
                })))
                .child(txt(11.).text_color(c(theme::FAINT)).child("Your password is sent to Rumble once to sign in and is never stored. Only the resulting session is kept, encrypted on this PC."))
                .into_any_element()
        };
        Self::card().child(Self::account_title(Platform::Rumble)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).into_any_element()
    }

    fn other_account(&mut self, platform: Platform, st: &AccountState, cx: &mut Context<Self>) -> AnyElement {
        if st.connected {
            self.prompts.remove(&platform);
        }
        let pending = st.pending.clone().or_else(|| self.prompts.get(&platform).cloned());
        let err = self.err.get(platform.as_str()).cloned().or_else(|| st.error.clone()).unwrap_or_default();
        let body: AnyElement = if st.unsupported {
            div().text_color(c(theme::MUTED)).child("Read-only in this beta. You can read chat, but sending and moderating from here is not available yet.").into_any_element()
        } else if st.connected {
            self.connected_line(platform, st, cx)
        } else if let Some(p) = pending {
            self.device_code(platform, &p, cx)
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(div().text_color(c(theme::MUTED)).child("Not logged in."))
                .child(btn(eid(format!("login-{}", platform.as_str())), "Log in", BtnOpts::default()).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.login(platform, json!({}), cx))))
                .into_any_element()
        };
        Self::card().child(Self::account_title(platform)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).into_any_element()
    }

    /// Kick signs in through the browser (the Agu Chat login service holds Kick's app secret): there is no code to
    /// type, the app just waits for the browser to come back.
    fn kick_account(&mut self, st: &AccountState, cx: &mut Context<Self>) -> AnyElement {
        let err = self.err.get("kick").cloned().or_else(|| st.error.clone()).unwrap_or_default();
        let body: AnyElement = if st.connected {
            self.connected_line(Platform::Kick, st, cx)
        } else if let Some(p) = st.pending.clone() {
            let uri = p.verification_uri.clone();
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(div().text_color(c(theme::MUTED)).child("Waiting for you to approve in your browser…"))
                .child(btn(eid("kick-reopen"), "Open browser again", BtnOpts::default()).on_click({
                    let bridge = self.bridge.clone();
                    cx.listener(move |_, _: &ClickEvent, _, _| bridge.open_external(uri.clone()))
                }))
                .child(btn(eid("kick-cancel"), "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_login(Platform::Kick, cx))))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(8.))
                .child(btn(eid("login-kick"), "Log in", BtnOpts::primary()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.login(Platform::Kick, json!({}), cx))))
                .child(txt(11.).text_color(c(theme::FAINT)).child("You approve it on kick.com in your browser; your password never goes through Agu Chat."))
                .into_any_element()
        };
        Self::card().child(Self::account_title(Platform::Kick)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).into_any_element()
    }

    /// X signs in through a window inside Agu Chat that shows x.com's own login page (the password goes to x.com,
    /// not to Agu Chat); only the resulting session is kept, encrypted on this PC.
    fn x_account(&mut self, st: &AccountState, cx: &mut Context<Self>) -> AnyElement {
        let err = self.err.get("x").cloned().or_else(|| st.error.clone()).unwrap_or_default();
        let body: AnyElement = if st.connected {
            self.connected_line(Platform::X, st, cx)
        } else if st.pending.is_some() {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(px(8.))
                .child(div().text_color(c(theme::MUTED)).child("Waiting for you to log in to X in the sign-in window…"))
                .child(btn(eid("x-cancel"), "Cancel", BtnOpts::default()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.cancel_login(Platform::X, cx))))
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(8.))
                .child(btn(eid("login-x"), "Log in", BtnOpts::primary()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.login(Platform::X, json!({}), cx))))
                .child(txt(11.).text_color(c(theme::FAINT)).child("X chat shows names and can be written to once you log in. You sign in on x.com inside Agu Chat; only the session is kept, encrypted on this PC."))
                .into_any_element()
        };
        Self::card().child(Self::account_title(Platform::X)).child(body).children((!err.is_empty()).then(|| Self::err_line(&err))).into_any_element()
    }

    fn render_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(accounts) = self.accounts.clone() else { return div().text_color(c(theme::FAINT)).child("Loading…").into_any_element() };
        let get = |p: Platform| accounts.get(&p).cloned().unwrap_or_default();
        div()
            .child(Self::h("Accounts"))
            .child(self.twitch_account(&get(Platform::Twitch), window, cx))
            .child(self.rumble_account(&get(Platform::Rumble), window, cx))
            .child(self.kick_account(&get(Platform::Kick), cx))
            .child(self.other_account(Platform::Youtube, &get(Platform::Youtube), cx))
            .child(self.x_account(&get(Platform::X), cx))
            .into_any_element()
    }

    // ---- appearance ---------------------------------------------------------------------------------------

    fn render_appearance(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let sv = serde_json::to_value(&self.settings).unwrap_or_default();
        let s = self.settings.clone();
        let pause = self.select("pauseChat", &s.pause_chat, &[("scroll", "Scroll only"), ("hover", "While the mouse is over it"), ("alt", "While Alt is held")], false, cx);
        let emote = self.select("emoteSize", &s.emote_size, &[("small", "Small"), ("medium", "Medium"), ("large", "Large")], false, cx);
        let deleted = self.select("deletedMessages", &s.deleted_messages, &[("strike", "Strike through"), ("hide", "Hide"), ("show", "Show normally")], false, cx);
        let font = self.slider_row("fontSize", s.font_size, 10., 22., 1., "px", cx);
        div()
            .child(Self::h("Window"))
            .child(self.bool_row(&sv, "alwaysOnTop", "Always on top", Some("Keep Agu Chat above other windows"), false, false, cx))
            .child(Self::h("Messages"))
            .child(self.bool_row(&sv, "timestamps", "Show timestamps", None, false, false, cx))
            .child(self.bool_row(&sv, "timestampSeconds", "Timestamps with seconds", None, !s.timestamps, false, cx))
            .child(self.bool_row(&sv, "alternateRows", "Alternate row backgrounds", None, false, false, cx))
            .child(Self::row("Pause chat", Some("Scrolling up always pauses it"), pause, false))
            .child(Self::row("Font size", Some("Chat text, in pixels"), font, false))
            .child(self.bool_row(&sv, "readableColors", "Readable name colors", Some("Lighten dark name colors so they stay legible"), false, false, cx))
            .child(self.bool_row(&sv, "showPlatformMarks", "Platform marks on rows", Some("A small platform icon in merged co-chats"), false, false, cx))
            .child(Self::h("Emotes and badges"))
            .child(Self::row("Emote size", None, emote, false))
            .child(self.bool_row(&sv, "showBadges", "Show badges", None, false, false, cx))
            .child(self.bool_row(&sv, "sevenTv", "7TV emotes", None, false, false, cx))
            .child(self.bool_row(&sv, "bttv", "BetterTTV emotes", None, false, false, cx))
            .child(self.bool_row(&sv, "ffz", "FrankerFaceZ emotes", None, false, false, cx))
            .child(self.bool_row(&sv, "sevenTvCosmetics", "7TV cosmetics", Some("Name paints and badges from 7TV"), false, false, cx))
            .child(self.bool_row(&sv, "animatedEmotes", "Animated emotes", Some("Off keeps emotes still and uses far less GPU. Changing it reloads the chats"), false, false, cx))
            .child(Self::h("Moderation"))
            .child(Self::row("Deleted messages", None, deleted, false))
            .child(self.bool_row(&sv, "showModButtons", "Show mod buttons", Some("Inline buttons on rows when mod mode is on"), false, false, cx))
            .child(self.bool_row(&sv, "showModActions", "Show moderation actions", Some("Bans, timeouts and mode changes as lines in chat"), false, false, cx))
            .child(self.bool_row(&sv, "showAutomod", "Show AutoMod", Some("Messages AutoMod held, with Allow and Deny"), false, true, cx))
            .into_any_element()
    }

    // ---- ignores --------------------------------------------------------------------------------------------

    fn chip(&self, id: String, on: bool, label: &str, cx: &mut Context<Self>, f: impl Fn(&mut Self, &mut Context<Self>) + 'static) -> AnyElement {
        div()
            .id(eid(id))
            .flex_none()
            .rounded(px(4.))
            .border_1()
            .px(px(6.))
            .py(px(4.))
            .font_family("Consolas")
            .text_size(px(11.))
            .line_height(px(11.))
            .when(on, |d| d.border_color(c(theme::ACCENT)).bg(theme::mention()).text_color(c(theme::TEXT)))
            .when(!on, |d| d.border_color(c(theme::LINE)).text_color(c(theme::FAINT)).hover(|s| s.text_color(c(theme::TEXT))))
            .child(label.to_string())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| f(this, cx)))
            .into_any_element()
    }

    pub(super) fn icon_btn(&self, id: String, icon: &'static str, size: f32, danger_hover: bool, disabled: bool) -> gpui::Stateful<gpui::Div> {
        div()
            .id(eid(id))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.))
            .rounded(px(4.))
            .text_color(c(theme::MUTED))
            .when(disabled, |d| d.opacity(0.3))
            .when(!disabled, |d| d.hover(move |s| s.bg(c(theme::RAISED_2)).text_color(c(if danger_hover { theme::RED_400 } else { theme::TEXT }))))
            .child(icons::icon(icon, size))
    }

    fn edit_phrase(&mut self, i: usize, f: impl FnOnce(&mut IgnoredPhrase), cx: &mut Context<Self>) {
        let mut v = self.settings.ignored_phrases.clone();
        if let Some(p) = v.get_mut(i) {
            let pat = self.text_of(&format!("ph.{i}.pat"), cx);
            let repl = self.text_of(&format!("ph.{i}.repl"), cx);
            let before = p.clone();
            f(p);
            p.pattern = pat;
            if before.action == "replace" || p.action == "replace" {
                p.replacement = if p.action == "replace" { repl } else { p.replacement.clone() };
            }
            if p.pattern.is_empty() || *p == before {
                return;
            }
            self.set(json!({ "ignoredPhrases": v }), cx);
        }
    }

    fn commit_phrase(&mut self, i: usize, cx: &mut Context<Self>) {
        self.edit_phrase(i, |_| {}, cx);
    }

    fn add_phrase(&mut self, cx: &mut Context<Self>) {
        let pat = self.text_of("ph.new.pat", cx);
        let repl = self.text_of("ph.new.repl", cx);
        let mut p = self.new_phrase.clone();
        p.pattern = pat;
        p.replacement = repl;
        if p.pattern.is_empty() || phrase_error(&p.pattern, p.regex, p.case_sensitive).is_some() {
            return;
        }
        let mut v = self.settings.ignored_phrases.clone();
        v.push(p);
        self.set(json!({ "ignoredPhrases": v }), cx);
        self.new_phrase = IgnoredPhrase { pattern: String::new(), regex: false, case_sensitive: false, action: "hide".into(), replacement: String::new() };
        self.set_text_of("ph.new.pat", "", cx);
        self.set_text_of("ph.new.repl", "", cx);
        cx.notify();
    }

    fn add_ignored_user(&mut self, cx: &mut Context<Self>) {
        let l = ignore_login(&self.text_of("ign.user", cx), "");
        if l.is_empty() {
            return;
        }
        let v = toggle_ignored_user(&self.settings.ignored_users, self.ignore_platform, &l, true);
        self.set(json!({ "ignoredUsers": v }), cx);
        self.set_text_of("ign.user", "", cx);
    }

    /// `PhraseEditor`
    fn phrase_editor(&mut self, idx: Option<usize>, last: bool, value: &IgnoredPhrase, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let adding = idx.is_none();
        let k = idx.map(|i| i.to_string()).unwrap_or_else(|| "new".into());
        let (pat_key, repl_key) = (format!("ph.{k}.pat"), format!("ph.{k}.repl"));
        let pat = self.input(&pat_key, if value.regex { "Regular expression" } else { "Phrase" }, InputStyle::Field, &value.pattern, value.regex, false, window, cx);
        let draft_pat = self.text_of(&pat_key, cx);
        let err = if draft_pat.is_empty() { None } else { phrase_error(&draft_pat, value.regex, value.case_sensitive) };
        pat.update(cx, |t, cx| t.set_invalid(err.is_some(), cx));
        let regex_chip = self.chip(format!("ph-{k}-rx"), value.regex, ".*", cx, move |this, cx| match idx {
            Some(i) => this.edit_phrase(i, |p| p.regex = !p.regex, cx),
            None => {
                this.new_phrase.regex = !this.new_phrase.regex;
                cx.notify();
            }
        });
        let case_chip = self.chip(format!("ph-{k}-cs"), value.case_sensitive, "Aa", cx, move |this, cx| match idx {
            Some(i) => this.edit_phrase(i, |p| p.case_sensitive = !p.case_sensitive, cx),
            None => {
                this.new_phrase.case_sensitive = !this.new_phrase.case_sensitive;
                cx.notify();
            }
        });
        let action_id = if adding { "ign.newaction".to_string() } else { format!("ph.{k}.action") };
        let action = self.select(&action_id, &value.action, &[("hide", "Hide message"), ("replace", "Replace with")], true, cx);
        let repl = (value.action == "replace").then(|| {
            let e = self.input(&repl_key, "(nothing)", InputStyle::Field, &value.replacement, false, false, window, cx);
            div().w(px(112.)).flex_none().child(e)
        });
        let tail: AnyElement = if adding {
            let ok = !draft_pat.is_empty() && err.is_none();
            let b = btn(eid("ph-add"), "Add", BtnOpts { primary: true, small: true, disabled: !ok, ..Default::default() });
            if ok { b.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_phrase(cx))).into_any_element() } else { b.into_any_element() }
        } else {
            let i = idx.unwrap();
            self.icon_btn(format!("ph-rm-{i}"), icons::TRASH, 14., true, false)
                .tooltip(super::ui::tip("Remove"))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let mut v = this.settings.ignored_phrases.clone();
                    if i < v.len() {
                        v.remove(i);
                    }
                    this.set(json!({ "ignoredPhrases": v }), cx);
                }))
                .into_any_element()
        };
        div()
            .py(px(6.))
            .when(!adding && !last, |d| d.border_b_1().border_color(a(theme::LINE, 0.6)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().min_w_0().flex_1().child(pat))
                    .child(regex_chip)
                    .child(case_chip)
                    .child(action)
                    .children(repl)
                    .child(tail),
            )
            .children(err.map(|e| txt(11.).mt(px(4.)).text_color(c(theme::RED_400)).child(format!("Invalid regular expression: {e}"))))
            .into_any_element()
    }

    fn render_ignores(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let users = self.settings.ignored_users.clone();
        let phrases = self.settings.ignored_phrases.clone();
        let platform = self.ignore_platform;
        let plats: Vec<(&str, &str)> = Platform::ALL.iter().map(|p| (p.as_str(), icons::platform_label(*p))).collect();
        let sel = self.select("ign.platform", platform.as_str(), &plats, true, cx);
        let ph = if platform == Platform::Youtube { "Channel name" } else { "Username" };
        let input = self.input("ign.user", ph, InputStyle::Field, "", false, false, window, cx);
        let typed = ignore_login(&self.text_of("ign.user", cx), "");
        let add = btn(eid("ign-add"), "Add", BtnOpts { primary: true, small: true, disabled: typed.is_empty(), ..Default::default() });
        let add = if typed.is_empty() { add } else { add.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.add_ignored_user(cx))) };
        let mut sorted: Vec<(usize, agu_chat_core::protocol::IgnoredUser)> = users.iter().cloned().enumerate().collect();
        let pidx = |p: Platform| Platform::ALL.iter().position(|x| *x == p).unwrap_or(0);
        sorted.sort_by(|a, b| pidx(a.1.platform).cmp(&pidx(b.1.platform)).then_with(|| a.1.login.cmp(&b.1.login)));
        let n_users = sorted.len();
        let user_rows: Vec<AnyElement> = sorted
            .into_iter()
            .enumerate()
            .map(|(row_i, (i, u))| {
                let title = format!("Stop ignoring {}", u.login);
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .py(px(4.))
                    .when(row_i + 1 < n_users, |d| d.border_b_1().border_color(a(theme::LINE, 0.6)))
                    .child(icons::platform_icon(u.platform, 14., true))
                    .child(div().min_w_0().flex_1().truncate().child(u.login.clone()))
                    .child(self.icon_btn(format!("ign-rm-{i}"), icons::TRASH, 14., true, false).tooltip(super::ui::tip(title)).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let v: Vec<_> = this.settings.ignored_users.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, u)| u.clone()).collect();
                        this.set(json!({ "ignoredUsers": v }), cx);
                    })))
                    .into_any_element()
            })
            .collect();
        let new_phrase = self.new_phrase.clone();
        let adder = self.phrase_editor(None, true, &new_phrase, window, cx);
        let phrase_rows: Vec<AnyElement> = phrases.iter().enumerate().map(|(i, p)| self.phrase_editor(Some(i), i + 1 == phrases.len(), p, window, cx)).collect();
        div()
            .child(Self::h("Ignored users"))
            .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child("Messages from these users are hidden in every split. You can also use \"Ignore user\" on a user card."))
            .child(div().mb(px(8.)).flex().items_center().gap(px(6.)).child(sel).child(div().min_w_0().flex_1().child(input)).child(add))
            .child(Self::card().child(if user_rows.is_empty() { div().text_color(c(theme::FAINT)).child("No ignored users.").into_any_element() } else { div().children(user_rows).into_any_element() }))
            .child(Self::h("Ignored phrases"))
            .child(
                txt(11.)
                    .mb(px(8.))
                    .text_color(c(theme::FAINT))
                    .child("A message containing the phrase is hidden, or the matching text is replaced. .* treats the phrase as a regular expression, Aa matches case."),
            )
            .child(adder)
            .child(Self::card().child(if phrase_rows.is_empty() { div().text_color(c(theme::FAINT)).child("No ignored phrases.").into_any_element() } else { div().children(phrase_rows).into_any_element() }))
            .into_any_element()
    }

    // ---- hotkeys ----------------------------------------------------------------------------------------------

    fn hk_table(&self) -> HotkeyTable {
        resolve_hotkeys(&serde_json::to_value(&self.settings.hotkeys).unwrap_or_default())
    }

    fn hk_save(&mut self, t: &HotkeyTable, cx: &mut Context<Self>) {
        self.set(json!({ "hotkeys": to_overrides(t) }), cx);
    }

    fn hk_set_recording(&mut self, r: Option<HotkeyAction>, window: &mut Window, cx: &mut Context<Self>) {
        self.recording = r;
        HOTKEY_RECORDING.set(r.is_some());
        if r.is_some() {
            window.focus(&self.focus);
        }
        cx.notify();
    }

    /// The press-to-record keydown (the capture-phase `keydown` listener of the JS).
    fn on_record_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rec) = self.recording else { return };
        window.prevent_default();
        cx.stop_propagation();
        let ks = &ev.keystroke;
        if ks.key == "escape" && !ks.modifiers.control && !ks.modifiers.alt && !ks.modifiers.shift && !ks.modifiers.platform {
            self.hk_set_recording(None, window, cx);
            return;
        }
        let Some(b) = event_to_binding(&crate::layout::app::key_like(ks)) else { return };
        if !is_usable_binding(&b) {
            self.hk_hint = format!("{b} would get in the way of typing. Hold Ctrl or Alt, or use a function key.");
            cx.notify();
            return;
        }
        let t = self.hk_table();
        self.hk_set_recording(None, window, cx);
        self.hk_hint.clear();
        if t.get(&rec).map(|l| l.contains(&b)).unwrap_or(false) {
            return;
        }
        let other = hotkey_actions().iter().find(|a| a.id != rec && t.get(&a.id).map(|l| l.contains(&b)).unwrap_or(false));
        if let Some(o) = other {
            self.hk_pending = HkPending { action: Some(rec), binding: b, other: Some(o.id) };
        } else {
            let mut t = t;
            t.entry(rec).or_default().push(b);
            self.hk_save(&t, cx);
        }
        cx.notify();
    }

    fn render_hotkeys(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let table = self.hk_table();
        let conflicts = find_conflicts(&table);
        let any_override = !to_overrides(&table).is_empty();
        let mut groups: Vec<&'static str> = Vec::new();
        for a in hotkey_actions() {
            if !groups.contains(&a.group) {
                groups.push(a.group);
            }
        }
        let reset_all = btn(eid("hk-reset-all"), "Reset all", BtnOpts { small: true, disabled: !any_override, ..Default::default() });
        let reset_all = if any_override { reset_all.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set(json!({ "hotkeys": {} }), cx))) } else { reset_all };
        let mut out = div()
            .child(div().mb(px(8.)).flex().items_center().justify_between().child(Self::h("Hotkeys")).child(reset_all))
            .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child("Click + and press the new shortcut (Esc cancels). An action can have several shortcuts."));
        for g in groups {
            let mut grp = div().mb(px(12.)).child(txt(11.).mb(px(4.)).text_color(c(theme::FAINT)).child(g.to_uppercase()));
            let defs: Vec<_> = hotkey_actions().iter().filter(|a| a.group == g).collect();
            for (di, def) in defs.iter().enumerate() {
                let id = def.id;
                let list = table.get(&id).cloned().unwrap_or_default();
                let is_default = list == def.defaults;
                let rec = self.recording == Some(id);
                let mut chips: Vec<AnyElement> = Vec::new();
                for b in &list {
                    let shared: Vec<HotkeyAction> = conflicts.get(b).map(|v| v.iter().copied().filter(|x| *x != id).collect()).unwrap_or_default();
                    let warn = !shared.is_empty();
                    let (b2, t2) = (b.clone(), table.clone());
                    let tip = warn.then(|| format!("Also bound to {}", shared.iter().map(|x| action_label(*x)).collect::<Vec<_>>().join(", ")));
                    let mut chip = div()
                        .id(eid(format!("hk-{}-{}", id.id(), b)))
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .rounded(px(4.))
                        .border_1()
                        .px(px(6.))
                        .py(px(1.))
                        .text_size(px(11.))
                        .line_height(px(16.5))
                        .when(warn, |d| d.border_color(a_amber(0.6)).text_color(c(theme::AMBER_300)))
                        .when(!warn, |d| d.border_color(c(theme::LINE)).bg(c(theme::APP)))
                        .child(b.clone())
                        .child(
                            div()
                                .id(eid(format!("hk-x-{}-{}", id.id(), b)))
                                .text_color(c(theme::FAINT))
                                .hover(|s| s.text_color(c(theme::RED_400)))
                                .child(icons::icon(icons::X, 10.))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                    let mut t = t2.clone();
                                    if let Some(l) = t.get_mut(&id) {
                                        l.retain(|x| *x != b2);
                                    }
                                    this.hk_save(&t, cx);
                                })),
                        );
                    if let Some(t) = tip {
                        chip = chip.tooltip(super::ui::tip(t));
                    }
                    chips.push(chip.into_any_element());
                }
                let chip_base = || div().flex().items_center().gap(px(4.)).rounded(px(4.)).border_1().px(px(6.)).py(px(1.)).text_size(px(11.)).line_height(px(16.5));
                let adder: AnyElement = if rec {
                    chip_base().border_color(c(theme::ACCENT)).text_color(c(theme::ACCENT_SOFT)).child("Press keys…").into_any_element()
                } else {
                    chip_base()
                        .id(eid(format!("hk-add-{}", id.id())))
                        .border_dashed()
                        .border_color(c(theme::LINE))
                        .text_color(c(theme::MUTED))
                        .hover(|s| s.border_color(c(theme::ACCENT)).text_color(c(theme::TEXT)))
                        .child("+")
                        .tooltip(super::ui::tip("Add a shortcut"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.hk_pending = HkPending::default();
                            this.hk_hint.clear();
                            this.hk_set_recording(Some(id), window, cx);
                        }))
                        .into_any_element()
                };
                let defaults = def.defaults.clone();
                let reset = btn(eid(format!("hk-reset-{}", id.id())), "Reset", BtnOpts { small: true, disabled: is_default, ..Default::default() });
                let t3 = table.clone();
                let reset = if is_default {
                    reset
                } else {
                    reset.tooltip(super::ui::tip(format!("Default: {}", defaults.join(", ")))).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let mut t = t3.clone();
                        t.insert(id, defaults.clone());
                        this.hk_save(&t, cx);
                    }))
                };
                let pend = (self.hk_pending.action == Some(id)).then(|| {
                    let pb = self.hk_pending.binding.clone();
                    let other = self.hk_pending.other.unwrap_or(id);
                    let t4 = table.clone();
                    let pb2 = pb.clone();
                    div()
                        .mt(px(4.))
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(a_amber(0.4))
                        .bg(a_amber(0.1))
                        .px(px(8.))
                        .py(px(4.))
                        .text_size(px(11.))
                        .line_height(px(16.5))
                        .text_color(c(theme::AMBER_200))
                        .child(div().min_w_0().flex_1().child(format!("{pb} is already used by \"{}\".", action_label(other))))
                        .child(btn(eid("hk-move"), "Move it here", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            let mut t = t4.clone();
                            if let Some(l) = t.get_mut(&other) {
                                l.retain(|x| *x != pb2);
                            }
                            t.entry(id).or_default().push(pb2.clone());
                            this.hk_save(&t, cx);
                            this.hk_pending = HkPending::default();
                            cx.notify();
                        })))
                        .child(btn(eid("hk-pcancel"), "Cancel", BtnOpts { small: true, ..Default::default() }).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.hk_pending = HkPending::default();
                            cx.notify();
                        })))
                });
                let hint = (rec && !self.hk_hint.is_empty()).then(|| txt(11.).mt(px(4.)).pl(px(168.)).text_color(c(theme::AMBER_300)).child(self.hk_hint.clone()));
                grp = grp.child(
                    div()
                        .py(px(4.))
                        .when(di + 1 < defs.len(), |d| d.border_b_1().border_color(a(theme::LINE, 0.6)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().w(px(160.)).flex_none().child(def.label.clone()))
                                .child(
                                    div()
                                        .flex()
                                        .min_w_0()
                                        .flex_1()
                                        .flex_wrap()
                                        .items_center()
                                        .gap(px(4.))
                                        .children(chips)
                                        .children((list.is_empty() && !rec).then(|| txt(11.).text_color(c(theme::FAINT)).child("none")))
                                        .child(adder),
                                )
                                .child(reset),
                        )
                        .children(hint)
                        .children(pend),
                );
            }
            out = out.child(grp);
        }
        out.into_any_element()
    }

    // ---- mod buttons ------------------------------------------------------------------------------------------

    fn mod_buttons(&self) -> Vec<ModButton> {
        normalize_mod_buttons(&serde_json::to_value(&self.settings.mod_buttons).unwrap_or_default())
    }

    fn save_mods(&mut self, next: Vec<ModButton>, cx: &mut Context<Self>) {
        self.set(json!({ "modButtons": next }), cx);
    }

    fn patch_mod(&mut self, id: &str, f: impl FnOnce(&mut ModButton), cx: &mut Context<Self>) {
        let mut v = self.mod_buttons();
        if let Some(b) = v.iter_mut().find(|b| b.id == id) {
            f(b);
            self.save_mods(v, cx);
        }
    }

    fn mod_label_changed(&mut self, id: &str, text: &str, cx: &mut Context<Self>) {
        let label: String = text.chars().take(MAX_LABEL).collect();
        let cur = self.mod_buttons().into_iter().find(|b| b.id == id);
        let Some(cur) = cur else { return };
        if cur.label == label {
            return;
        }
        self.patch_mod(
            id,
            |b| {
                b.label = label.clone();
                if label.trim().is_empty() {
                    b.icon = if b.icon.is_empty() { default_icon(&b.action).to_string() } else { b.icon.clone() };
                }
            },
            cx,
        );
    }

    fn set_mod_action(&mut self, id: &str, action: &str, cx: &mut Context<Self>) {
        let action = action.to_string();
        self.patch_mod(
            id,
            |b| {
                // A new action gets that action's look (a "1m" label would mislabel a ban).
                b.action = action.clone();
                b.icon = default_icon(&action).to_string();
                b.label = String::new();
                if action == "timeout" && b.seconds == 0 {
                    b.seconds = 600;
                }
                if action == "command" && b.command.is_empty() {
                    b.command = "/warn {user} Please follow the chat rules".into();
                }
            },
            cx,
        );
    }

    fn commit_mod_secs(&mut self, id: &str, text: &str, cx: &mut Context<Self>) {
        let cur = self.mod_buttons().into_iter().find(|b| b.id == id);
        let Some(cur) = cur else { return };
        let initial = format_duration(cur.seconds as i64);
        match parse_duration(text) {
            None => self.set_text_of(&format!("mb.{id}.secs"), &initial, cx),
            Some(s) if text != initial => self.patch_mod(id, |b| b.seconds = s as u32, cx),
            _ => {}
        }
    }

    fn commit_mod_cmd(&mut self, id: &str, text: &str, cx: &mut Context<Self>) {
        let cur = self.mod_buttons().into_iter().find(|b| b.id == id);
        let Some(cur) = cur else { return };
        if text.trim().is_empty() {
            self.set_text_of(&format!("mb.{id}.cmd"), &cur.command, cx);
        } else if text != cur.command {
            let v: String = text.trim().chars().take(MAX_COMMAND).collect();
            self.patch_mod(id, |b| b.command = v, cx);
        }
    }

    /// `ModButtonFace`
    pub(super) fn mod_face(b: &ModButton, icon_size: f32) -> AnyElement {
        if !b.label.is_empty() {
            return div().px(px(1.)).text_size(px(0.72 * 13.)).line_height(px(0.72 * 13.)).font_weight(gpui::FontWeight::BOLD).child(b.label.clone()).into_any_element();
        }
        icons::icon(crate::chat::rows::icon_for_mod(if b.icon.is_empty() { "chat" } else { &b.icon }), icon_size).into_any_element()
    }

    fn mod_button_editor(&mut self, b: &ModButton, first: bool, last: bool, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let id = b.id.clone();
        let actions = [("ban", "Ban"), ("timeout", "Timeout"), ("warn", "Warn"), ("delete", "Delete message"), ("command", "Command")];
        let action = self.select(&format!("mb.{id}.action"), &b.action, &actions, true, cx);
        let middle: Option<AnyElement> = match b.action.as_str() {
            "timeout" => {
                let key = format!("mb.{id}.secs");
                let saved = format_duration(b.seconds as i64);
                let e = self.input(&key, "10m", InputStyle::Field, &saved, false, false, window, cx);
                let bad = parse_duration(&self.text_of(&key, cx)).is_none();
                e.update(cx, |t, cx| t.set_invalid(bad, cx));
                Some(div().flex().items_center().gap(px(8.)).child(div().w(px(80.)).flex_none().child(e)).child(txt(11.).truncate().text_color(c(theme::FAINT)).child("30s, 10m, 1h, 1d, up to 2w")).into_any_element())
            }
            "command" => {
                let key = format!("mb.{id}.cmd");
                let e = self.input(&key, "/timeout {user} 600", InputStyle::Field, &b.command, false, false, window, cx);
                let bad = self.text_of(&key, cx).trim().is_empty();
                e.update(cx, |t, cx| t.set_invalid(bad, cx));
                Some(e.into_any_element())
            }
            _ => None,
        };
        let (id1, id2, id3) = (id.clone(), id.clone(), id.clone());
        let up = self.icon_btn(format!("mb-up-{id}"), icons::CHEVRON_UP, 12., false, first).tooltip(super::ui::tip("Move up"));
        let up = if first { up } else { up.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_mod(&id1, -1, cx))) };
        let down = self.icon_btn(format!("mb-dn-{id}"), icons::CHEVRON, 12., false, last).tooltip(super::ui::tip("Move down"));
        let down = if last { down } else { down.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.move_mod(&id2, 1, cx))) };
        let rm = self.icon_btn(format!("mb-rm-{id}"), icons::X, 12., true, false).tooltip(super::ui::tip("Remove")).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            let v: Vec<_> = this.mod_buttons().into_iter().filter(|x| x.id != id3).collect();
            this.save_mods(v, cx);
        }));

        let mut icons_row = div().mt(px(4.)).flex().flex_wrap().items_center().gap(px(4.)).text_size(px(11.)).line_height(px(16.5)).text_color(c(theme::FAINT)).child(div().mr(px(2.)).child("Icon"));
        for (ic, label) in MOD_BUTTON_ICONS.iter().zip(MOD_ICON_LABELS) {
            let on = b.label.is_empty() && b.icon == *ic;
            let (bid, ic2) = (id.clone(), ic.to_string());
            icons_row = icons_row.child(
                div()
                    .id(eid(format!("mbi-{id}-{ic}")))
                    .flex()
                    .size(px(20.))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .when(on, |d| d.border_color(c(theme::ACCENT)).bg(theme::mention()).text_color(c(theme::TEXT)))
                    .when(!on, |d| d.border_color(gpui::transparent_black()).text_color(c(theme::MUTED)).hover(|s| s.bg(c(theme::RAISED_2))))
                    .tooltip(super::ui::tip(label))
                    .child(icons::icon(crate::chat::rows::icon_for_mod(ic), 12.))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let ic3 = ic2.clone();
                        this.patch_mod(&bid, |b| {
                            b.icon = ic3;
                            b.label = String::new();
                        }, cx)
                    })),
            );
        }
        let lkey = format!("mb.{id}.label");
        let le = self.input(&lkey, "1d", InputStyle::Small, &b.label, false, false, window, cx);
        icons_row = icons_row.child(div().ml(px(8.)).mr(px(2.)).child("or text")).child(div().w(px(64.)).flex_none().child(le));

        div()
            .mb(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::LINE))
            .bg(c(theme::RAISED))
            .px(px(8.))
            .py(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .h(px(24.))
                            .w(px(32.))
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .border_1()
                            .border_color(c(theme::LINE))
                            .bg(c(theme::APP))
                            .text_size(px(13.))
                            .text_color(c(theme::MUTED))
                            .child(Self::mod_face(b, 14.)),
                    )
                    .child(action)
                    .child(div().min_w_0().flex_1().children(middle))
                    .child(up)
                    .child(down)
                    .child(rm),
            )
            .child(icons_row)
            .children((b.action == "command").then(|| txt(11.).mt(px(4.)).text_color(c(theme::FAINT)).child(format!("A slash command or a chat message. Placeholders: {TEMPLATE_HELP}."))))
            .into_any_element()
    }

    fn move_mod(&mut self, id: &str, d: i32, cx: &mut Context<Self>) {
        let mut v = self.mod_buttons();
        let Some(i) = v.iter().position(|b| b.id == id) else { return };
        let j = i as i32 + d;
        if j < 0 || j as usize >= v.len() {
            return;
        }
        v.swap(i, j as usize);
        self.save_mods(v, cx);
    }

    fn render_mod_buttons(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let buttons = self.mod_buttons();
        let is_default = serde_json::to_value(&buttons).ok() == serde_json::to_value(default_mod_buttons()).ok();
        let reset = btn(eid("mb-reset"), "Reset to defaults", BtnOpts { small: true, disabled: is_default, ..Default::default() });
        let reset = if is_default { reset } else { reset.on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.save_mods(default_mod_buttons(), cx))) };
        let preview = Self::card()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .text_size(px(13.))
                    .line_height(px(19.5))
                    .child(div().flex().items_center().gap(px(1.)).children(buttons.iter().map(|b| {
                        div()
                            .id(eid(format!("pv-{}", b.id)))
                            .flex()
                            .h(px(16.))
                            .min_w(px(16.))
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_color(c(theme::FAINT))
                            .tooltip(super::ui::tip(describe_mod_button(b)))
                            .child(Self::mod_face(b, 12.))
                    })))
                    .child(div().font_weight(gpui::FontWeight::SEMIBOLD).text_color(c(theme::ACCENT_SOFT)).child("viewer"))
                    .child(div().text_color(c(theme::MUTED)).child(":"))
                    .child(div().child("example message")),
            )
            .children(buttons.is_empty().then(|| txt(11.).mt(px(4.)).text_color(c(theme::FAINT)).child("No buttons: rows show none in mod mode.")));
        let n = buttons.len();
        let editors: Vec<AnyElement> = buttons.iter().enumerate().map(|(i, b)| self.mod_button_editor(b, i == 0, i + 1 == n, window, cx)).collect();
        let full = n >= MAX_MOD_BUTTONS;
        let add = btn(eid("mb-add"), "Add button", BtnOpts { disabled: full, ..Default::default() });
        let add = if full {
            add
        } else {
            add.tooltip(super::ui::tip("Adds a 1 minute timeout; change it below")).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let mut v = this.mod_buttons();
                v.push(ModButton { id: new_mod_button_id(), action: "timeout".into(), icon: "clock".into(), label: "1m".into(), seconds: 60, command: String::new() });
                this.save_mods(v, cx);
            }))
        };
        div()
            .child(div().mb(px(8.)).flex().items_center().justify_between().child(Self::h("Mod buttons")).child(reset))
            .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child("Shown in this order at the start of every message when a split's mod mode is on. Ban, timeout and delete work wherever you can moderate; commands run in Twitch chats."))
            .child(preview)
            .children(editors)
            .child(div().flex().child(add))
            .into_any_element()
    }

    // ---- updates ------------------------------------------------------------------------------------------------

    fn render_updates(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let u = self.updater.clone();
        let busy = u.status == "checking" || u.status == "downloading";
        let channel = self.settings.update_channel.clone();
        let cards = ["beta", "stable"].map(|ch| {
            let on = channel == ch;
            div()
                .id(eid(format!("ch-{ch}")))
                .flex_1()
                .rounded(px(4.))
                .border_1()
                .p(px(8.))
                .when(on, |d| d.border_color(c(theme::ACCENT)).bg(theme::mention()))
                .when(!on, |d| d.border_color(c(theme::LINE)).hover(|s| s.bg(c(theme::RAISED_2))))
                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(if ch == "beta" { "Beta" } else { "Stable" }))
                .child(txt(11.).text_color(c(theme::MUTED)).child(if ch == "beta" { "Prereleases: the newest features, with rough edges." } else { "Full releases only: fewer updates, better tested." }))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set(json!({ "updateChannel": ch }), cx)))
        });
        let [beta, stable] = cards;
        let check = btn(eid("up-check"), "Check now", BtnOpts { disabled: busy, ..Default::default() });
        let check = if busy {
            check
        } else {
            check.on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let fut = this.bridge.updater_check();
                cx.spawn(async move |_this, _cx| fut.await).detach();
            }))
        };
        let can_install = u.status == "available" || u.status == "ready";
        let install = can_install.then(|| {
            btn(eid("up-install"), "Restart and update", BtnOpts::primary()).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                let fut = this.bridge.updater_install();
                cx.spawn(async move |_this, _cx| fut.await).detach();
            }))
        });
        let pct = percent_of(u.progress);
        let err = self.err.get("updater").cloned().unwrap_or_default();
        div()
            .child(Self::h("Updates"))
            .child(
                Self::card()
                    .child(txt(11.).mb(px(8.)).text_color(c(theme::FAINT)).child("UPDATE CHANNEL"))
                    .child(div().flex().gap(px(8.)).child(beta).child(stable)),
            )
            .child(
                Self::card()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .child(
                                div()
                                    .child(
                                        div()
                                            .flex()
                                            .child("Current version ")
                                            .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(self.info.version.clone()))
                                            .children((!self.info.packaged).then(|| div().text_color(c(theme::FAINT)).child(" (development build)"))),
                                    )
                                    .child(div().mt(px(2.)).text_color(c(theme::MUTED)).child(updater_text(&u))),
                            )
                            .child(div().flex().flex_none().gap(px(8.)).child(check).children(install)),
                    )
                    .children((u.status == "downloading").then(|| {
                        div().mt(px(8.)).h(px(6.)).overflow_hidden().rounded(px(4.)).bg(c(theme::APP)).child(div().h_full().w(gpui::relative(pct.clamp(2., 100.) / 100.)).bg(c(theme::ACCENT)))
                    }))
                    .children((!u.notes.is_empty()).then(|| {
                        div().mt(px(8.)).pl(px(20.)).flex().flex_col().gap(px(2.)).children(u.notes.iter().map(|n| {
                            txt(11.).flex().text_color(c(theme::MUTED)).child(div().w(px(12.)).flex_none().child("\u{2022}")).child(div().min_w_0().child(n.clone()))
                        }))
                    }))
                    .children((!err.is_empty()).then(|| Self::err_line(&err))),
            )
            .into_any_element()
    }

    // ---- about ----------------------------------------------------------------------------------------------------

    fn render_about(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let link = self.link("about-repo", REPO_URL, REPO_URL, cx);
        div()
            .child(Self::h("About"))
            .child(
                Self::card()
                    .child(txt(16.).font_weight(gpui::FontWeight::BOLD).child("Agu Chat"))
                    .child(div().text_color(c(theme::MUTED)).child(format!("Version {}", self.info.version)))
                    .child(div().mt(px(8.)).text_color(c(theme::MUTED)).child("Live chat for Twitch, Kick, YouTube, Rumble and X in one window."))
                    .child(div().mt(px(12.)).flex().child(link)),
            )
            .into_any_element()
    }
}

const MOD_ICON_LABELS: [&str; 10] = ["Ban", "Clock", "Trash", "Shield", "Warning", "Flag", "Hidden", "Chat", "Pin", "Check"];

fn a_amber(alpha: f32) -> gpui::Rgba {
    a(theme::AMBER_500, alpha)
}

/// Progress as a percentage. The backend reports 0..1 (updater.js); the dev mock reports 0..100. The JS dialog
/// printed `Math.round(progress)` and so showed 0-1% for a real download; this shows the real percentage.
fn percent_of(p: f32) -> f32 {
    if p <= 1.0 {
        p * 100.0
    } else {
        p
    }
}

/// `updaterText`
pub fn updater_text(u: &UpdaterState) -> String {
    let v = |d: &str| u.version.clone().unwrap_or_else(|| d.to_string());
    match u.status.as_str() {
        "idle" => "Not checked yet.".into(),
        "dev" => "Updates are off in development builds.".into(),
        "checking" => "Checking for updates…".into(),
        "none" => "You are on the latest version.".into(),
        "available" => format!("Version {} is available.", v("?")),
        "downloading" => format!("Downloading {}… {}%", v("the update"), percent_of(u.progress).round() as i64),
        "ready" => format!("Version {} is ready to install.", v("?")),
        "error" => format!("Update check failed{}", u.error.as_ref().map(|e| format!(": {e}")).unwrap_or_else(|| ".".into())),
        _ => String::new(),
    }
}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let vp = window.viewport_size();
        let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
        let narrow = vw < 640.;
        let height = (vh * 0.72).min(540.);
        let section = self.section;
        // Thin Chatterino-style scrollbar (index.css): 8px, thumb #2c2c36, radius 4
        let (scroll_max, view_h, off) = (f32::from(self.scroll.max_offset().height), f32::from(self.scroll.bounds().size.height), -f32::from(self.scroll.offset().y));
        let thumb = (scroll_max > 0.).then(|| {
            let total = view_h + scroll_max;
            let th = (view_h * view_h / total).max(20.);
            let y = (off / scroll_max).clamp(0., 1.) * (view_h - th);
            div().absolute().top(px(y)).right_0().w(px(8.)).h(px(th)).rounded(px(4.)).bg(c(theme::SCROLLBAR_THUMB))
        });

        let nav = SECTIONS.iter().map(|(s, label)| {
            let on = *s == section;
            let s = *s;
            div()
                .id(eid(format!("nav-{label}")))
                .w_full()
                .px(px(12.))
                .py(px(6.))
                .border_l_2()
                .when(narrow, |d| d.w_auto().px(px(10.)).border_l_0().border_b_2())
                .when(on, |d| d.border_color(c(theme::ACCENT)).bg(c(theme::RAISED)).text_color(c(theme::TEXT)))
                .when(!on, |d| d.border_color(gpui::transparent_black()).text_color(c(theme::MUTED)).hover(|st| st.bg(c(theme::RAISED)).text_color(c(theme::TEXT))))
                .child(label.to_string())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.set_section(s, cx)))
        }).collect::<Vec<_>>();

        let content: AnyElement = match section {
            Section::Accounts => self.render_accounts(window, cx),
            Section::Appearance => self.render_appearance(cx),
            Section::Highlights => self.render_highlights(window, cx),
            Section::Ignores => self.render_ignores(window, cx),
            Section::Hotkeys => self.render_hotkeys(cx),
            Section::ModButtons => self.render_mod_buttons(window, cx),
            Section::Updates => self.render_updates(cx),
            Section::About => self.render_about(cx),
        };

        let body = div()
            .id("settings-body")
            .key_context("SettingsDialog")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_record_key(ev, window, cx)))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.drag.is_some() {
                    this.slider_to(ev.position.x, cx);
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None))
            .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _: &MouseUpEvent, _, _| this.drag = None))
            .flex()
            .when(narrow, |d| d.flex_col())
            .h(px(height))
            .font_family(theme::FONT)
            .text_size(px(12.))
            .line_height(px(18.))
            .text_color(c(theme::TEXT))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .when(!narrow, |d| d.flex_col().w(px(160.)).border_r_1().py(px(8.)))
                    .when(narrow, |d| d.flex_wrap().w_full().border_b_1())
                    .border_color(c(theme::LINE))
                    .bg(c(theme::APP))
                    .children(nav),
            )
            .child(
                div()
                    .relative()
                    .min_h_0()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .id("settings-scroll")
                            .track_scroll(&self.scroll)
                            .size_full()
                            .overflow_y_scroll()
                            .pl(px(if narrow { 12. } else { 16. }))
                            .pr(px(if narrow { 12. } else { 16. } + if scroll_max > 0. { 8. } else { 0. }))
                            .py(px(if narrow { 12. } else { 16. }))
                            .child(content),
                    )
                    .children(thumb),
            );

        let me = cx.entity();
        let close: CloseFn = Rc::new(move |_w, cx| me.update(cx, |_, cx| cx.emit(DialogEvent::Close)));
        modal("Settings", 780., close, body, None)
    }
}

#[allow(dead_code)]
fn _unused(_: SharedString) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(status: &str) -> UpdaterState {
        UpdaterState { status: status.into(), channel: "beta".into(), version: Some("0.1.0".into()), notes: vec![], progress: 0.0, error: None }
    }

    #[test]
    fn updater_text_matches_the_electron_dialog() {
        assert_eq!(updater_text(&st("idle")), "Not checked yet.");
        assert_eq!(updater_text(&st("dev")), "Updates are off in development builds.");
        assert_eq!(updater_text(&st("checking")), "Checking for updates…");
        assert_eq!(updater_text(&st("none")), "You are on the latest version.");
        assert_eq!(updater_text(&st("available")), "Version 0.1.0 is available.");
        assert_eq!(updater_text(&st("ready")), "Version 0.1.0 is ready to install.");
        let mut u = st("error");
        assert_eq!(updater_text(&u), "Update check failed.");
        u.error = Some("Could not reach GitHub.".into());
        assert_eq!(updater_text(&u), "Update check failed: Could not reach GitHub.");
        let mut d = st("downloading");
        d.progress = 0.42; // the backend reports 0..1
        assert_eq!(updater_text(&d), "Downloading 0.1.0… 42%");
        d.progress = 60.0; // the dev mock reports percent
        assert_eq!(updater_text(&d), "Downloading 0.1.0… 60%");
        d.version = None;
        assert_eq!(updater_text(&d), "Downloading the update… 60%");
    }

    #[test]
    fn section_names_for_the_screenshot_aid() {
        assert_eq!(Section::from_name("modButtons"), Some(Section::ModButtons));
        assert_eq!(Section::from_name("highlights"), Some(Section::Highlights));
        assert_eq!(Section::from_name("nope"), None);
    }

    #[test]
    fn a_patch_replaces_only_its_keys() {
        let mut cur = json!({ "fontSize": 13, "timestamps": false });
        merge(&mut cur, &json!({ "fontSize": 15 }));
        assert_eq!(cur, json!({ "fontSize": 15, "timestamps": false }));
        // and a typed Settings survives the round trip the optimistic update does
        let mut v = serde_json::to_value(Settings::default()).unwrap();
        merge(&mut v, &json!({ "updateChannel": "stable", "highlightVolume": 35 }));
        let s: Settings = serde_json::from_value(v).unwrap();
        assert_eq!((s.update_channel.as_str(), s.highlight_volume), ("stable", 35));
    }
}
