//! The "Agu Chat - Log in to YouTube" window on macOS: a GPUI window with a WKWebView (`webview_host_mac.rs`) showing
//! Google's own sign-in page for YouTube. The same behaviour as `youtube_login.rs` (Windows): the user types the
//! password into Google's page, never into Agu Chat; when the youtube.com session cookies show up in the web view's
//! cookie store (`auth::login_complete`) the window closes and the kept cookies go to core in memory
//! (`agu_chat_core::youtube::auth`, which verifies and stores them encrypted). Closing the window first is a cancel.
//! Private (non-persistent) data store; the page is muted (youtube.com's home page autoplays).
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agu_chat_core::youtube::auth::{self, Cookies, LoginHook};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::App;

use super::webview_host_mac::{self as host, Host, Spec, Store};

const COOKIE_HOST: &str = "www.youtube.com";
const POLL: Duration = Duration::from_millis(500);
const SPEC: Spec = Spec {
    title: "Agu Chat \u{2014} Log in to YouTube",
    url: "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F",
    // Google's sign-in can open a popup (window.open)
    popup_hosts: &["accounts.google.com", "google.com", "youtube.com"],
    mute: true,
    store: Store::Private,
    hidden: false,
};

type Reply = oneshot::Sender<Result<Cookies, String>>;
type ReplyCell = Rc<RefCell<Option<Reply>>>;

enum Req {
    Open(Reply),
    Close,
}

thread_local! {
    static CUR: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// Hook the core's YouTube login up to this window. Called once, with the real backend.
pub fn install(cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<Req>();
    let (open_tx, close_tx) = (tx.clone(), tx);
    auth::set_login_hook(LoginHook {
        open: Arc::new(move || {
            let (rtx, rrx) = oneshot::channel();
            let _ = open_tx.unbounded_send(Req::Open(rtx));
            Box::pin(async move { rrx.await.unwrap_or_else(|_| Err(String::new())) })
        }),
        close: Arc::new(move || {
            let _ = close_tx.unbounded_send(Req::Close);
        }),
    });
    cx.spawn(async move |cx| {
        while let Some(req) = rx.next().await {
            let _ = cx.update(|cx| match req {
                Req::Open(reply) => open(cx, reply),
                Req::Close => close_window(cx),
            });
        }
    })
    .detach();
}

fn close_window(cx: &mut App) {
    let cur = CUR.with(|c| c.borrow_mut().take());
    if let Some(h) = cur {
        h.close(cx);
    }
}

/// Open the sign-in window (replacing one that is open). `reply` gets the cookies, or an error; closing the
/// window without logging in answers it with an empty error, which the core reads as a cancel.
fn open(cx: &mut App, reply: Reply) {
    close_window(cx);
    let reply: ReplyCell = Rc::new(RefCell::new(Some(reply)));
    let on_gone = reply.clone();
    let opened = Host::open(cx, &SPEC, move |_| {
        let r = on_gone.borrow_mut().take();
        if let Some(r) = r {
            let _ = r.send(Err(String::new()));
        }
    });
    let h = match opened {
        Ok(h) => h,
        Err(e) => {
            let r = reply.borrow_mut().take();
            if let Some(r) = r {
                let _ = r.send(Err(format!("Could not open the YouTube login window ({e}).")));
            }
            return;
        }
    };
    CUR.with(|c| *c.borrow_mut() = Some(h.clone()));
    host::poll(cx, &h, POLL, move |h, cx| {
        if reply.borrow().is_none() {
            return false;
        }
        // (name, value) of the cookies core keeps (`auth::KEPT`); nothing else is copied out
        let Some(list) = h.cookies_for(COOKIE_HOST, |n| auth::KEPT.contains(&n)) else { return true };
        if auth::login_complete(&list) {
            let r = reply.borrow_mut().take();
            if let Some(r) = r {
                let _ = r.send(Ok(Cookies { pairs: list }));
            }
            let h = h.clone();
            let _ = cx.update(|cx| {
                CUR.with(|c| *c.borrow_mut() = None);
                h.close(cx);
            });
            return false;
        }
        true
    });
}
