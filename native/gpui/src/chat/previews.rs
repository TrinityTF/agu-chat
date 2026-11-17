//! UI side of the link previews (core's `link_preview`): one cache shared by every pane, requests run on the net
//! runtime (at most 4 at a time), and a finished preview is handed to each pane's inbox so the rows that show it
//! are measured again. Nothing is asked while the "Link previews" setting is off.
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use agu_chat_core::link_preview::cache::PreviewCache;
use agu_chat_core::link_preview::fetch::fetch;
use agu_chat_core::link_preview::{LinkTarget, Preview};
use gpui::App;

use super::net::Net;

struct State {
    cache: PreviewCache,
    inboxes: Vec<Weak<RefCell<Vec<String>>>>,
}

#[derive(Clone)]
pub struct Previews(Rc<RefCell<State>>);

fn now_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

impl Default for Previews {
    fn default() -> Self {
        Self::new()
    }
}

impl Previews {
    pub fn new() -> Self {
        Previews(Rc::new(RefCell::new(State { cache: PreviewCache::new(), inboxes: Vec::new() })))
    }

    /// A pane's inbox of preview keys that just arrived.
    pub fn add_inbox(&self) -> Rc<RefCell<Vec<String>>> {
        let inbox = Rc::new(RefCell::new(Vec::new()));
        self.0.borrow_mut().inboxes.push(Rc::downgrade(&inbox));
        inbox
    }

    pub fn set_enabled(&self, on: bool) {
        self.0.borrow_mut().cache.set_enabled(on);
    }

    /// The preview of a link when it is in; otherwise None, and its request is started (once).
    pub fn get(&self, t: &LinkTarget, net: &Arc<Net>, cx: &mut App) -> Option<Arc<Preview>> {
        let got = self.0.borrow_mut().cache.get(t, now_ms());
        if got.is_none() {
            self.pump(net, cx);
        }
        got
    }

    /// Without starting anything.
    pub fn peek(&self, key: &str) -> Option<Arc<Preview>> {
        self.0.borrow().cache.peek(key)
    }

    fn pump(&self, net: &Arc<Net>, cx: &mut App) {
        loop {
            let Some(job) = self.0.borrow_mut().cache.next_job() else { break };
            let key = job.key.clone();
            let client = net.client.clone();
            let task = net.run(async move { fetch(&job, &client).await });
            let this = self.clone();
            let netc = net.clone();
            cx.spawn(async move |cx| {
                // a request that panicked counts as a blip
                let res = task.await.unwrap_or(Err(agu_chat_core::images::Fail::Transient));
                let _ = cx.update(|cx| {
                    let ok = this.0.borrow_mut().cache.finish(&key, res, now_ms());
                    if ok {
                        let mut st = this.0.borrow_mut();
                        st.inboxes.retain(|w| w.upgrade().is_some());
                        for w in st.inboxes.iter() {
                            if let Some(b) = w.upgrade() {
                                b.borrow_mut().push(key.clone());
                            }
                        }
                    }
                    this.pump(&netc, cx);
                    if ok {
                        cx.refresh_windows();
                    }
                });
            })
            .detach();
        }
    }
}
