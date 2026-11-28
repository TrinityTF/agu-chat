//! The "Agu Chat - Log in to Rumble" window on macOS: a GPUI window with a WKWebView (`webview_host_mac.rs`) showing
//! Rumble's own login page. The same behaviour as `rumble_login.rs` (Windows): the user types the password into
//! Rumble's page, never into Agu Chat (Rumble refuses password logins from anything but a browser). When the `u_s`
//! session cookie shows up in the web view's cookie store AND the page is back on rumble.com (guests get a `u_s` too)
//! the window closes and the cookie goes to core in memory (`agu_chat_core::rumble`, which checks it with Rumble and
//! stores it encrypted). Closing the window first is a cancel (the reply is answered with an empty error). The web
//! view uses a private (non-persistent) data store, so nothing is left behind once the window is gone.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agu_chat_core::rumble::login::{self as auth, LoginHook, Pairs};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::App;

use super::webview_host_mac::{self as host, Host, Spec, Store};

const COOKIE_HOST: &str = "rumble.com";
const POLL: Duration = Duration::from_millis(500);
const SPEC: Spec = Spec {
    title: "Agu Chat \u{2014} Log in to Rumble",
    url: "https://auth.rumble.com/?redirect_uri=https%3A%2F%2Frumble.com%2F",
    // Rumble's "Continue with Google / Apple" buttons open the provider's sign-in in a popup (window.open)
    popup_hosts: &["accounts.google.com", "appleid.apple.com", "rumble.com"],
    mute: false,
    store: Store::Private,
    hidden: false,
};

type Reply = oneshot::Sender<Result<Pairs, String>>;
type ReplyCell = Rc<RefCell<Option<Reply>>>;

enum Req {
    Open(Reply),
    Close,
}

thread_local! {
    static CUR: RefCell<Option<Host>> = const { RefCell::new(None) };
}

/// True when the page is on rumble.com itself, i.e. it has left the auth.rumble.com login page.
fn back_on_rumble(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    (host == "rumble.com" || host.ends_with(".rumble.com")) && host != "auth.rumble.com"
}

/// Hook the core's Rumble login up to this window. Called once, with the real backend.
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
                let _ = r.send(Err(format!("Could not open the Rumble login window ({e}).")));
            }
            return;
        }
    };
    CUR.with(|c| *c.borrow_mut() = Some(h.clone()));
    // the page's own answer to "is this browser logged in" (see rumble::login::PAGE_CHECK_JS)
    let answer: std::sync::Arc<std::sync::Mutex<String>> = Default::default();
    let mut ticks = 0u32;
    host::poll(cx, &h, POLL, move |h, cx| {
        if reply.borrow().is_none() {
            return false;
        }
        // (name, value) of rumble.com's cookies; core keeps the session and drops Cloudflare's
        let Some(list) = h.cookies_for(COOKIE_HOST, |_| true) else { return true };
        let sessions = agu_chat_core::rumble::login::all_sessions(&list);
        if !sessions.is_empty() && h.url().as_deref().map(back_on_rumble).unwrap_or(false) {
            ticks += 1;
            if !agu_chat_core::rumble::login::page_says_logged_in(&answer.lock().unwrap()) {
                if ticks % 3 == 1 {
                    let a = answer.clone();
                    h.eval(agu_chat_core::rumble::login::PAGE_CHECK_JS, move |v| *a.lock().unwrap() = v);
                }
                return true;
            }
            let r = reply.borrow_mut().take();
            if let Some(r) = r {
                let _ = r.send(Ok(list));
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
