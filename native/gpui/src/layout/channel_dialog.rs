//! Port of src/renderer/layout/ChannelDialog.tsx.
//! "Select channel": pick one or several sources for a split (several = merged co-chat).
use std::rc::Rc;

use agu_chat_core::protocol::{Platform, SourceRef};
use gpui::{div, prelude::*, px, ClickEvent, Context, Entity, EventEmitter, FocusHandle, Subscription, Window};

use crate::icons;
use crate::layout::tree::{short_name, source_key};
use crate::layout::ui::{btn, modal, BtnOpts};
use crate::text_input::{InputStyle, TextInput, TextInputEvent};
use crate::theme::{self, a, c};

fn hint(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "Twitch login, e.g. a_g_u__",
        Platform::Kick => "Kick channel slug, e.g. trainwreckstv",
        Platform::Youtube => "@handle, channel URL or video URL",
        Platform::Rumble => "Channel name or video URL",
        Platform::X => "Username or broadcast URL",
    }
}

fn placeholder(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "channel login",
        Platform::Kick => "channel slug",
        Platform::Youtube => "@handle or youtube.com/...",
        Platform::Rumble => "channel or rumble.com/... URL",
        Platform::X => "@username or x.com/i/broadcasts/...",
    }
}

fn last_segment(input: &str) -> String {
    let rest = input.strip_prefix("https://").or_else(|| input.strip_prefix("http://")).unwrap_or(input);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let path = rest.split_once('/').map(|(_, p)| p).unwrap_or("");
    path.split('/').filter(|p| !p.is_empty()).last().unwrap_or("").to_string()
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(needle)
}

/// Light client-side tidy-up; the platform module in main does the real normalisation.
fn clean(platform: Platform, raw: &str) -> Result<String, String> {
    let mut cch = raw.trim().to_string();
    if cch.is_empty() {
        return Err("Enter a channel first.".into());
    }
    match platform {
        Platform::Twitch => {
            if contains_ci(&cch, "twitch.tv/") {
                cch = last_segment(&cch);
            }
            cch = cch.trim_start_matches(['@', '#']).to_lowercase();
            if !(1..=25).contains(&cch.len()) || !cch.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_') {
                return Err("A Twitch login is letters, digits and underscores (max 25).".into());
            }
        }
        Platform::Kick => {
            if contains_ci(&cch, "kick.com/") {
                cch = last_segment(&cch);
            }
            cch = cch.trim_start_matches('@').to_lowercase();
            if !(1..=40).contains(&cch.len()) || !cch.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-') {
                return Err("A Kick slug is letters, digits, dashes and underscores.".into());
            }
        }
        Platform::X => {
            let l = cch.to_lowercase();
            if !(l.starts_with("http://") || l.starts_with("https://")) && !(l.contains("x.com/") || l.contains("twitter.com/")) {
                cch = cch.trim_start_matches('@').to_string();
            }
            if cch.is_empty() {
                return Err("Enter a username or broadcast URL.".into());
            }
        }
        _ => {}
    }
    Ok(cch)
}

pub enum DialogEvent {
    Submit(Vec<SourceRef>),
    Close,
}

impl EventEmitter<DialogEvent> for ChannelDialog {}

pub struct ChannelDialog {
    title: String,
    submit_label: String,
    platform: Platform,
    input: Entity<TextInput>,
    sources: Vec<SourceRef>,
    error: String,
    _sub: Subscription,
}

impl ChannelDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>, title: &str, initial: Vec<SourceRef>, submit_label: &str) -> Self {
        let platform = initial.first().map(|s| s.platform).unwrap_or(Platform::Twitch);
        let input = cx.new(|cx| TextInput::new(window, cx, placeholder(platform), InputStyle::Field));
        let sub = cx.subscribe(&input, |this, _input, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Changed => {
                this.error.clear();
                cx.notify();
            }
            TextInputEvent::Enter => this.submit(cx),
            _ => {}
        });
        input.update(cx, |i, _| i.focus(window));
        ChannelDialog { title: title.into(), submit_label: submit_label.into(), platform, input, sources: initial, error: String::new(), _sub: sub }
    }

    pub fn focus_input(&self, window: &mut Window, cx: &gpui::App) {
        self.input.read(cx).focus(window);
    }

    /// Adds the typed channel to the list; returns the new list, or None when the input was bad.
    fn add_typed(&mut self, cx: &mut Context<Self>) -> Option<Vec<SourceRef>> {
        let text = self.input.read(cx).text();
        if text.trim().is_empty() {
            return Some(self.sources.clone());
        }
        match clean(self.platform, &text) {
            Err(e) => {
                self.error = e;
                cx.notify();
                None
            }
            Ok(channel) => {
                let r = SourceRef { platform: self.platform, channel };
                let key = source_key(&r).to_lowercase();
                if !self.sources.iter().any(|s| source_key(s).to_lowercase() == key) {
                    self.sources.push(r);
                }
                self.input.update(cx, |i, cx| i.set_text("", cx));
                self.error.clear();
                cx.notify();
                Some(self.sources.clone())
            }
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(next) = self.add_typed(cx) else { return };
        if next.is_empty() {
            self.error = "Add at least one channel.".into();
            cx.notify();
            return;
        }
        cx.emit(DialogEvent::Submit(next));
    }

    fn label(text: String) -> impl IntoElement {
        div().mb(px(4.)).text_size(px(11.)).text_color(c(theme::FAINT)).child(text.to_uppercase())
    }
}

impl gpui::Focusable for ChannelDialog {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        gpui::Focusable::focus_handle(self.input.read(cx), cx)
    }
}

impl Render for ChannelDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let typed_empty = self.input.read(cx).text().trim().is_empty();

        let platforms = div().flex().flex_wrap().gap(px(4.)).children(Platform::ALL.iter().map(|&p| {
            let selected = self.platform == p;
            div()
                .id(("platform", p as usize))
                .flex()
                .items_center()
                .gap(px(6.))
                .rounded(px(4.))
                .border_1()
                .px(px(8.))
                .py(px(4.))
                .text_size(px(12.))
                .when(selected, |d| d.border_color(c(theme::ACCENT)).bg(theme::mention()).text_color(c(theme::TEXT)))
                .when(!selected, |d| d.border_color(c(theme::LINE)).text_color(c(theme::MUTED)).hover(|s| s.bg(c(theme::RAISED_2))))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.platform = p;
                    this.error.clear();
                    this.input.update(cx, |i, cx| {
                        i.set_placeholder(placeholder(p), cx);
                        i.focus(window);
                    });
                    cx.notify();
                }))
                .child(icons::platform_icon(p, 14., true))
                .child(icons::platform_label(p))
        }));

        let in_split = if self.sources.is_empty() {
            div().text_size(px(12.)).text_color(c(theme::FAINT)).child("Nothing yet. Add several channels to merge their chats into one split.").into_any_element()
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(self.sources.iter().enumerate().map(|(i, s)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .rounded(px(4.))
                        .border_1()
                        .border_color(c(theme::LINE))
                        .bg(c(theme::APP))
                        .py(px(2.))
                        .pl(px(6.))
                        .pr(px(4.))
                        .text_size(px(12.))
                        .child(icons::platform_icon(s.platform, 14., true))
                        .child(div().max_w(px(180.)).truncate().child(short_name(s)))
                        .child(
                            div()
                                .id(("rm", i))
                                .rounded(px(4.))
                                .p(px(2.))
                                .text_color(c(theme::FAINT))
                                .hover(|st| st.bg(c(theme::RAISED_2)).text_color(c(theme::TEXT)))
                                .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                                    if i < this.sources.len() {
                                        this.sources.remove(i);
                                    }
                                    cx.notify();
                                }))
                                .child(icons::icon(icons::X, 12.)),
                        )
                }))
                .into_any_element()
        };

        let body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(12.))
            .child(div().child(Self::label("Platform".into())).child(platforms))
            .child(
                div()
                    .child(Self::label("Channel".into()))
                    .child(
                        div().flex().gap(px(6.)).child(div().flex_1().min_w_0().child(self.input.clone())).child(
                            btn("add-typed", "Add", BtnOpts { disabled: typed_empty, ..Default::default() }).when(!typed_empty, |b| {
                                b.on_click(cx.listener(|this, _: &ClickEvent, _w, cx| {
                                    this.add_typed(cx);
                                }))
                            }),
                        ),
                    )
                    .child(div().mt(px(4.)).text_size(px(11.)).text_color(c(theme::FAINT)).child(hint(self.platform)))
                    .when(!self.error.is_empty(), |d| d.child(div().mt(px(4.)).text_size(px(11.)).text_color(c(theme::RED_400)).child(self.error.clone()))),
            )
            .child(
                div()
                    .child(Self::label(format!("In this split{}", if self.sources.len() > 1 { " (merged co-chat)" } else { "" })))
                    .child(in_split),
            );

        let close = cx.listener(|_, _: &(), _w, cx| cx.emit(DialogEvent::Close));
        let close2 = cx.listener(|_, _: &ClickEvent, _w, cx| cx.emit(DialogEvent::Close));
        let submit = cx.listener(|this, _: &ClickEvent, _w, cx| this.submit(cx));
        let footer = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(btn("cancel", "Cancel", BtnOpts::default()).on_click(close2))
            .child(btn("submit", self.submit_label.clone(), BtnOpts::primary()).on_click(submit))
            .into_any_element();

        let close_fn: Rc<dyn Fn(&mut Window, &mut gpui::App)> = Rc::new(move |w, cx| close(&(), w, cx));
        modal(&self.title, 460., close_fn, body, Some(footer))
    }
}

#[allow(dead_code)]
fn _unused(_: gpui::Rgba) {
    let _ = a;
}
