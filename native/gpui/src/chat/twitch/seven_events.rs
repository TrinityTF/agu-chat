//! The desktop's side of the 7TV EventAPI: the one shared socket (agu-chat-model's `SevenSocket`, which runs on the
//! net runtime) with its events hopped onto the UI thread, where the panes' listeners run. The protocol, parsing
//! and applying the changes are in agu-chat-model (shared with Android).
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use gpui::App;

pub use agu_chat_model::twitch::seven_events::*;

use crate::chat::net::Net;

/// A listener; it returns false once it is gone (its pane was released) and is then dropped.
type UiListener = Box<dyn Fn(&SevenEvent, &mut App) -> bool>;

#[derive(Clone)]
pub struct SevenEvents {
    socket: SevenSocket,
    listeners: Rc<RefCell<Vec<UiListener>>>,
}

impl SevenEvents {
    pub fn new(net: Arc<Net>, cx: &mut App) -> SevenEvents {
        let (tx, mut rx) = unbounded::<SevenEvent>();
        let socket = SevenSocket::new(
            &net,
            Arc::new(move |ev| {
                let _ = tx.unbounded_send(ev);
            }),
        );
        let this = SevenEvents { socket, listeners: Rc::new(RefCell::new(Vec::new())) };
        let pump = this.clone();
        cx.spawn(async move |cx| {
            while let Some(ev) = rx.next().await {
                if cx.update(|cx| pump.emit(&ev, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        this
    }

    pub fn listen(&self, f: impl Fn(&SevenEvent, &mut App) -> bool + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    pub fn subscribe(&self, kind: &str, object_id: &str) {
        self.socket.subscribe(kind, object_id);
    }

    pub fn unsubscribe(&self, kind: &str, object_id: &str) {
        self.socket.unsubscribe(kind, object_id);
    }

    fn emit(&self, ev: &SevenEvent, cx: &mut App) {
        // listeners run without the list borrowed: they may subscribe / unsubscribe
        let mut list = std::mem::take(&mut *self.listeners.borrow_mut());
        list.retain(|l| l(ev, cx));
        let mut cur = self.listeners.borrow_mut();
        list.append(&mut cur);
        *cur = list;
    }
}
