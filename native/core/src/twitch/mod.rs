//! Port of src/main/twitch/index.js: the Twitch platform module. See docs/ARCHITECTURE.md ("Platform module").
//!
//!   reading   anonymous IRC (one shared socket, irc_hub.rs) + recent-messages history (backlog.rs)
//!   mod feed  one EventSub session (eventsub.rs) for the channels the user moderates
//!   writing   Helix with the user's own token (routes.rs)
//!   whispers  reserved `whisper:` channels, see whisper.rs
//!   login     OAuth device code flow with Agu Chat's Public client id (auth.rs)
//!
//! Nothing here talks to agu.lol / api.agu.lol, and there is no bot account or client secret.
pub mod access;
pub mod auth;
pub mod backlog;
pub mod channel;
pub mod eventsub;
pub mod helix;
pub mod http;
pub mod irc_hub;
pub mod routes;
pub mod rt;
pub mod shared;
pub mod source;
pub mod vendor;
pub mod whisper;

use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::platforms::{Accounts, Ctx, Emit, PlatformModule, Source};
use crate::protocol::{AccountState, Platform};
use crate::relay::Handler;

pub use channel::normalize_channel;

/// For other modules: the current token, if logged in.
pub use auth::get_user_token;

pub struct TwitchModule;

struct TwitchAccounts;

impl Accounts for TwitchAccounts {
    fn state(&self) -> AccountState {
        auth::state()
    }
    fn login(&self, _args: Value) -> BoxFuture<'static, Result<Value, String>> {
        Box::pin(async { auth::login().await.map(|p| serde_json::to_value(p).unwrap_or(Value::Null)) })
    }
    fn cancel(&self) {
        auth::cancel();
    }
    fn logout(&self) -> BoxFuture<'static, ()> {
        Box::pin(auth::logout())
    }
}

static AUTH_LISTENER: Mutex<Option<u64>> = Mutex::new(None);

impl PlatformModule for TwitchModule {
    fn platform(&self) -> Platform {
        Platform::Twitch
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        if whisper::parse(input.trim()).is_some() {
            return whisper::normalize(input);
        }
        channel::normalize_channel(input)
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        if rt::ctx().is_none() {
            rt::set_ctx(ctx); // sources can be created before init() in tests
        }
        match whisper::parse(&channel) {
            Some(whisper::Channel::Inbox) => whisper::Inbox::new(emit),
            Some(whisper::Channel::User(login)) => whisper::Conversation::new(login, emit),
            None => source::TwitchSource::new(channel, emit),
        }
    }

    fn routes(&self) -> Vec<(String, Handler)> {
        routes::routes()
    }

    fn accounts(&self) -> Option<Arc<dyn Accounts>> {
        Some(Arc::new(TwitchAccounts))
    }

    fn init(&self, ctx: Arc<Ctx>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            shared::reset();
            if let Some(old) = AUTH_LISTENER.lock().unwrap().take() {
                auth::EVENTS.off(old);
            }
            let id = auth::EVENTS.on(Arc::new(|_| {
                // A different user, or a fresh login: nothing cached for the old one applies.
                access::invalidate();
                helix::clear_user_cache();
                routes::clear_caches();
            }));
            *AUTH_LISTENER.lock().unwrap() = Some(id);
            auth::init(ctx);
            Ok(())
        })
    }
}
