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
/// Run in the sign-in page once it is back on rumble.com: asks rumble.com, from its own page, whether this browser is
/// logged in. Rumble swaps the guest `u_s` for the real one a moment after the page is back, so the cookie alone can
/// still be the guest's. Answers 'yes', 'no' or 'pending' (a 'no' asks again next time). Android has a copy.
pub const PAGE_CHECK_JS: &str = "(function(){var s=window.__aguLogin;if(s==='yes'||s==='pending')return s;window.__aguLogin='pending';fetch('/service.php?name=user.has_unread_notifications',{method:'POST',credentials:'include',headers:{'Accept':'application/json'}}).then(function(r){return r.json()}).then(function(j){window.__aguLogin=(j&&j.user&&j.user.logged_in)?'yes':'no'}).catch(function(){window.__aguLogin='no'});return 'pending'})()";

/// The page check's answer (as a web view hands it back, JSON-quoted or not) says logged in.
pub fn page_says_logged_in(answer: &str) -> bool {
    answer.trim().trim_matches('"') == "yes"
}

/// Every `u_s` value in a cookie list, in order.
pub fn all_sessions(list: &[(String, String)]) -> Vec<(String, String)> {
    list.iter().filter(|(k, v)| k == "u_s" && !v.trim().is_empty()).cloned().collect()
}

pub fn session_of(pairs: &[(String, String)]) -> Option<String> {
    sessions_of(pairs).into_iter().next()
}

/// Every distinct `u_s` the browser had: a guest one set for .rumble.com can sit next to the logged-in one for
/// rumble.com, and the cookie jar does not say which is which.
pub fn sessions_of(pairs: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in pairs {
        let v = v.trim();
        if k == "u_s" && !v.is_empty() && !out.iter().any(|o| o == v) {
            out.push(v.to_string());
        }
    }
    out
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
        assert!(page_says_logged_in("\"yes\"") && page_says_logged_in("yes"));
        assert!(!page_says_logged_in("\"pending\"") && !page_says_logged_in("null") && !page_says_logged_in(""));
        assert_eq!(all_sessions(&p(&[("u_s", "a"), ("x", "1"), ("u_s", " "), ("u_s", "b")])).len(), 2);
        assert_eq!(sessions_of(&p(&[("u_s", "guest"), ("u_s", "me"), ("u_s", "guest"), ("u_s", "")])), vec!["guest", "me"]);
    }
}
