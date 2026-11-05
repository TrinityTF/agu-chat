//! Port of src/main/platforms.js: every platform module, keyed by platform. Also holds the "Platform module"
//! contract from docs/ARCHITECTURE.md (`PlatformModule`, `Source`, `Accounts`) and the shared `Ctx` that
//! main.js builds and hands to the modules.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::future::BoxFuture;
use serde_json::{json, Value};

use crate::log::Logger;
use crate::protocol::{AccountState, Frame, Platform};
use crate::relay::Handler;
use crate::store::{SecretStore, SettingsStore};

/// `emit({type:"conn", ...})` and friends: the callback a source pushes frames into.
pub type Emit = Arc<dyn Fn(Frame) + Send + Sync>;

/// What a module may use from the app (main.js's `ctx`).
pub struct Ctx {
    pub settings: Arc<SettingsStore>,
    pub secrets: Arc<SecretStore>,
    pub logger: Arc<Logger>,
    pub user_agent: String,
    accounts_changed: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl Ctx {
    pub fn new(settings: Arc<SettingsStore>, secrets: Arc<SecretStore>, logger: Arc<Logger>, user_agent: String) -> Self {
        Ctx { settings, secrets, logger, user_agent, accounts_changed: Mutex::new(None) }
    }
    pub fn log(&self, msg: &str) {
        self.logger.log(msg);
    }
    /// Set once by the backend: broadcasts the accounts state to the UI.
    pub fn set_accounts_changed(&self, f: Arc<dyn Fn() + Send + Sync>) {
        *self.accounts_changed.lock().unwrap() = Some(f);
    }
    pub fn accounts_changed(&self) {
        let f = self.accounts_changed.lock().unwrap().clone();
        if let Some(f) = f {
            f();
        }
    }
}

/// One platform's chat for one channel.
pub trait Source: Send + Sync {
    /// Open upstream; emit `{type:"conn", state:"connecting"}` first.
    fn start(self: Arc<Self>);
    /// Close everything, clear timers.
    fn stop(&self);
    /// The frames a NEW subscriber needs (usually one chatlog plus the latest conn).
    fn snapshot(&self) -> Vec<Frame>;
}

pub trait Accounts: Send + Sync {
    fn state(&self) -> AccountState;
    fn login(&self, args: Value) -> BoxFuture<'static, Result<Value, String>>;
    fn cancel(&self);
    fn logout(&self) -> BoxFuture<'static, ()>;
}

pub trait PlatformModule: Send + Sync {
    fn platform(&self) -> Platform;
    /// Errs with the message to show the user.
    fn normalize_channel(&self, input: &str) -> Result<String, String>;
    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source>;
    /// `"METHOD /path"` (or `"* /path"`) -> handler
    fn routes(&self) -> Vec<(String, Handler)> {
        Vec::new()
    }
    fn accounts(&self) -> Option<Arc<dyn Accounts>> {
        None
    }
    fn init(&self, _ctx: Arc<Ctx>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

pub type Modules = HashMap<Platform, Arc<dyn PlatformModule>>;

pub fn modules() -> Modules {
    let list: Vec<Arc<dyn PlatformModule>> = vec![
        Arc::new(crate::twitch::TwitchModule),
        Arc::new(crate::kick::Kick),
        Arc::new(crate::youtube::Youtube),
        Arc::new(crate::rumble::Rumble),
        Arc::new(crate::x::X),
    ];
    list.into_iter().map(|m| (m.platform(), m)).collect()
}

/// The source of a platform whose backend is not ported yet (kick, youtube, rumble, x): it only ever says so.
/// A later round replaces each of those modules with the real port.
pub struct UnavailableSource {
    emit: Emit,
}

impl UnavailableSource {
    pub fn new(emit: Emit) -> Arc<dyn Source> {
        Arc::new(UnavailableSource { emit })
    }
    fn conn() -> Frame {
        json!({"type": "conn", "state": "unavailable", "detail": "Coming in the native port"})
    }
}

impl Source for UnavailableSource {
    fn start(self: Arc<Self>) {
        (self.emit)(Self::conn());
    }
    fn stop(&self) {}
    fn snapshot(&self) -> Vec<Frame> {
        vec![Self::conn()]
    }
}
