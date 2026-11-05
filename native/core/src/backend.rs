//! Port of src/main/main.js, the parts that are not the window: the shared ctx, the relay and api router, the
//! accounts / settings / layout / updater / sounds handlers that main.js exposed over IPC, app info and
//! `openExternal`. Here they are the `AguBridge` implementation the UI calls directly.
//!
//! Skipped on purpose (the UI owns windows now): BrowserWindow creation, window.json tracking, pop-out
//! cards, always-on-top, the image animation policy, taskbar flashing, the `win:*` handlers, the single
//! instance lock and the menu.
//!
//! The backend owns a tokio runtime; every future it hands out is a `JoinHandle` wrapper, so it can be
//! awaited from any executor.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::log::Logger;
use crate::platforms::{modules, Ctx, Modules};
use crate::protocol::*;
use crate::relay::{Api, SourceManager};
use crate::sounds::Sounds;
use crate::store::Store;
use crate::updater::{GithubFeed, NoInstaller, Updater};
use crate::util::Listeners;

/// `%APPDATA%\Agu Chat` (what Electron's `app.getPath("userData")` is for this app) unless `AGU_DATA_DIR`
/// points elsewhere (dev runs on a copy).
pub fn default_data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("AGU_DATA_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Roaming")))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("Agu Chat")
}

/// What the app is: `app.getVersion()` and `app.isPackaged`.
#[derive(Clone, Debug)]
pub struct AppMeta {
    pub version: String,
    pub packaged: bool,
}

impl Default for AppMeta {
    fn default() -> Self {
        AppMeta {
            version: std::env::var("AGU_APP_VERSION").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
            packaged: std::env::var("AGU_PACKAGED").map(|v| v == "1").unwrap_or(!cfg!(debug_assertions)),
        }
    }
}

struct Inner {
    meta: AppMeta,
    ctx: Arc<Ctx>,
    store: Store,
    modules: Arc<Modules>,
    relay: SourceManager,
    api: Api,
    updater: Arc<Updater>,
    sounds: Sounds,
    frames: Arc<Listeners<(SubId, Frame)>>,
    accounts_cbs: Arc<Listeners<AccountsState>>,
    updater_cbs: Arc<Listeners<UpdaterState>>,
}

pub struct Backend {
    rt: Arc<tokio::runtime::Runtime>,
    inner: Arc<Inner>,
}

fn accounts_state(modules: &Modules) -> AccountsState {
    let mut out = AccountsState::new();
    for p in Platform::ALL {
        let st = match modules.get(&p).and_then(|m| m.accounts()) {
            Some(a) => a.state(),
            None => AccountState { connected: false, unsupported: true, ..Default::default() },
        };
        out.insert(p, st);
    }
    out
}

/// `userAgent`, as main.js builds it.
pub fn user_agent(version: &str) -> String {
    format!("AguChat/{version} (+https://github.com/TrinityTF/agu-chat)")
}

impl Backend {
    /// Starts the backend on `data_dir` (settings.json, layout.json, window.json, secrets.bin, Local State,
    /// sounds/, agu-chat.log). Creates its own tokio runtime.
    pub fn start(data_dir: PathBuf) -> Backend {
        Self::start_with(data_dir, AppMeta::default())
    }

    pub fn start_with(data_dir: PathBuf, meta: AppMeta) -> Backend {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("agu-core").build().expect("tokio runtime"),
        );
        let logger = Arc::new(Logger::new(&data_dir, meta.packaged));
        let store = Store::open(data_dir.clone(), logger.clone());
        let ctx = Arc::new(Ctx::new(store.settings.clone(), store.secrets.clone(), logger.clone(), user_agent(&meta.version)));
        let modules = Arc::new(modules());

        let frames: Arc<Listeners<(SubId, Frame)>> = Arc::new(Listeners::new());
        let accounts_cbs: Arc<Listeners<AccountsState>> = Arc::new(Listeners::new());
        let updater_cbs: Arc<Listeners<UpdaterState>> = Arc::new(Listeners::new());

        let f2 = frames.clone();
        let relay = SourceManager::new(modules.clone(), ctx.clone(), Arc::new(move |sub_id, frame| f2.emit(&(sub_id.clone(), frame.clone()))));
        let api = Api::new(&modules, ctx.clone());
        let updater = Updater::new(meta.version.clone(), meta.packaged, Arc::new(GithubFeed), Arc::new(NoInstaller), logger.clone());
        let u2 = updater_cbs.clone();
        updater.events.on(Arc::new(move |s: &UpdaterState| u2.emit(s)));

        // ctx.accountsChanged: broadcast the accounts state
        let (m2, a2) = (modules.clone(), accounts_cbs.clone());
        ctx.set_accounts_changed(Arc::new(move || a2.emit(&accounts_state(&m2))));

        let sounds = Sounds::new(&data_dir);
        let inner = Arc::new(Inner { meta: meta.clone(), ctx: ctx.clone(), store, modules: modules.clone(), relay, api, updater: updater.clone(), sounds, frames, accounts_cbs, updater_cbs });

        // storeEvents.on("settings", ...): the update channel follows the setting
        let u3 = updater.clone();
        inner.store.settings.events.on(Arc::new(move |s: &Settings| u3.set_channel(&s.update_channel)));

        // app.whenReady: every module's init(), then the updater
        let (ctx2, mods2, upd, channel) = (ctx.clone(), modules.clone(), updater.clone(), inner.store.settings.get_typed().update_channel);
        rt.block_on(async move {
            for m in mods2.values() {
                if let Err(e) = m.init(ctx2.clone()).await {
                    ctx2.log(&format!("[init] {} {e}", m.platform().as_str()));
                }
            }
            upd.init(&channel);
        });
        Backend { rt, inner }
    }

    pub fn data_dir(&self) -> &Path {
        self.inner.store.files.dir()
    }

    /// The stores, for the UI's own files (window.json) and for diagnostics.
    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn sounds_dir(&self) -> PathBuf {
        self.data_dir().join("sounds")
    }

    /// Runs `fut` on the backend runtime; the returned future completes with its output (or `fallback` if the
    /// task panicked or the runtime is gone).
    fn run<T: Send + 'static>(&self, fut: impl std::future::Future<Output = T> + Send + 'static, fallback: impl FnOnce() -> T + Send + 'static) -> BoxFuture<'static, T> {
        let h = self.rt.spawn(fut);
        Box::pin(async move { h.await.unwrap_or_else(|_| fallback()) })
    }

    /// Stops every source (the `window-all-closed` handler). Also runs on drop.
    pub fn shutdown(&self) {
        self.inner.relay.stop_all();
        let _g = self.rt.enter();
        crate::twitch::shared::reset();
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn shell_open(url: &str) {
    #[cfg(windows)]
    {
        use windows::core::{w, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            ShellExecuteW(None, w!("open"), PCWSTR(wide.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
        }
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(url).spawn();
    }
}

/// `/^https?:\/\//i`: the only links that ever leave the app.
pub fn is_openable_url(url: &str) -> bool {
    let l = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

impl AguBridge for Backend {
    fn subscribe(&self, source: SourceRef) -> BoxFuture<'static, Result<SubId, String>> {
        let inner = self.inner.clone();
        self.run(async move { inner.relay.subscribe(&source) }, || Err("backend stopped".into()))
    }

    fn unsubscribe(&self, sub_id: SubId) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        self.run(async move { inner.relay.unsubscribe(&sub_id) }, || ())
    }

    fn on_frame(&self, cb: Arc<dyn Fn(&SubId, &Frame) + Send + Sync>) -> Unsubscribe {
        let frames = self.inner.frames.clone();
        let id = frames.on(Arc::new(move |(sub, frame): &(SubId, Frame)| cb(sub, frame)));
        Box::new(move || frames.off(id))
    }

    fn api(&self, source: SourceRef, path: String, init: ApiInit) -> BoxFuture<'static, ApiResult> {
        let inner = self.inner.clone();
        self.run(async move { inner.api.call(source, &path, init).await }, || ApiResult { status: 500, body: serde_json::json!({"error": "backend stopped"}) })
    }

    fn accounts_get(&self) -> BoxFuture<'static, AccountsState> {
        let modules = self.inner.modules.clone();
        self.run(async move { accounts_state(&modules) }, AccountsState::new)
    }

    fn accounts_login(&self, platform: Platform, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let modules = self.inner.modules.clone();
        self.run(
            async move {
                let acc = modules.get(&platform).and_then(|m| m.accounts()).ok_or_else(|| format!("{} accounts are not supported.", platform.as_str()))?;
                acc.login(args).await
            },
            || Err("backend stopped".into()),
        )
    }

    fn accounts_cancel(&self, platform: Platform) -> BoxFuture<'static, ()> {
        let modules = self.inner.modules.clone();
        self.run(
            async move {
                if let Some(a) = modules.get(&platform).and_then(|m| m.accounts()) {
                    a.cancel();
                }
            },
            || (),
        )
    }

    fn accounts_logout(&self, platform: Platform) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        self.run(
            async move {
                if let Some(a) = inner.modules.get(&platform).and_then(|m| m.accounts()) {
                    a.logout().await;
                    inner.ctx.accounts_changed();
                }
            },
            || (),
        )
    }

    fn on_accounts(&self, cb: Arc<dyn Fn(&AccountsState) + Send + Sync>) -> Unsubscribe {
        let l = self.inner.accounts_cbs.clone();
        let id = l.on(cb);
        Box::new(move || l.off(id))
    }

    fn settings_get(&self) -> Settings {
        self.inner.store.settings.get_typed()
    }

    fn settings_set(&self, patch: Value) -> BoxFuture<'static, Settings> {
        let inner = self.inner.clone();
        let fallback = self.inner.store.settings.clone();
        self.run(async move { inner.store.settings.set(&patch) }, move || fallback.get_typed())
    }

    fn on_settings(&self, cb: Arc<dyn Fn(&Settings) + Send + Sync>) -> Unsubscribe {
        let inner = self.inner.clone();
        let id = inner.store.settings.events.on(cb);
        Box::new(move || inner.store.settings.events.off(id))
    }

    fn layout_get(&self) -> Option<Layout> {
        let v = self.inner.store.layout.get()?;
        match serde_json::from_value::<Layout>(v) {
            Ok(l) => Some(l),
            Err(e) => {
                self.inner.ctx.log(&format!("[store] layout.json is not a layout: {e}"));
                None
            }
        }
    }

    fn layout_set(&self, layout: Layout) {
        let res = serde_json::to_value(&layout).map_err(|e| e.to_string()).and_then(|v| self.inner.store.layout.set(&v).map_err(|e| e.to_string()));
        if let Err(e) = res {
            self.inner.ctx.log(&format!("[ipc] layout:set {e}"));
        }
    }

    fn updater_get(&self) -> UpdaterState {
        self.inner.updater.get_state()
    }

    fn updater_check(&self) -> BoxFuture<'static, ()> {
        let u = self.inner.updater.clone();
        self.run(async move { u.check() }, || ())
    }

    fn updater_install(&self) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        self.run(
            async move {
                if let Err(e) = inner.updater.install().await {
                    inner.ctx.log(&format!("[ipc] updater:install {e}"));
                }
            },
            || (),
        )
    }

    fn on_updater(&self, cb: Arc<dyn Fn(&UpdaterState) + Send + Sync>) -> Unsubscribe {
        let l = self.inner.updater_cbs.clone();
        let id = l.on(cb);
        Box::new(move || l.off(id))
    }

    fn app_info(&self) -> AppInfo {
        AppInfo { version: self.inner.meta.version.clone(), packaged: self.inner.meta.packaged }
    }

    fn open_external(&self, url: String) {
        if is_openable_url(&url) {
            shell_open(&url);
        }
    }

    fn sounds_import(&self, path: PathBuf) -> BoxFuture<'static, Result<String, String>> {
        let inner = self.inner.clone();
        self.run(async move { inner.sounds.import(&path) }, || Err("backend stopped".into()))
    }

    fn sounds_read(&self, file: String) -> BoxFuture<'static, Result<Vec<u8>, String>> {
        let inner = self.inner.clone();
        self.run(async move { inner.sounds.read(&file) }, || Err("backend stopped".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_links_open() {
        assert!(is_openable_url("https://example.com/x"));
        assert!(is_openable_url("HTTP://example.com"));
        assert!(!is_openable_url("file:///c:/windows/system32/calc.exe"));
        assert!(!is_openable_url("javascript:alert(1)"));
        assert!(!is_openable_url("ms-settings:"));
        assert!(!is_openable_url(""));
    }

    #[test]
    fn the_user_agent_is_honest() {
        assert_eq!(user_agent("0.0.1-beta.1"), "AguChat/0.0.1-beta.1 (+https://github.com/TrinityTF/agu-chat)");
    }
}
