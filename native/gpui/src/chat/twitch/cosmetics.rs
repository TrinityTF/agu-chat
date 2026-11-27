//! The desktop's side of src/renderer/chat/twitch/cosmetics.ts: `use7TvStyle` (a React hook) becomes
//! `Cosmetics::style_of`, which answers from the cache and starts a background fetch (at most 3 in flight) for an
//! unknown chatter; `on_style` listeners then refresh the rows. The definitions, the lookups and the paint maths are
//! in agu-chat-model (shared with Android).
//!
//! A painted name is drawn whole by `chat/paint_text.rs` (DirectWrite/Direct2D mask filled with the CSS
//! gradient / image and the paint's drop-shadows); `paint_color_at` (per character colours) remains as the fallback
//! and for the /me message body.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

pub use agu_chat_model::twitch::cosmetics::*;

use crate::chat::net::Net;

type StyleListener = Box<dyn Fn(&str)>;

/// UI-thread state of all 7TV cosmetics lookups.
pub struct CosmeticsState {
    pub defs: Option<Arc<SevenTvDefs>>,
    styles: HashMap<String, Option<ChatterStyle>>,
    pending: HashSet<String>,
    queue: VecDeque<String>,
    active: usize,
    listeners: Vec<StyleListener>,
    defs_requested: bool,
    /// failed lookups so far, per chatter (cleared on an answer); `defs_failures` for the definitions
    failures: HashMap<String, u32>,
    defs_failures: u32,
}

#[derive(Clone)]
pub struct Cosmetics(pub Rc<RefCell<CosmeticsState>>);

pub enum StyleLookup {
    Unknown,
    Known(Option<ChatterStyle>),
}

impl Cosmetics {
    pub fn new() -> Cosmetics {
        // dev: `AGU_PAINT_FIXTURE=file.json` ({"defs": {paints}, "users": [{id, name, paint_id}]}) stands in for 7TV
        let fixture = paint_fixture();
        let mut styles = HashMap::new();
        if let Some(f) = &fixture {
            for u in f.get("users").and_then(|u| u.as_array()).into_iter().flatten() {
                let id = u.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let paint_id = u.get("paint_id").and_then(|v| v.as_str()).map(|s| s.to_string());
                styles.insert(id, Some(ChatterStyle { color: None, paint_id, badge_id: None, personal: None }));
            }
        }
        Cosmetics(Rc::new(RefCell::new(CosmeticsState {
            defs: fixture.as_ref().and_then(|f| f.get("defs")).map(|d| Arc::new(defs_from_raw(d))),
            styles,
            pending: HashSet::new(),
            queue: VecDeque::new(),
            active: 0,
            listeners: Vec::new(),
            defs_requested: false,
            failures: HashMap::new(),
            defs_failures: 0,
        })))
    }

    pub fn on_style(&self, f: impl Fn(&str) + 'static) {
        self.0.borrow_mut().listeners.push(Box::new(f));
    }

    pub fn defs(&self) -> Option<Arc<SevenTvDefs>> {
        self.0.borrow().defs.clone()
    }

    /// Cached style of a chatter; an unknown one is queued for fetching (the listeners hear about the answer).
    pub fn style_of(&self, user_id: &str, net: &Arc<Net>, cx: &mut gpui::App) -> StyleLookup {
        if user_id.is_empty() {
            return StyleLookup::Known(None);
        }
        {
            let s = self.0.borrow();
            if let Some(v) = s.styles.get(user_id) {
                return StyleLookup::Known(v.clone());
            }
            if s.pending.contains(user_id) {
                return StyleLookup::Unknown;
            }
        }
        self.0.borrow_mut().pending.insert(user_id.to_string());
        self.0.borrow_mut().queue.push_back(user_id.to_string());
        self.pump(net, cx);
        StyleLookup::Unknown
    }

    /// The listeners hear this id when the definitions arrive: every painted row has to be redrawn.
    pub const DEFS_ARRIVED: &'static str = "";

    /// Start the fetch of the paint / badge definitions; a failed fetch is retried (`retry_delay`).
    pub fn ensure_defs(&self, net: &Arc<Net>, cx: &mut gpui::App) {
        if std::mem::replace(&mut self.0.borrow_mut().defs_requested, true) || self.0.borrow().defs.is_some() {
            return;
        }
        let this = self.clone();
        let netc = net.clone();
        let client = net.client.clone();
        let task = net.run(async move { fetch_seven_tv_defs(&client).await.map(|d| defs_from_raw(&d)) });
        cx.spawn(async move |cx| {
            if let Some(Some(defs)) = task.await {
                let _ = cx.update(|cx| {
                    this.0.borrow_mut().defs = Some(Arc::new(defs));
                    this.notify(Self::DEFS_ARRIVED);
                    cx.refresh_windows();
                });
                return;
            }
            let wait = {
                let mut s = this.0.borrow_mut();
                s.defs_failures += 1;
                retry_delay(s.defs_failures)
            };
            cx.background_executor().timer(wait).await;
            let _ = cx.update(|cx| {
                this.0.borrow_mut().defs_requested = false;
                this.ensure_defs(&netc, cx);
            });
        })
        .detach();
    }

    fn notify(&self, uid: &str) {
        let st = self.0.borrow();
        for l in st.listeners.iter() {
            l(uid);
        }
    }

    fn pump(&self, net: &Arc<Net>, cx: &mut gpui::App) {
        while self.0.borrow().active < 3 {
            let Some(uid) = self.0.borrow_mut().queue.pop_front() else { break };
            self.0.borrow_mut().active += 1;
            let this = self.clone();
            let netc = net.clone();
            let client = net.client.clone();
            let id = uid.clone();
            let task = net.run(async move { fetch_chatter_style(&client, &id).await });
            cx.spawn(async move |cx| {
                // An answer (a style, or none: those are cached so repeat lookups don't refire) is kept; a
                // failed lookup stays pending and is queued again after a wait, so a network blip doesn't
                // take a chatter's paint and badge away for the whole session.
                match task.await.flatten() {
                    Some(style) => {
                        let _ = cx.update(|cx| {
                            {
                                let mut s = this.0.borrow_mut();
                                s.active -= 1;
                                s.pending.remove(&uid);
                                s.failures.remove(&uid);
                                s.styles.insert(uid.clone(), style);
                            }
                            this.notify(&uid);
                            this.pump(&netc, cx);
                            cx.refresh_windows();
                        });
                    }
                    None => {
                        let wait = {
                            let mut s = this.0.borrow_mut();
                            s.active -= 1;
                            let n = s.failures.entry(uid.clone()).or_insert(0);
                            *n += 1;
                            retry_delay(*n)
                        };
                        let _ = cx.update(|cx| this.pump(&netc, cx));
                        cx.background_executor().timer(wait).await;
                        let _ = cx.update(|cx| {
                            this.0.borrow_mut().queue.push_back(uid.clone());
                            this.pump(&netc, cx);
                        });
                    }
                }
            })
            .detach();
        }
    }
}

impl Default for Cosmetics {
    fn default() -> Self {
        Self::new()
    }
}
