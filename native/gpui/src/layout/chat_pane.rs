//! Placeholder for src/renderer/chat/ChatPane.tsx. Round 2 ports the real chat view (rows, emotes, input...);
//! until then a split's body is a minimal list of the incoming chat frames in the right font and colors.
//! The contract `layout/` relies on is already the real one: props (`sources`, `mod_mode`), `onStatus` (here
//! `status` + `PaneEvent::Status`) and `onActivity` (`PaneEvent::Activity`).
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use agu_chat_core::protocol::{AguBridge, ConnFrame, Frame, SourceRef, SubId, Unsubscribe};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{div, prelude::*, px, Context, EventEmitter, Rgba, SharedString, Window};

use crate::layout::tree::source_key;
use crate::theme::{self, c};

/// Tabs that got live chat while not selected: "message" (a subtle marker) or "highlight" (a strong one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Message,
    Highlight,
}

pub enum PaneEvent {
    /// A source's `conn` state changed (the split header shows it).
    Status,
    /// Live chat arrived (drives the tab's unread / highlight marker).
    Activity(Activity),
}

impl EventEmitter<PaneEvent> for ChatPane {}

struct Line {
    name: Option<SharedString>,
    color: Option<u32>,
    text: SharedString,
}

#[derive(Default)]
struct Routes {
    /// sub id -> source key
    subs: HashMap<SubId, String>,
    /// frames that arrived before `subscribe` resolved with their id
    pending: Vec<(SubId, Frame)>,
    awaiting: usize,
}

pub struct ChatPane {
    pub sources: Vec<SourceRef>,
    pub mod_mode: bool,
    /// Connection state per source (`source_key`), from the uniform `conn` frame.
    pub status: HashMap<String, ConnFrame>,
    lines: VecDeque<Line>,
    font_size: f32,
    alternate_rows: bool,
    routes: Arc<Mutex<Routes>>,
    unsub_frames: Option<Unsubscribe>,
    _task: gpui::Task<()>,
}

const MAX_LINES: usize = 300;

impl ChatPane {
    pub fn new(sources: Vec<SourceRef>, mod_mode: bool, bridge: Arc<dyn AguBridge>, font_size: f32, alternate_rows: bool, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = unbounded::<(String, Frame)>();
        let routes = Arc::new(Mutex::new(Routes { awaiting: sources.len(), ..Default::default() }));

        let unsub_frames = {
            let routes = routes.clone();
            let tx: UnboundedSender<(String, Frame)> = tx.clone();
            bridge.on_frame(Arc::new(move |sub, frame| {
                let mut r = routes.lock().unwrap();
                if let Some(k) = r.subs.get(sub) {
                    let _ = tx.unbounded_send((k.clone(), frame.clone()));
                } else if r.awaiting > 0 && r.pending.len() < 512 {
                    r.pending.push((sub.clone(), frame.clone()));
                }
            }))
        };

        let task = {
            let bridge = bridge.clone();
            let routes = routes.clone();
            let srcs = sources.clone();
            cx.spawn(async move |this, cx| {
                // Subscribe to every source; frames route to this pane by sub id.
                for s in srcs {
                    let key = source_key(&s);
                    let id = bridge.subscribe(s).await;
                    let mut r = routes.lock().unwrap();
                    r.awaiting = r.awaiting.saturating_sub(1);
                    if let Ok(id) = id {
                        r.subs.insert(id.clone(), key.clone());
                        let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut r.pending).into_iter().partition(|(sid, _)| *sid == id);
                        r.pending = rest;
                        for (_, f) in mine {
                            let _ = tx.unbounded_send((key.clone(), f));
                        }
                    }
                    if r.awaiting == 0 {
                        r.pending.clear();
                    }
                }
                while let Some((key, frame)) = rx.next().await {
                    if this.update(cx, |p, cx| p.on_frame(&key, &frame, cx)).is_err() {
                        break;
                    }
                }
            })
        };

        // Unsubscribe when the pane goes away (a split closed, or its channels changed).
        {
            let bridge = bridge.clone();
            let routes = routes.clone();
            cx.on_release(move |this, cx| {
                if let Some(u) = this.unsub_frames.take() {
                    u();
                }
                let ids: Vec<SubId> = routes.lock().unwrap().subs.keys().cloned().collect();
                for id in ids {
                    cx.background_spawn(bridge.unsubscribe(id)).detach();
                }
            })
            .detach();
        }

        ChatPane { sources, mod_mode, status: HashMap::new(), lines: VecDeque::new(), font_size, alternate_rows, routes, unsub_frames: Some(unsub_frames), _task: task }
    }

    pub fn set_display(&mut self, font_size: f32, alternate_rows: bool, cx: &mut Context<Self>) {
        if self.font_size != font_size || self.alternate_rows != alternate_rows {
            self.font_size = font_size;
            self.alternate_rows = alternate_rows;
            cx.notify();
        }
    }

    fn push(&mut self, l: Line) {
        self.lines.push_back(l);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    fn on_frame(&mut self, key: &str, frame: &Frame, cx: &mut Context<Self>) {
        let _ = &self.routes;
        match frame.get("type").and_then(|t| t.as_str()) {
            Some("conn") => {
                if let Ok(cf) = serde_json::from_value::<ConnFrame>(frame.clone()) {
                    let changed = self.status.get(key).map(|old| (&old.state, &old.live, &old.title, &old.detail) != (&cf.state, &cf.live, &cf.title, &cf.detail)).unwrap_or(true);
                    self.status.insert(key.to_string(), cf);
                    if changed {
                        cx.emit(PaneEvent::Status);
                    }
                }
            }
            Some("chat") => {
                self.push(chat_line(frame));
                cx.emit(PaneEvent::Activity(Activity::Message));
                cx.notify();
            }
            Some("chatlog") => {
                if let Some(ms) = frame.get("messages").and_then(|m| m.as_array()) {
                    for m in ms {
                        self.push(chat_line(m));
                    }
                    cx.notify();
                }
            }
            Some("chatnotice") => {
                let text = frame.get("text").or_else(|| frame.get("message")).and_then(|t| t.as_str()).unwrap_or("").to_string();
                if !text.is_empty() {
                    self.push(Line { name: None, color: None, text: text.into() });
                    cx.notify();
                }
            }
            _ => {}
        }
    }
}

fn chat_line(m: &Frame) -> Line {
    let user = m.get("user");
    let name = user.and_then(|u| u.get("name")).or_else(|| m.get("name")).or_else(|| m.get("user").filter(|u| u.is_string())).and_then(|n| n.as_str());
    let color = user
        .and_then(|u| u.get("color"))
        .or_else(|| m.get("color"))
        .and_then(|c| c.as_str())
        .and_then(|c| u32::from_str_radix(c.trim_start_matches('#'), 16).ok());
    let text = m.get("text").or_else(|| m.get("message")).and_then(|t| t.as_str()).unwrap_or("");
    Line { name: name.map(|n| n.to_string().into()), color, text: text.to_string().into() }
}

impl gpui::Render for ChatPane {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .justify_end()
            .overflow_hidden()
            .bg(c(theme::PANEL))
            .text_size(px(self.font_size))
            .text_color(c(theme::TEXT))
            .children(self.lines.iter().enumerate().map(|(i, l)| {
                let mut row = div().flex_none().w_full().px(px(8.)).py(px(2.));
                if self.alternate_rows && i % 2 == 1 {
                    row = row.bg(c(theme::PANEL_ALT));
                }
                row.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .children(l.name.clone().map(|n| {
                            div()
                                .font_weight(gpui::FontWeight::BOLD)
                                .text_color(l.color.map(c).unwrap_or(Rgba::from(c(theme::MUTED))))
                                .child(format!("{n}: "))
                        }))
                        .child(l.text.clone()),
                )
            }))
    }
}
