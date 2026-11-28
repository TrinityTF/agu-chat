//! The "Agu Chat — Log in to Rumble" window: Microsoft Edge WebView2 (through the `wry` crate) showing Rumble's own
//! login page (auth.rumble.com).
//!
//! The user types the password into Rumble's page, never into Agu Chat. Rumble refuses password logins from
//! anything but a browser, so this is the only way in. When the `u_s` session cookie shows up in the WebView2 cookie
//! manager AND the page is back on rumble.com (Rumble sets `u_s` for guests too, so the cookie alone is not a login)
//! the window closes and the cookie goes to core in memory (`agu_chat_core::rumble`, which checks it with Rumble and
//! stores it encrypted). The WebView2 profile lives in `<data>/webview-rumble` and is deleted once the WebView is
//! gone; a folder left behind by a crash is removed at the next start. Nothing here gets past a Cloudflare check:
//! this is an ordinary web view with its default user agent that the user signs in with themselves.
//!
//! WHY A PLAIN WIN32 WINDOW AND NOT A GPUI ONE: see `x_login.rs` (GPUI's DirectComposition paints over child
//! windows). The core asks for the window through `rumble::login::set_login_hook`; requests cross to the UI thread
//! over a channel (`install`). Closing the window without logging in is a cancel (the reply is dropped with it).
use std::cell::RefCell;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::rumble::login::{self as auth, LoginHook, Pairs};
use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use gpui::App;
use raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use webview2_com::Microsoft::Web::WebView2::Win32::{ICoreWebView2CookieList, ICoreWebView2_2};
use webview2_com::{take_pwstr, GetCookiesCompletedHandler};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_core::{Interface, HSTRING, PWSTR};
use wry::{WebContext, WebView, WebViewBuilder, WebViewExtWindows};

const LOGIN_URL: &str = "https://auth.rumble.com/?redirect_uri=https%3A%2F%2Frumble.com%2F";
const COOKIE_URL: &str = "https://rumble.com";
const POLL_MS: u32 = 500;
const TIMER_ID: usize = 1;
const WINDOW_TITLE: &str = "Agu Chat \u{2014} Log in to Rumble";
const MISSING_RUNTIME: &str = "The Rumble login needs Microsoft Edge WebView2, which could not be started. Install the WebView2 Runtime from microsoft.com/edge/webview2 and try again.";

type Reply = oneshot::Sender<Result<Pairs, String>>;

enum Req {
    Open(Reply),
    Close,
}

/// The open sign-in window's handle (0 = none), so a cancel from Settings can close it.
static HWND_OPEN: AtomicIsize = AtomicIsize::new(0);

fn profile_dir() -> PathBuf {
    agu_chat_core::default_data_dir().join("webview-rumble")
}

/// Delete the profile folder, retrying while the WebView2 processes let go of it.
fn delete_profile_later() {
    let dir = profile_dir();
    std::thread::spawn(move || {
        for _ in 0..30 {
            if !dir.exists() || std::fs::remove_dir_all(&dir).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}

/// Hook the core's Rumble login up to this window. Called once, with the real backend.
pub fn install(cx: &mut App) {
    // a profile a crashed run left behind
    delete_profile_later();
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
    cx.spawn(async move |_cx| {
        while let Some(req) = rx.next().await {
            match req {
                Req::Open(reply) => open(reply),
                Req::Close => close_window(),
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

/// Rumble's login page offers Google / Apple sign-in in a popup (window.open). Without a handler wry blocks every
/// popup. Sign-in providers and Rumble itself open in WebView2's own popup window (same profile, so the login
/// lands in this WebView); anything else stays blocked.
fn sign_in_popup(url: String, _features: wry::NewWindowFeatures) -> wry::NewWindowResponse {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    let ok = ["accounts.google.com", "appleid.apple.com", "rumble.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
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
    reply: Option<Reply>,
    /// the answer of the last cookie request, picked up by the next tick
    slot: Slot,
    inflight: Arc<AtomicBool>,
    /// the page's own answer to "is this browser logged in" (rumble::login::PAGE_CHECK_JS)
    answer: Arc<Mutex<String>>,
    ticks: u32,
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
    let name = w!("AguChatRumbleLogin");
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

/// Open the sign-in window (replacing one that is open). `reply` gets the cookies, or an error; closing the
/// window without logging in drops it, which the core reads as a cancel.
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
        let _ = reply.send(Err("Could not open the Rumble login window.".into()));
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
            delete_profile_later();
            return;
        }
    };
    CUR.with(|c| *c.borrow_mut() = Some(Login { webview: Some(webview), web_ctx: Some(web_ctx), reply: Some(reply), slot: Default::default(), inflight: Default::default(), answer: Default::default(), ticks: 0 }));
    HWND_OPEN.store(hwnd.0 as isize, Ordering::SeqCst);
    unsafe {
        SetTimer(hwnd, TIMER_ID, POLL_MS, None);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
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
            if tick() {
                let _ = DestroyWindow(hwnd);
            }
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
                // closing the window without a login is a cancel
                if let Some(r) = l.reply.take() {
                    let _ = r.send(Err(String::new()));
                }
                l.webview.take();
                l.web_ctx.take();
            }
            delete_profile_later();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// True when the page is on rumble.com itself, i.e. it has left the auth.rumble.com login page.
fn back_on_rumble(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    (host == "rumble.com" || host.ends_with(".rumble.com")) && host != "auth.rumble.com"
}

/// Pick up the last cookie answer, then ask again. True when the session cookie was there, the page was back on
/// rumble.com, and the cookie went to the core.
fn tick() -> bool {
    let (answer, inflight, url) = CUR.with(|c| match c.borrow().as_ref() {
        Some(l) => (l.slot.lock().unwrap().take(), Some(l.inflight.clone()), l.webview.as_ref().and_then(|w| w.url().ok())),
        None => (None, None, None),
    });
    let Some(inflight) = inflight else { return false };
    if let Some(list) = answer {
        let sessions = agu_chat_core::rumble::login::all_sessions(&list);
        if !sessions.is_empty() && url.as_deref().map(back_on_rumble).unwrap_or(false) {
            if page_logged_in() {
                let mut list = list;
                let name = CUR.with(|c| c.borrow().as_ref().and_then(|l| agu_chat_core::rumble::login::page_username(&l.answer.lock().unwrap())));
                list.extend(name.map(|n| (agu_chat_core::rumble::login::USERNAME_KEY.to_string(), n)));
                let reply = CUR.with(|c| c.borrow_mut().as_mut().and_then(|l| l.reply.take()));
                if let Some(r) = reply {
                    let _ = r.send(Ok(list));
                }
                return true;
            }
        }
    }
    if !inflight.swap(true, Ordering::SeqCst) && !CUR.with(|c| c.borrow().as_ref().map(request_cookies).unwrap_or(false)) {
        inflight.store(false, Ordering::SeqCst);
    }
    false
}

/// True once the page said it is logged in; until then asks it again every third tick.
fn page_logged_in() -> bool {
    CUR.with(|c| {
        let mut b = c.borrow_mut();
        let Some(l) = b.as_mut() else { return false };
        if agu_chat_core::rumble::login::page_says_logged_in(&l.answer.lock().unwrap()) {
            return true;
        }
        l.ticks += 1;
        if l.ticks % 3 == 1 {
            if let Some(wv) = &l.webview {
                let a = l.answer.clone();
                let _ = wv.evaluate_script_with_callback(agu_chat_core::rumble::login::PAGE_CHECK_JS, move |v| *a.lock().unwrap() = v);
            }
        }
        false
    })
}

/// Ask WebView2's cookie manager for rumble.com's cookies; the callback runs on this thread from the message loop.
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

/// (name, value) of rumble.com's cookies; core keeps the session and drops Cloudflare's.
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
            let mut value = PWSTR::null();
            cookie.Value(&mut value)?;
            out.push((name, take_pwstr(value)));
        }
    }
    Ok(out)
}
