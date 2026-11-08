//! The one Twitch login: a single window gets the user both the OAuth login (device code flow, `auth.rs`) and the
//! twitch.tv web session the user card needs (`web.rs`).
//!
//! The device flow starts first, so Settings has its pending state at once. The window then shows twitch.tv's login
//! page (skipped when its browser profile already holds the session), the session cookie is verified and stored,
//! and the same window goes to the page that authorises the device code, so the user only has to click Authorize.
//! The window closes when the flow ends, however it ends; closing it early cancels the flow and keeps the web
//! session if one was already stored.
use std::time::Duration;

use super::auth;
use super::web::{self, GqlError, LoginHook};
use crate::protocol::DevicePrompt;

const FLOW_CHECK: Duration = Duration::from_secs(1);

/// Starts the combined login. The prompt is the device flow's (the window opens the page itself); the Err is the
/// message to show.
pub async fn login() -> Result<DevicePrompt, String> {
    let hook = web::login_hook().ok_or("The Twitch login window is not available in this build.")?;
    let prompt = auth::login().await?;
    tokio::spawn(run(hook, prompt.clone()));
    Ok(prompt)
}

/// What a failed window means for the device flow: nothing to say for a plain close, a message otherwise.
fn failure_message(r: &Result<(), String>) -> Option<&str> {
    r.as_ref().err().map(String::as_str).filter(|m| !m.is_empty())
}

async fn run(hook: LoginHook, prompt: DevicePrompt) {
    let code = prompt.user_code;
    let window = async {
        let cookies = (hook.open)().await?;
        if let Err(e) = web::store(cookies).await {
            if e == GqlError::Unauthorized {
                (hook.forget)(); // a dead session in the profile would be captured again
            }
            return Err(e.message());
        }
        (hook.show)(prompt.verification_uri).await;
        Ok(())
    };
    let ended = async {
        while auth::is_waiting(&code) {
            tokio::time::sleep(FLOW_CHECK).await;
        }
    };
    tokio::select! {
        r = window => auth::end_flow(&code, failure_message(&r)),
        _ = ended => (hook.close)(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_the_window_is_a_cancel_not_an_error() {
        assert_eq!(failure_message(&Ok(())), None);
        assert_eq!(failure_message(&Err(String::new())), None);
        assert_eq!(failure_message(&Err("no WebView2".into())), Some("no WebView2"));
    }
}
