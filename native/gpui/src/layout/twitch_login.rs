//! The "Agu Chat - Log in to Twitch" window: Microsoft Edge WebView2 (through `wry`) showing twitch.tv's own pages.
//!
//! The same design as `x_login.rs` (a plain Win32 window, because GPUI's DirectComposition paints over child
//! windows): the user types the password into twitch.tv's page, never into Agu Chat. It is one window for the whole
//! Twitch login (`agu_chat_core::twitch::signin`): twitch.tv's login page first, unless the profile already holds
//! the session. When the `auth-token` cookie shows up it goes to core in memory (which verifies it and stores it
//! encrypted) and the same window is pointed at the page that authorises the app (`show`). Core closes it when the
//! login ends; closing it by hand is a cancel.
//!
//! The WebView2 profile lives in `<data>/webview-twitch` and keeps the twitch.tv session between logins, so the
//! next login skips the sign-in page. `forget` (Twitch log out, an expired session) deletes it.
use std::cell::RefCell;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::twitch::web::{self as auth, Cookies, LoginHook};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::App;
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2CookieList, ICoreWebView2_2, ICoreWebView2_8};
use webview2_com::{take_pwstr, GetCookiesCompletedHandler};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebContext, WebView, WebViewBuilder, WebViewExtWindows};

const LOGIN_URL: &str = "https://www.twitch.tv/login";
const COOKIE_URL: &str = "https://www.twitch.tv";
const POLL_MS: u32 = 500;
const TIMER_ID: usize = 1;
/// Ticks (`POLL_MS` each) after which the window shows even though the profile has not answered.
const SHOW_AFTER_TICKS: u32 = 6;
const WINDOW_TITLE: &str = "Agu Chat \u{2014} Log in to Twitch";
const MISSING_RUNTIME: &str = "The Twitch login needs Microsoft Edge WebView2, which could not be started. Install the WebView2 Runtime from microsoft.com/edge/webview2 and try again.";

type Reply = oneshot::Sender<Result<Cookies, String>>;

enum Req {
    Open(Reply),
    /// the page to show, and the sender that is dropped when the window is gone
    Show(String, oneshot::Sender<()>),
    Close,
    Forget,
}

/// The open window's handle (0 = none), so core can close it.
static HWND_OPEN: AtomicIsize = AtomicIsize::new(0);

fn profile_dir() -> PathBuf {
    agu_chat_core::default_data_dir().join("webview-twitch")
}

/// Delete the profile folder, retrying while the WebView2 processes let go of it. A window opened meanwhile is
/// using the folder again, so that ends it.
fn delete_profile_later() {
    let dir = profile_dir();
    std::thread::spawn(move || {
        for _ in 0..30 {
            if HWND_OPEN.load(Ordering::SeqCst) != 0 || !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
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
    cx.spawn(async move |_cx| {
        while let Some(req) = rx.next().await {
            match req {
                Req::Open(reply) => open(reply),
                Req::Show(url, closed) => show(&url, closed),
                Req::Close => close_window(),
                Req::Forget => {
                    close_window();
                    delete_profile_later();
                }
            }
        }
    })
    .detach();
}

fn close_window() {
    let h = HWND_OPEN.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(h as *mut core::ffi::c_void));
        }
    }
}

/// twitch.tv's "Continue with Google / Apple" buttons open the provider's sign-in in a popup (window.open). Without
/// a handler wry blocks every popup, so those buttons did nothing. Sign-in providers and twitch.tv itself open in
/// WebView2's own popup window (same profile, so the login lands in this WebView); anything else stays blocked.
fn sign_in_popup(url: String, _features: wry::NewWindowFeatures) -> wry::NewWindowResponse {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    let ok = ["accounts.google.com", "appleid.apple.com", "twitch.tv"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if url.starts_with("https://") && ok {
        wry::NewWindowResponse::Allow
    } else {
        wry::NewWindowResponse::Deny
    }
}

/// A window handle for wry to build its child WebView in.
struct HostWindow(isize);

impl HasWindowHandle for HostWindow {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(h))) })
    }
}

type Slot = Arc<Mutex<Option<Vec<(String, String)>>>>;

struct Login {
    // field order is drop order: the WebView goes before the context that holds its profile folder
    webview: Option<WebView>,
    web_ctx: Option<WebContext>,
    /// answered with the cookie; gone once it was
    reply: Option<Reply>,
    /// held while the authorisation page is up; dropped with the window, which tells core
    closed: Option<oneshot::Sender<()>>,
    shown: bool,
    ticks: u32,
    /// the answer of the last cookie request, picked up by the next tick
    slot: Slot,
    inflight: Arc<AtomicBool>,
}

thread_local! {
    static CUR: RefCell<Option<Login>> = const { RefCell::new(None) };
}

fn client_rect(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    ((r.right - r.left).max(1), (r.bottom - r.top).max(1))
}

fn rect_px(w: i32, h: i32) -> wry::Rect {
    wry::Rect { position: wry::dpi::PhysicalPosition::new(0, 0).into(), size: wry::dpi::PhysicalSize::new(w as u32, h as u32).into() }
}

fn instance() -> HINSTANCE {
    HINSTANCE(unsafe { GetModuleHandleW(None) }.map(|m| m.0).unwrap_or_default())
}

fn register_class() -> PCWSTR {
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    let name = w!("AguChatTwitchLogin");
    if !REGISTERED.swap(true, Ordering::SeqCst) {
        unsafe {
            let inst = instance();
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: inst,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: name,
                ..Default::default()
            };
            RegisterClassExW(&class);
        }
    }
    name
}

/// Open the window on twitch.tv's login page (replacing one that is open). It stays hidden until it is clear the
/// user has to sign in: a profile that already holds the session never shows the login page. `reply` gets the
/// cookies, or an error; closing the window first drops it, which core reads as a cancel.
fn open(reply: Reply) {
    close_window();
    let class = register_class();
    // AGU_WIN_POS=x,y,w,h (test runs keep windows on the second monitor) also places this window.
    let forced = std::env::var("AGU_WIN_POS").ok().and_then(|s| {
        let v: Vec<i32> = s.split(',').filter_map(|p| p.trim().parse::<f32>().ok().map(|f| f as i32)).collect();
        (v.len() == 4).then(|| (v[0] + 40, v[1] + 40))
    });
    let (x, y) = forced.unwrap_or((CW_USEDEFAULT, CW_USEDEFAULT));
    let title = windows::core::HSTRING::from(WINDOW_TITLE);
    let hwnd = unsafe {
        let inst = instance();
        CreateWindowExW(WINDOW_EX_STYLE(0), class, &title, WS_OVERLAPPEDWINDOW, x, y, 520, 760, None, None, inst, None)
    };
    let Ok(hwnd) = hwnd else {
        let _ = reply.send(Err("Could not open the Twitch login window.".into()));
        return;
    };
    // size in device pixels for this monitor's scale
    unsafe {
        let dpi = GetDpiForWindow(hwnd).max(96) as i32;
        let (w, h) = (520 * dpi / 96, 760 * dpi / 96);
        let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
    }
    let dir = profile_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut web_ctx = WebContext::new(Some(dir));
    let (cw, ch) = client_rect(hwnd);
    let built = WebViewBuilder::new_with_web_context(&mut web_ctx).with_url(LOGIN_URL).with_bounds(rect_px(cw, ch)).with_focused(true).with_new_window_req_handler(sign_in_popup).build_as_child(&HostWindow(hwnd.0 as isize));
    let webview = match built {
        Ok(wv) => wv,
        Err(_) => {
            let _ = reply.send(Err(MISSING_RUNTIME.into()));
            drop(web_ctx);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return;
        }
    };
    // twitch.tv sends a signed-in user to its front page, which autoplays a stream: nothing in this window
    // ever needs sound
    unsafe {
        if let Ok(w8) = webview.webview().cast::<ICoreWebView2_8>() {
            let _ = w8.SetIsMuted(true);
        }
    }
    CUR.with(|c| *c.borrow_mut() = Some(Login { webview: Some(webview), web_ctx: Some(web_ctx), reply: Some(reply), closed: None, shown: false, ticks: 0, slot: Default::default(), inflight: Default::default() }));
    HWND_OPEN.store(hwnd.0 as isize, Ordering::SeqCst);
    unsafe {
        SetTimer(hwnd, TIMER_ID, POLL_MS, None);
    }
}

fn show_window(hwnd: HWND) {
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Point the open window at `url` (the page that authorises the app) and keep `closed` until the window is gone.
/// With no window left `closed` is dropped here, which tells core at once.
fn show(url: &str, closed: oneshot::Sender<()>) {
    let h = HWND_OPEN.load(Ordering::SeqCst);
    let open = CUR.with(|c| match c.borrow_mut().as_mut() {
        Some(l) => {
            if let Some(wv) = &l.webview {
                let _ = wv.load_url(url);
            }
            l.closed = Some(closed);
            l.shown = true;
            true
        }
        None => false,
    });
    if open {
        show_window(HWND(h as *mut core::ffi::c_void));
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_SIZE => {
            let (w, h) = client_rect(hwnd);
            CUR.with(|c| {
                if let Some(wv) = c.borrow().as_ref().and_then(|l| l.webview.as_ref()) {
                    let _ = wv.set_bounds(rect_px(w, h));
                }
            });
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_ID => {
            tick(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_ID);
            HWND_OPEN.store(0, Ordering::SeqCst);
            // taken out of the cell before anything is dropped, so nothing re-enters it
            let login = CUR.with(|c| c.borrow_mut().take());
            if let Some(mut l) = login {
                // closing the window before the cookie arrived is a cancel
                if let Some(r) = l.reply.take() {
                    let _ = r.send(Err(String::new()));
                }
                l.closed.take(); // tells core the window is gone
                l.webview.take();
                l.web_ctx.take();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// Pick up the last cookie answer, then ask again until the cookie is there. The window stays hidden when the
/// profile already holds the session; otherwise it shows as soon as the profile answered without it.
fn tick(hwnd: HWND) {
    let (answer, inflight, shown, ticks) = CUR.with(|c| match c.borrow_mut().as_mut() {
        Some(l) if l.reply.is_some() => {
            l.ticks += 1;
            (l.slot.lock().unwrap().take(), Some(l.inflight.clone()), l.shown, l.ticks)
        }
        _ => (None, None, true, 0),
    });
    let Some(inflight) = inflight else { return };
    let token = answer.as_ref().and_then(|list| list.iter().find(|(k, v)| k == "auth-token" && !v.is_empty()).map(|(_, v)| v.clone()));
    if let Some(auth_token) = token {
        // leave twitch.tv's front page (autoplaying stream) at once; core sends the window on to the authorize
        // page next
        let reply = CUR.with(|c| {
            c.borrow_mut().as_mut().and_then(|l| {
                if let Some(wv) = &l.webview {
                    let _ = wv.load_url("about:blank");
                }
                l.reply.take()
            })
        });
        if let Some(r) = reply {
            let _ = r.send(Ok(Cookies { auth_token }));
        }
        return;
    }
    if !shown && (answer.is_some() || ticks >= SHOW_AFTER_TICKS) {
        CUR.with(|c| {
            if let Some(l) = c.borrow_mut().as_mut() {
                l.shown = true;
            }
        });
        show_window(hwnd);
    }
    if !inflight.swap(true, Ordering::SeqCst) && !CUR.with(|c| c.borrow().as_ref().map(request_cookies).unwrap_or(false)) {
        inflight.store(false, Ordering::SeqCst);
    }
}

/// Ask WebView2's cookie manager for twitch.tv's cookies; the callback runs on this thread from the message loop.
fn request_cookies(l: &Login) -> bool {
    let Some(wv) = &l.webview else { return false };
    let (slot, inflight) = (l.slot.clone(), l.inflight.clone());
    let handler = GetCookiesCompletedHandler::create(Box::new(move |res, list| {
        inflight.store(false, Ordering::SeqCst);
        res?;
        if let Some(list) = list {
            *slot.lock().unwrap() = Some(read_list(&list)?);
        }
        Ok(())
    }));
    unsafe {
        let Ok(core) = wv.webview().cast::<ICoreWebView2_2>() else { return false };
        let Ok(manager) = core.CookieManager() else { return false };
        manager.GetCookies(&HSTRING::from(COOKIE_URL), &handler).is_ok()
    }
}

/// (name, value) of the one cookie the sign-in needs; nothing else is copied out.
fn read_list(list: &ICoreWebView2CookieList) -> windows_core::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    unsafe {
        let mut count = 0;
        list.Count(&mut count)?;
        for i in 0..count {
            let cookie = list.GetValueAtIndex(i)?;
            let mut name = PWSTR::null();
            cookie.Name(&mut name)?;
            let name = take_pwstr(name);
            if name != "auth-token" {
                continue;
            }
            let mut value = PWSTR::null();
            cookie.Value(&mut value)?;
            out.push((name, take_pwstr(value)));
        }
    }
    Ok(out)
}
