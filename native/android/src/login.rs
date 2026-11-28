//! Sign-in windows for the cookie logins (Twitch's web session, YouTube, X, Rumble). The core asks the app for a window
//! through each platform's `set_login_hook`; on Android the window is a Kotlin screen with a WebView, so a request
//! crosses the bridge as an event and the answer comes back as a call:
//!
//! - `login.webview` `{platform, url, id}`: show a sign-in WebView on `url`; answer with `login.result {id, cookies}`
//!   when the sign-in cookies showed up, `{id, error}` when it failed, `{id, cancelled: true}` when it was closed.
//! - `login.show` `{platform, url, id}` (Twitch): point the open window at `url`; the request ends (any `login.result`
//!   for `id`) when the window is gone.
//! - `login.close` `{platform}`: close the window (a cancel from Accounts, or the login ended).
//! - `login.forget` `{platform}`: close it and drop the site's cookies, so the next login starts clean.
//!
//! `cookies` is a list of `[name, value]` pairs; only the names a platform's login needs are ever read, and no
//! value is logged. A request whose window is closed or replaced resolves to an empty error, which core reads as a
//! cancel (nothing to show the user).
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use agu_chat_core::{rumble, twitch, x, youtube};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use serde_json::{json, Value};

use crate::session::Sink;

type Pairs = Vec<(String, String)>;
type Answer = Result<Pairs, String>;

struct Waiting {
    platform: &'static str,
    tx: oneshot::Sender<Answer>,
}

#[derive(Default)]
struct State {
    next: u64,
    waiting: HashMap<u64, Waiting>,
}

/// The bridge between the core's login hooks and the Kotlin sign-in screen.
#[derive(Clone)]
pub struct LoginBridge {
    sink: Sink,
    state: Arc<Mutex<State>>,
}

impl LoginBridge {
    pub fn new(sink: Sink) -> Self {
        LoginBridge { sink, state: Arc::new(Mutex::new(State::default())) }
    }

    /// Ask the app for a window event; the future resolves with its answer. A request that is dropped without an
    /// answer (the window was closed by core, or replaced) is an empty error.
    pub fn request(&self, kind: &'static str, platform: &'static str, url: &str) -> BoxFuture<'static, Answer> {
        let (tx, rx) = oneshot::channel();
        let id = {
            let mut s = self.state.lock().unwrap();
            s.next += 1;
            let id = s.next;
            // one window per platform: an older request of it is over
            s.waiting.retain(|_, w| w.platform != platform || kind == "login.show");
            s.waiting.insert(id, Waiting { platform, tx });
            id
        };
        (self.sink)(kind, &json!({"platform": platform, "url": url, "id": id}));
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err(String::new())) })
    }

    /// `login.result` from the app.
    pub fn answer(&self, args: &Value) -> Result<(), String> {
        let id = args.get("id").and_then(|v| v.as_u64()).ok_or("missing id")?;
        let w = self.state.lock().unwrap().waiting.remove(&id);
        let Some(w) = w else { return Ok(()) }; // already over
        let answer: Answer = if let Some(list) = args.get("cookies").and_then(|c| c.as_array()) {
            Ok(list
                .iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some((p.first()?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string()))
                })
                .collect())
        } else if args.get("cancelled").and_then(|c| c.as_bool()) == Some(true) {
            Err(String::new())
        } else {
            Err(args.get("error").and_then(|e| e.as_str()).unwrap_or("").to_string())
        };
        let _ = w.tx.send(answer);
        Ok(())
    }

    /// Core closes the window: tell the app, and end every request of the platform.
    pub fn close(&self, platform: &'static str, kind: &'static str) {
        self.state.lock().unwrap().waiting.retain(|_, w| w.platform != platform);
        (self.sink)(kind, &json!({"platform": platform}));
    }

    #[cfg(test)]
    fn pending(&self) -> usize {
        self.state.lock().unwrap().waiting.len()
    }

    /// Install the hooks of Twitch (web session), X, Rumble and YouTube.
    pub fn install(&self) {
        let b = self.clone();
        x::auth::set_login_hook(x::auth::LoginHook {
            open: Arc::new({
                let b = b.clone();
                move || {
                    let f = b.request("login.webview", "x", X_LOGIN_URL);
                    Box::pin(async move { x_cookies(&f.await?) })
                }
            }),
            close: Arc::new({
                let b = b.clone();
                move || b.close("x", "login.close")
            }),
        });
        rumble::login::set_login_hook(rumble::login::LoginHook {
            open: Arc::new({
                let b = b.clone();
                move || {
                    let f = b.request("login.webview", "rumble", RUMBLE_LOGIN_URL);
                    Box::pin(async move { rumble_cookies(f.await?) })
                }
            }),
            close: Arc::new({
                let b = b.clone();
                move || b.close("rumble", "login.close")
            }),
        });
        youtube::auth::set_login_hook(youtube::auth::LoginHook {
            open: Arc::new({
                let b = b.clone();
                move || {
                    let f = b.request("login.webview", "youtube", YOUTUBE_LOGIN_URL);
                    Box::pin(async move { youtube_cookies(f.await?) })
                }
            }),
            close: Arc::new({
                let b = b.clone();
                move || b.close("youtube", "login.close")
            }),
        });
        twitch::web::set_login_hook(twitch::web::LoginHook {
            open: Arc::new({
                let b = b.clone();
                move || {
                    let f = b.request("login.webview", "twitch", TWITCH_LOGIN_URL);
                    Box::pin(async move { twitch_cookies(&f.await?) })
                }
            }),
            show: Arc::new({
                let b = b.clone();
                move |url: String| {
                    let f = b.request("login.show", "twitch", &url);
                    Box::pin(async move {
                        let _ = f.await;
                    })
                }
            }),
            close: Arc::new({
                let b = b.clone();
                move || b.close("twitch", "login.close")
            }),
            forget: Arc::new({
                let b = b.clone();
                move || b.close("twitch", "login.forget")
            }),
        });
    }
}

pub const X_LOGIN_URL: &str = "https://x.com/i/flow/login";
/// Rumble's own sign-in page; it returns to rumble.com afterwards, which is where the session cookie is read.
pub const RUMBLE_LOGIN_URL: &str = "https://auth.rumble.com/?redirect_uri=https%3A%2F%2Frumble.com%2F";
/// Google signs in, then youtube.com/signin sets the session cookies and redirects to `continue`. That is a plain text
/// file rather than YouTube's front page: the page's scripts can hand a phone over to the YouTube app, which then took
/// the sign-in out of Agu Chat, and the cookies are all the app needs.
pub const YOUTUBE_LOGIN_URL: &str = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2Frobots.txt";
pub const TWITCH_LOGIN_URL: &str = "https://www.twitch.tv/login";

fn get<'a>(pairs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    pairs.iter().find(|(k, v)| k == name && !v.is_empty()).map(|(_, v)| v.as_str())
}

/// `auth_token` and `ct0` make the session; `twid` rides along when it is there (the desktop does the same).
pub fn x_cookies(pairs: &[(String, String)]) -> Result<x::auth::Cookies, String> {
    match (get(pairs, "auth_token"), get(pairs, "ct0")) {
        (Some(a), Some(c)) => Ok(x::auth::Cookies { auth_token: a.into(), ct0: c.into(), twid: get(pairs, "twid").unwrap_or_default().into() }),
        _ => Err("The X login did not finish, try again.".into()),
    }
}

/// `u_s` is Rumble's session; core checks it with Rumble before keeping it.
pub fn rumble_cookies(pairs: Pairs) -> Result<rumble::login::Pairs, String> {
    match get(&pairs, "u_s") {
        Some(_) => Ok(pairs),
        None => Err("The Rumble login did not finish, try again.".into()),
    }
}

pub fn youtube_cookies(pairs: Pairs) -> Result<youtube::auth::Cookies, String> {
    if youtube::auth::login_complete(&pairs) {
        Ok(youtube::auth::Cookies { pairs })
    } else {
        Err("The YouTube login did not finish, try again.".into())
    }
}

pub fn twitch_cookies(pairs: &[(String, String)]) -> Result<twitch::web::Cookies, String> {
    get(pairs, "auth-token").map(|t| twitch::web::Cookies { auth_token: t.into() }).ok_or_else(|| "The Twitch login did not finish, try again.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;

    fn bridge() -> (LoginBridge, Arc<Mutex<Vec<(String, Value)>>>) {
        let log: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let l = log.clone();
        (LoginBridge::new(Arc::new(move |k, v| l.lock().unwrap().push((k.to_string(), v.clone())))), log)
    }

    fn pairs(list: &[(&str, &str)]) -> Pairs {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn open_and_answer_round_trip() {
        let (b, log) = bridge();
        let fut = b.request("login.webview", "x", X_LOGIN_URL);
        let (kind, ev) = log.lock().unwrap()[0].clone();
        assert_eq!(kind, "login.webview");
        assert_eq!(ev["platform"], "x");
        assert_eq!(ev["url"], X_LOGIN_URL);
        let id = ev["id"].as_u64().unwrap();
        b.answer(&json!({"id": id, "cookies": [["auth_token", "a"], ["ct0", "c"], ["twid", "u%3D1"]]})).unwrap();
        let got = block_on(fut).unwrap();
        let c = x_cookies(&got).unwrap();
        assert_eq!((c.auth_token.as_str(), c.ct0.as_str(), c.twid.as_str()), ("a", "c", "u%3D1"));
        assert_eq!(b.pending(), 0);
    }

    #[test]
    fn cancel_is_an_empty_error() {
        let (b, log) = bridge();
        let fut = b.request("login.webview", "youtube", YOUTUBE_LOGIN_URL);
        let id = log.lock().unwrap()[0].1["id"].as_u64().unwrap();
        b.answer(&json!({"id": id, "cancelled": true})).unwrap();
        assert_eq!(block_on(fut), Err(String::new()));
    }

    #[test]
    fn error_text_comes_through() {
        let (b, log) = bridge();
        let fut = b.request("login.webview", "x", "u");
        let id = log.lock().unwrap()[0].1["id"].as_u64().unwrap();
        b.answer(&json!({"id": id, "error": "no WebView"})).unwrap();
        assert_eq!(block_on(fut), Err("no WebView".to_string()));
    }

    #[test]
    fn close_ends_the_request_and_tells_the_app() {
        let (b, log) = bridge();
        let fut = b.request("login.webview", "x", "u");
        b.close("x", "login.close");
        assert_eq!(block_on(fut), Err(String::new()));
        assert_eq!(log.lock().unwrap()[1], ("login.close".to_string(), json!({"platform": "x"})));
        // a late answer is ignored
        b.answer(&json!({"id": 1, "cancelled": true})).unwrap();
    }

    #[test]
    fn a_new_window_replaces_the_old_request_of_that_platform_only() {
        let (b, _log) = bridge();
        let old = b.request("login.webview", "x", "u");
        let other = b.request("login.webview", "youtube", "u");
        let _new = b.request("login.webview", "x", "u");
        assert_eq!(block_on(old), Err(String::new()));
        assert_eq!(b.pending(), 2);
        drop(other);
    }

    #[test]
    fn twitch_show_survives_the_open_request_and_ends_on_close() {
        let (b, log) = bridge();
        let open = b.request("login.webview", "twitch", TWITCH_LOGIN_URL);
        b.answer(&json!({"id": 1, "cookies": [["auth-token", "t"]]})).unwrap();
        assert_eq!(twitch_cookies(&block_on(open).unwrap()).unwrap().auth_token, "t");
        let show = b.request("login.show", "twitch", "https://www.twitch.tv/activate");
        assert_eq!(log.lock().unwrap()[1].0, "login.show");
        b.close("twitch", "login.close");
        assert_eq!(block_on(show), Err(String::new()));
    }

    #[test]
    fn cookie_rules() {
        assert!(x_cookies(&pairs(&[("auth_token", "a")])).is_err());
        assert!(x_cookies(&pairs(&[("auth_token", "a"), ("ct0", "")])).is_err());
        assert!(x_cookies(&pairs(&[("auth_token", "a"), ("ct0", "c")])).is_ok());
        assert!(rumble_cookies(pairs(&[("other", "a")])).is_err());
        assert!(rumble_cookies(pairs(&[("u_s", "")])).is_err());
        assert_eq!(rumble::login::session_of(&rumble_cookies(pairs(&[("u_s", "tok"), ("x", "y")])).unwrap()), Some("tok".to_string()));
        assert!(youtube_cookies(pairs(&[("SAPISID", "a")])).is_err());
        assert!(youtube_cookies(pairs(&[("SAPISID", "a"), ("LOGIN_INFO", "b")])).is_ok());
        assert!(twitch_cookies(&pairs(&[("auth-token", "")])).is_err());
        assert!(twitch_cookies(&pairs(&[("auth-token", "t")])).is_ok());
    }

    #[test]
    fn cookie_values_are_never_in_debug_output() {
        let c = x_cookies(&pairs(&[("auth_token", "SECRET"), ("ct0", "SECRET2")])).unwrap();
        assert!(!format!("{c:?}").contains("SECRET"));
    }
}
