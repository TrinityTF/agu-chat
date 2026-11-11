//! The "Agu Chat - Log in to Twitch" window on macOS: a GPUI window with a WKWebView (`webview_host_mac.rs`) showing
//! twitch.tv's own pages. The same behaviour as `twitch_login.rs` (Windows): one window for the whole Twitch login
//! (`agu_chat_core::twitch::signin`), the password goes into twitch.tv's page, never into Agu Chat. It stays hidden
//! until it is clear the user has to sign in (a profile that already holds the session never shows the login page).
//! When the `auth-token` cookie shows up it goes to core in memory and the same window is pointed at the page that
//! authorises the app (`show`). Core closes it when the login ends; closing it by hand is a cancel.
//!
//! The data store is a named persistent one (macOS 14+, see `webview_host_mac.rs`) that keeps the twitch.tv session
//! between logins like the Windows WebView2 profile; `forget` (Twitch log out, an expired session) deletes it. On
//! macOS 11-13 the store is private, so the login page shows each time and there is nothing to delete.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agu_chat_core::twitch::web::{self as auth, Cookies, LoginHook};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::App;

use super::webview_host_mac::{self as host, Host, Spec, Store};

const COOKIE_HOST: &str = "www.twitch.tv";
const POLL: Duration = Duration::from_millis(500);
/// Ticks (`POLL` each) after which the window shows even though the cookie store has not answered.
const SHOW_AFTER_TICKS: u32 = 6;
/// The named data store of the Twitch session (16 bytes become the store's UUID).
const STORE_ID: [u8; 16] = *b"AguChat-Twitch-1";
const SPEC: Spec = Spec {
    title: "Agu Chat \u{2014} Log in to Twitch",
    url: "https://www.twitch.tv/login",
    // twitch.tv's "Continue with Google / Apple" buttons open the provider's sign-in in a popup (window.open)
    popup_hosts: &["accounts.google.com", "appleid.apple.com", "twitch.tv"],
    // twitch.tv sends a signed-in user to its front page, which autoplays a stream
    mute: true,
    store: Store::Persistent(STORE_ID),
    hidden: true,
};

type Reply = oneshot::Sender<Result<Cookies, String>>;

enum Req {
    Open(Reply),
    /// the page to show, and the sender that is dropped when the window is gone
    Show(String, oneshot::Sender<()>),
    Close,
    Forget,
}

struct Login {
    host: Host,
    /// held while the authorisation page is up; dropped with the window, which tells core
    closed: Rc<RefCell<Option<oneshot::Sender<()>>>>,
    shown: Rc<Cell<bool>>,
}

thread_local! {
    static CUR: RefCell<Option<Login>> = const { RefCell::new(None) };
}

/// Hook the core's Twitch login up to this window. Called once, with the real backend.
pub fn install(cx: &mut App) {
    let (tx, mut rx) = mpsc::unbounded::<Req>();
    let (open_tx, show_tx, close_tx, forget_tx) = (tx.clone(), tx.clone(), tx.clone(), tx);
    auth::set_login_hook(LoginHook {
        open: Arc::new(move || {
            let (rtx, rrx) = oneshot::channel();
            let _ = open_tx.unbounded_send(Req::Open(rtx));
            Box::pin(async move { rrx.await.unwrap_or_else(|_| Err(String::new())) })
        }),
        show: Arc::new(move |url| {
            let (stx, srx) = oneshot::channel();
            let _ = show_tx.unbounded_send(Req::Show(url, stx));
            // resolves when the sender is dropped: the window is gone
            Box::pin(async move {
                let _ = srx.await;
            })
        }),
        close: Arc::new(move || {
            let _ = close_tx.unbounded_send(Req::Close);
        }),
        forget: Arc::new(move || {
            let _ = forget_tx.unbounded_send(Req::Forget);
        }),
    });
    cx.spawn(async move |cx| {
        while let Some(req) = rx.next().await {
            let _ = cx.update(|cx| match req {
                Req::Open(reply) => open(cx, reply),
                Req::Show(url, closed) => show(cx, &url, closed),
                Req::Close => close_window(cx),
                Req::Forget => {
                    close_window(cx);
                    delete_store_later(cx);
                }
            });
        }
    })
    .detach();
}

fn close_window(cx: &mut App) {
    let cur = CUR.with(|c| c.borrow_mut().take());
    if let Some(l) = cur {
        l.host.close(cx);
    }
}

/// Delete the named data store, retrying while the closed window's web view lets go of it. A window opened
/// meanwhile is using the store again, so that ends it.
fn delete_store_later(cx: &mut App) {
    if !host::persistent_store_available() {
        return;
    }
    cx.spawn(async move |cx| {
        for _ in 0..5 {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if CUR.with(|c| c.borrow().is_some()) {
                return;
            }
            let ok = Arc::new(AtomicBool::new(false));
            let ok2 = ok.clone();
            host::remove_store(STORE_ID, move |done| ok2.store(done, Ordering::SeqCst));
            cx.background_executor().timer(Duration::from_millis(500)).await;
            if ok.load(Ordering::SeqCst) {
                return;
            }
        }
    })
    .detach();
}

/// Open the window on twitch.tv's login page (replacing one that is open), hidden. `reply` gets the cookies, or an
/// error; closing the window first answers it with an empty error, which core reads as a cancel.
fn open(cx: &mut App, reply: Reply) {
    close_window(cx);
    let reply = Rc::new(RefCell::new(Some(reply)));
    let closed: Rc<RefCell<Option<oneshot::Sender<()>>>> = Rc::new(RefCell::new(None));
    let (reply_gone, closed_gone) = (reply.clone(), closed.clone());
    let opened = Host::open(cx, &SPEC, move |_| {
        // closing the window before the cookie arrived is a cancel
        let r = reply_gone.borrow_mut().take();
        if let Some(r) = r {
            let _ = r.send(Err(String::new()));
        }
        // tells core the window is gone
        let c = closed_gone.borrow_mut().take();
        drop(c);
    });
    let h = match opened {
        Ok(h) => h,
        Err(e) => {
            let r = reply.borrow_mut().take();
            if let Some(r) = r {
                let _ = r.send(Err(format!("Could not open the Twitch login window ({e}).")));
            }
            return;
        }
    };
    let shown = Rc::new(Cell::new(false));
    CUR.with(|c| *c.borrow_mut() = Some(Login { host: h.clone(), closed, shown: shown.clone() }));
    let mut ticks = 0u32;
    host::poll(cx, &h, POLL, move |h, cx| {
        if reply.borrow().is_none() {
            return false;
        }
        ticks += 1;
        // (name, value) of the one cookie the sign-in needs; nothing else is copied out
        let answer = h.cookies_for(COOKIE_HOST, |n| n == "auth-token");
        let token = answer.as_ref().and_then(|list| list.iter().find(|(k, v)| k == "auth-token" && !v.is_empty()).map(|(_, v)| v.clone()));
        if let Some(auth_token) = token {
            // leave twitch.tv's front page (autoplaying stream) at once; core sends the window on to the
            // authorize page next
            h.load_url("about:blank");
            let r = reply.borrow_mut().take();
            if let Some(r) = r {
                let _ = r.send(Ok(Cookies { auth_token }));
            }
            return false;
        }
        // the window shows as soon as the cookie store answered without the cookie
        if !shown.get() && (answer.is_some() || ticks >= SHOW_AFTER_TICKS) {
            shown.set(true);
            let h = h.clone();
            let _ = cx.update(|cx| h.show(cx));
        }
        true
    });
}

/// Point the open window at `url` (the page that authorises the app) and keep `closed` until the window is gone.
/// With no window left `closed` is dropped here, which tells core at once.
fn show(cx: &mut App, url: &str, closed: oneshot::Sender<()>) {
    let cur = CUR.with(|c| c.borrow().as_ref().filter(|l| !l.host.is_gone()).map(|l| (l.host.clone(), l.closed.clone(), l.shown.clone())));
    let Some((host, slot, shown)) = cur else { return };
    host.load_url(url);
    *slot.borrow_mut() = Some(closed);
    shown.set(true);
    host.show(cx);
}
