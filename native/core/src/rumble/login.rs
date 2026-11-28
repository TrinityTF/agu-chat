//! The Rumble sign-in window, provided by the app. Rumble refuses password logins from anything but a browser, so
//! the user signs in on Rumble's own page in a web view (the same design as X: `x::auth::LoginHook`) and the
//! app hands back the cookies it found there; core only keeps `u_s`. Cookie values are never logged.
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;

/// `(name, value)` cookie pairs of rumble.com.
pub type Pairs = Vec<(String, String)>;

/// What opening the sign-in window resolves to: the cookies, or an error. An empty error is "the window was
/// closed", which is a cancel and not worth a message.
pub type LoginFuture = BoxFuture<'static, Result<Pairs, String>>;

#[derive(Clone)]
pub struct LoginHook {
    /// Open the window; the future resolves when it found the session cookie or was closed.
    pub open: Arc<dyn Fn() -> LoginFuture + Send + Sync>,
    /// Close the window (a cancel from Settings).
    pub close: Arc<dyn Fn() + Send + Sync>,
}

static HOOK: Mutex<Option<LoginHook>> = Mutex::new(None);

pub fn set_login_hook(h: LoginHook) {
    *HOOK.lock().unwrap() = Some(h);
}

pub(super) fn hook() -> Option<LoginHook> {
    HOOK.lock().unwrap().clone()
}

/// The session cookie among the pairs (the one Rumble's `service.php` and chat API take as `u_s=`).
pub fn session_of(pairs: &[(String, String)]) -> Option<String> {
    pairs.iter().find(|(k, v)| k == "u_s" && !v.trim().is_empty()).map(|(_, v)| v.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_non_empty_u_s_counts() {
        let p = |l: &[(&str, &str)]| l.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Pairs>();
        assert_eq!(session_of(&p(&[("a", "1"), ("u_s", "tok")])), Some("tok".into()));
        assert_eq!(session_of(&p(&[("u_s", " ")])), None);
        assert_eq!(session_of(&p(&[("u_sx", "tok")])), None);
        assert_eq!(session_of(&[]), None);
    }
}
