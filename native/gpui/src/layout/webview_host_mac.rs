//! The macOS host of the login windows (`x_login_mac.rs`, `youtube_login_mac.rs`, `twitch_login_mac.rs`): an
//! ordinary GPUI window with a `wry` WKWebView attached as a child view.
//!
//! WINDOW. On Windows GPUI's DirectComposition visual paints over child windows, which is why those logins use a raw
//! Win32 window. On macOS GPUI's `native_view` is a plain layer-backed NSView, so a WKWebView added as its subview
//! shows above it and `gpui::Window` implements `HasWindowHandle` (AppKit handle) for `build_as_child`. GPUI owns the
//! NSApplication run loop; no tao/winit loop is started. The root view is an empty div; the web view is kept sized to
//! the window by a bounds observer.
//!
//! CLOSE / CANCEL. Whatever ends the window (the red button, `Host::close`) releases the root view, and that runs
//! the `on_gone` callback and drops the WebView. The login files use `on_gone` to answer "cancelled" when no
//! cookie was handed over yet.
//!
//! COOKIES. wry's `WebView::cookies()` (WKHTTPCookieStore) with the domain filter done here, because wry's own
//! `cookies_for_url` compares the cookie domain (".x.com") to the URL host ("x.com") literally and would match
//! nothing. The call blocks the main thread for a few ms while it spins the run loop, so it must run outside any
//! `App` borrow: `poll` calls the tick from a spawned task, not from inside `App::update`. The WebView is taken
//! out of its cell for the duration of the call, so a window that closes meanwhile cannot re-enter the cell.
//!
//! PROFILE / DATA. WKWebView has no data-directory option (WebView2's profile folder). `Store::Private` is WebKit's
//! non-persistent data store (nothing on disk, gone with the window, isolated from every other web view in the
//! app, on every macOS back to 11): used by X and YouTube, whose Windows profile is deleted after the login anyway.
//! `Store::Persistent(id)` is a named on-disk data store (`with_data_store_identifier`, macOS 14+), used by Twitch
//! so the twitch.tv session survives between logins like its WebView2 profile; on macOS 11-13 it falls back to the
//! private store (the login page shows each time). The default data store is never used, so the logins never
//! share cookies with anything else.
//!
//! The User-Agent is WKWebView's own; nothing is spoofed. WKWebView has no mute switch, so `Spec::mute` injects a
//! small script that keeps the page's media elements muted (twitch.tv and youtube.com autoplay).
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use gpui::{
    div, px, size, AnyWindowHandle, App, AppContext, AsyncApp, Bounds, Context, IntoElement, Render, Styled, TitlebarOptions, Window,
    WindowBounds, WindowKind, WindowOptions,
};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtDarwin, WebViewExtDarwin};

/// Where the page keeps its cookies and storage.
#[derive(Clone, Copy)]
pub enum Store {
    /// Non-persistent: nothing survives the window.
    Private,
    /// Named on-disk store (macOS 14+; the private store on older systems).
    Persistent([u8; 16]),
}

pub struct Spec {
    pub title: &'static str,
    pub url: &'static str,
    /// hosts (and their subdomains) a `window.open` popup may go to; everything else stays blocked
    pub popup_hosts: &'static [&'static str],
    pub mute: bool,
    pub store: Store,
    /// created without being shown; `Host::show` brings it up
    pub hidden: bool,
}

const WIDTH: f32 = 520.;
const HEIGHT: f32 = 760.;

const MUTE_JS: &str = "(function(){function m(){document.querySelectorAll('video,audio').forEach(function(e){if(!e.muted){e.muted=true;}});}setInterval(m,250);})();";

struct Root;

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full()
    }
}

/// An open login window. Cheap to clone; all clones talk to the same window.
#[derive(Clone)]
pub struct Host {
    window: AnyWindowHandle,
    webview: Rc<RefCell<Option<WebView>>>,
    gone: Rc<Cell<bool>>,
}

/// macOS major version (11, 12, ... 26), 0 when it cannot be read.
fn macos_major() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().split('.').next().and_then(|m| m.parse().ok()))
            .unwrap_or(0)
    })
}

/// True when the persistent named data store can be used on this system.
pub fn persistent_store_available() -> bool {
    macos_major() >= 14
}

/// Delete a named on-disk data store (no-op where `Store::Persistent` is not available). Call with no web view
/// left on it; `done(true)` when WebKit reported success. Main thread, with the run loop running.
pub fn remove_store(id: [u8; 16], done: impl FnOnce(bool) + Send + 'static) {
    if !persistent_store_available() {
        done(true);
        return;
    }
    <WebView as WebViewExtDarwin>::remove_data_store(&id, move |r| done(r.is_ok()));
}

fn logical_rect(w: f32, h: f32) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(0., 0.).into(),
        size: wry::dpi::LogicalSize::new(w.max(1.) as f64, h.max(1.) as f64).into(),
    }
}

/// True when `url` is https and its host is one of `hosts` or a subdomain of one.
pub fn popup_allowed(url: &str, hosts: &[&str]) -> bool {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").rsplit('@').next().unwrap_or("").split(':').next().unwrap_or("").to_ascii_lowercase();
    url.starts_with("https://") && hosts.iter().any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

/// A cookie with `domain` (".x.com", "www.youtube.com") is sent to `host` ("x.com", "www.youtube.com").
fn domain_matches(domain: &str, host: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
}

fn build_webview(window: &Window, spec: &Spec) -> Result<WebView, String> {
    let s = window.viewport_size();
    let hosts = spec.popup_hosts;
    let mut b = WebViewBuilder::new()
        .with_url(spec.url)
        .with_bounds(logical_rect(f32::from(s.width), f32::from(s.height)))
        .with_focused(true)
        .with_accept_first_mouse(true)
        .with_new_window_req_handler(move |url, _| if popup_allowed(&url, hosts) { wry::NewWindowResponse::Allow } else { wry::NewWindowResponse::Deny });
    b = match spec.store {
        Store::Persistent(id) if persistent_store_available() => b.with_data_store_identifier(id),
        _ => b.with_incognito(true),
    };
    if spec.mute {
        b = b.with_initialization_script(MUTE_JS);
    }
    b.build_as_child(window).map_err(|e| e.to_string())
}

fn fit(cell: &RefCell<Option<WebView>>, window: &Window) {
    let s = window.viewport_size();
    if let Some(wv) = cell.borrow().as_ref() {
        let _ = wv.set_bounds(logical_rect(f32::from(s.width), f32::from(s.height)));
    }
}

type GoneCb = Rc<RefCell<Option<Box<dyn FnOnce(&mut App)>>>>;

impl Host {
    /// Open the window. `on_gone` runs once when the window is gone for any reason (not when this returns `Err`).
    pub fn open(cx: &mut App, spec: &Spec, on_gone: impl FnOnce(&mut App) + 'static) -> Result<Host, String> {
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx))),
            titlebar: Some(TitlebarOptions { title: Some(spec.title.into()), appears_transparent: false, traffic_light_position: None }),
            focus: !spec.hidden,
            show: !spec.hidden,
            kind: WindowKind::Normal,
            is_resizable: true,
            is_minimizable: true,
            window_min_size: Some(size(px(360.), px(480.))),
            ..Default::default()
        };
        let cell: Rc<RefCell<Option<WebView>>> = Rc::new(RefCell::new(None));
        let gone = Rc::new(Cell::new(false));
        let on_gone: GoneCb = Rc::new(RefCell::new(Some(Box::new(on_gone))));
        let mut build_err: Option<String> = None;
        let handle = cx
            .open_window(opts, |window, cx| {
                match build_webview(window, spec) {
                    Ok(wv) => *cell.borrow_mut() = Some(wv),
                    Err(e) => build_err = Some(e),
                }
                let (cell_rel, gone_rel, gone_cb) = (cell.clone(), gone.clone(), on_gone.clone());
                let cell_fit = cell.clone();
                cx.new(|cx| {
                    cx.on_release(move |_, cx| {
                        gone_rel.set(true);
                        let wv = cell_rel.borrow_mut().take();
                        drop(wv);
                        let f = gone_cb.borrow_mut().take();
                        if let Some(f) = f {
                            f(cx);
                        }
                    })
                    .detach();
                    cx.observe_window_bounds(window, move |_, window, _| fit(&cell_fit, window)).detach();
                    Root
                })
            })
            .map_err(|e| e.to_string())?;
        if let Some(e) = build_err {
            // the window is useless without its web view
            on_gone.borrow_mut().take();
            let _ = handle.update(cx, |_, window, _| window.remove_window());
            return Err(e);
        }
        Ok(Host { window: handle.into(), webview: cell, gone })
    }

    pub fn is_gone(&self) -> bool {
        self.gone.get()
    }

    /// (name, value) of the cookies the page would send to `host` whose name passes `keep`; `None` when the
    /// cookie store could not be read in time. Blocks for a moment (see the module comment): never call it from
    /// inside `App::update`.
    pub fn cookies_for(&self, host: &str, keep: impl Fn(&str) -> bool) -> Option<Vec<(String, String)>> {
        let wv = self.webview.borrow_mut().take()?;
        let all = wv.cookies();
        if self.gone.get() {
            drop(wv);
        } else {
            *self.webview.borrow_mut() = Some(wv);
        }
        let all = all.ok()?;
        Some(
            all.iter()
                .filter(|c| domain_matches(c.domain().unwrap_or(""), host) && keep(c.name()))
                .map(|c| (c.name().to_string(), c.value().to_string()))
                .collect(),
        )
    }

    pub fn load_url(&self, url: &str) {
        if let Some(wv) = self.webview.borrow().as_ref() {
            let _ = wv.load_url(url);
        }
    }

    /// Bring the window to the front (a hidden one appears) and give the page the keyboard.
    pub fn show(&self, cx: &mut App) {
        if self.gone.get() {
            return;
        }
        cx.activate(true);
        let _ = self.window.update(cx, |_, window, _| window.activate_window());
        if let Some(wv) = self.webview.borrow().as_ref() {
            let _ = wv.focus();
        }
    }

    /// Close the window (its `on_gone` runs once GPUI has released it).
    pub fn close(&self, cx: &mut App) {
        if self.gone.get() {
            return;
        }
        let wv = self.webview.borrow_mut().take();
        drop(wv);
        let _ = self.window.update(cx, |_, window, _| window.remove_window());
    }
}

/// Call `tick` every `every` until the window is gone or `tick` returns false. It runs from a spawned task, so it
/// may call `Host::cookies_for`; use `cx.update` for anything that needs the `App`.
pub fn poll(cx: &mut App, host: &Host, every: Duration, mut tick: impl FnMut(&Host, &mut AsyncApp) -> bool + 'static) {
    let host = host.clone();
    cx.spawn(async move |cx| loop {
        cx.background_executor().timer(every).await;
        if host.is_gone() || !tick(&host, cx) {
            break;
        }
    })
    .detach();
}
