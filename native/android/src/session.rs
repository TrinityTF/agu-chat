//! The Android app's Rust side: one `Session` owns the core backend and a chat store per open channel. Kotlin
//! talks to it with JSON commands (`call`) and hears back through JSON events (the `Sink`):
//!
//! - `rows`     `{sk, reset, upsert: [row], remove: [key]}`: rows to add or replace (in list order) and rows gone.
//!              `reset` means the list is exactly `upsert` (a snapshot, or the emotes changed every line).
//! - `conn`     `{sk, conn}`: the channel's connection state (`ConnFrame`).
//! - `accounts` the `AccountsState` map; `settings` the settings object.
//!
//! Frames are applied off the UI thread and the diffs are sent at most every `FLUSH_MS`, so a busy chat costs
//! the bridge a few calls a second, not one per message.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::protocol::{AguBridge, Platform, SourceRef, SubId};
use agu_chat_core::{AppMeta, Backend};
use agu_chat_model::mod_caps::relay_api;
use agu_chat_model::net::Net;
use agu_chat_model::store::{source_key, ChatStore};
use agu_chat_model::twitch::emotes::{flush_cache, init_cache, merge_emote_sets};
use agu_chat_model::twitch::use_twitch_data::{load_badges, load_cached_provider_sets, load_provider_sets, ProviderSets};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::view::{row_json, Env};

/// Events to the app: (kind, JSON payload). Called from background threads.
pub type Sink = Arc<dyn Fn(&str, &Value) + Send + Sync>;

const FLUSH_MS: u64 = 60;

struct Chan {
    source: SourceRef,
    store: ChatStore,
    sub: Option<SubId>,
    env: Env,
    /// what the app was last sent: row key -> the row's Arc address (a changed row is a new Arc)
    sent: HashMap<String, usize>,
    sent_order: Vec<String>,
    dirty: bool,
    /// everything must be sent again (snapshot replay, emotes or badges arrived)
    full: bool,
    emotes_loading: bool,
}

impl Chan {
    fn new(source: SourceRef) -> Self {
        let mut store = ChatStore::default();
        store.sync(std::slice::from_ref(&source));
        Chan { source, store, sub: None, env: Env::default(), sent: HashMap::new(), sent_order: Vec::new(), dirty: true, full: true, emotes_loading: false }
    }

    fn refresh_known(&mut self) {
        let sk = source_key(&self.source);
        if let Some(st) = self.store.source(&sk) {
            self.env.known = st.known.clone();
        }
    }

    /// The rows event since the last one, or None when nothing changed.
    fn diff(&mut self) -> Option<Value> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let rows = self.store.rows();
        let sk = source_key(&self.source);
        let now: Vec<(String, usize)> = rows.iter().map(|r| (r.key.clone(), Arc::as_ptr(r) as usize)).collect();
        let keys_now: HashSet<&str> = now.iter().map(|(k, _)| k.as_str()).collect();
        let removed: Vec<String> = self.sent_order.iter().filter(|k| !keys_now.contains(k.as_str())).cloned().collect();
        let new_count = now.iter().filter(|(k, _)| !self.sent.contains_key(k)).count();
        // new rows must all sit at the end of the list for an incremental update; history arriving in the middle
        // (or anything else unusual) sends the whole list
        let tail_ok = now.iter().skip(now.len() - new_count).all(|(k, _)| !self.sent.contains_key(k));
        let full = self.full || !tail_ok;
        self.full = false;
        let mut upsert = Vec::new();
        for (r, (k, p)) in rows.iter().zip(now.iter()) {
            if full || self.sent.get(k) != Some(p) {
                upsert.push(row_json(r, &self.env));
            }
        }
        self.sent = now.iter().cloned().collect();
        self.sent_order = now.into_iter().map(|(k, _)| k).collect();
        if !full && upsert.is_empty() && removed.is_empty() {
            return None;
        }
        Some(json!({"sk": sk, "reset": full, "upsert": upsert, "remove": if full { vec![] } else { removed }}))
    }
}

#[derive(Default)]
struct Routes {
    subs: HashMap<SubId, String>,
    pending: HashMap<SubId, Vec<Value>>,
    inflight: usize,
}

enum Inbox {
    Frame(String, Value),
    Reset(String),
}

struct Inner {
    bridge: Arc<dyn AguBridge>,
    backend: Arc<Backend>,
    net: Arc<Net>,
    sink: Sink,
    data_dir: PathBuf,
    chans: Mutex<HashMap<String, Chan>>,
    routes: Arc<Mutex<Routes>>,
    inbox: mpsc::UnboundedSender<Inbox>,
}

#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

fn platform_of(v: &Value) -> Result<Platform, String> {
    serde_json::from_value(v.get("platform").cloned().unwrap_or(Value::Null)).map_err(|_| "unknown platform".to_string())
}

fn str_arg<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(|s| s.as_str()).ok_or_else(|| format!("missing {k}"))
}

impl Session {
    pub fn start(data_dir: PathBuf, version: String, sink: Sink) -> Session {
        let _ = std::fs::create_dir_all(&data_dir);
        std::env::set_var("AGU_DATA_DIR", &data_dir);
        let backend = Arc::new(Backend::start_with(data_dir.clone(), AppMeta { version: version.clone(), packaged: true }));
        let bridge: Arc<dyn AguBridge> = backend.clone();
        let net = Net::new(&version);
        init_cache(data_dir.clone());
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner { bridge: bridge.clone(), backend, net: net.clone(), sink: sink.clone(), data_dir, chans: Mutex::new(HashMap::new()), routes: Arc::new(Mutex::new(Routes::default())), inbox: tx.clone() });

        // frames can arrive before `subscribe` answers with their id: hold them while a subscribe is in flight
        let routes = inner.routes.clone();
        let tx2 = tx.clone();
        std::mem::forget(bridge.on_frame(Arc::new(move |sub, frame| {
            let mut r = routes.lock().unwrap();
            if let Some(sk) = r.subs.get(sub) {
                let _ = tx2.send(Inbox::Frame(sk.clone(), frame.clone()));
            } else if r.inflight > 0 {
                let list = r.pending.entry(sub.clone()).or_default();
                if list.len() < 4096 {
                    list.push(frame.clone());
                }
            }
        })));
        let s2 = sink.clone();
        std::mem::forget(bridge.on_accounts(Arc::new(move |a| s2(&"accounts", &serde_json::to_value(a).unwrap_or(Value::Null)))));
        let s3 = sink.clone();
        std::mem::forget(bridge.on_settings(Arc::new(move |s| s3(&"settings", &serde_json::to_value(s).unwrap_or(Value::Null)))));

        let session = Session { inner };
        let s = session.clone();
        net.rt.spawn(async move { s.pump(rx).await });
        session
    }

    async fn pump(self, mut rx: mpsc::UnboundedReceiver<Inbox>) {
        let mut tick = tokio::time::interval(Duration::from_millis(FLUSH_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                m = rx.recv() => {
                    let Some(m) = m else { return };
                    self.take(m);
                    while let Ok(m) = rx.try_recv() {
                        self.take(m);
                    }
                }
                _ = tick.tick() => self.flush(),
            }
        }
    }

    fn take(&self, m: Inbox) {
        let mut chans = self.inner.chans.lock().unwrap();
        match m {
            Inbox::Reset(sk) => {
                if let Some(c) = chans.get_mut(&sk) {
                    c.store.reset_source(&sk);
                    c.dirty = true;
                    c.full = true;
                }
            }
            Inbox::Frame(sk, f) => {
                let Some(c) = chans.get_mut(&sk) else { return };
                if f.get("type").and_then(|t| t.as_str()) == Some("conn") {
                    (self.inner.sink)("conn", &json!({"sk": sk, "conn": f}));
                }
                let before = c.store.source(&sk).and_then(|s| s.channel_id.clone());
                if c.store.apply(&sk, &f) {
                    c.dirty = true;
                }
                let after = c.store.source(&sk).and_then(|s| s.channel_id.clone());
                if c.source.platform == Platform::Twitch && after.is_some() && after != before && c.env.emotes.is_none() && !c.emotes_loading {
                    c.emotes_loading = true;
                    let (s, cid) = (self.clone(), after.unwrap());
                    self.inner.net.rt.spawn(async move { s.load_emotes(sk, cid, false).await });
                }
            }
        }
    }

    fn flush(&self) {
        let events: Vec<Value> = {
            let mut chans = self.inner.chans.lock().unwrap();
            chans
                .values_mut()
                .filter_map(|c| {
                    if c.dirty {
                        c.refresh_known();
                    }
                    c.diff()
                })
                .collect()
        };
        for e in events {
            (self.inner.sink)("rows", &e);
        }
    }

    async fn load_badges_for(&self, sk: String, source: SourceRef) {
        let Some((map, cid)) = load_badges(&self.inner.bridge, &source, &sk).await else { return };
        let start_emotes = {
            let mut chans = self.inner.chans.lock().unwrap();
            let Some(c) = chans.get_mut(&sk) else { return };
            c.env.badges = Arc::new(map);
            c.dirty = true;
            c.full = true;
            let go = cid.is_some() && c.env.emotes.is_none() && !c.emotes_loading;
            if go {
                c.emotes_loading = true;
            }
            cid.filter(|_| go)
        };
        if let Some(cid) = start_emotes {
            self.load_emotes(sk, cid, false).await;
        }
    }

    fn apply_sets(&self, sk: &str, sets: &ProviderSets) {
        let merged = merge_emote_sets(&sets.seven, &sets.bttv, &sets.ffz);
        let mut chans = self.inner.chans.lock().unwrap();
        if let Some(c) = chans.get_mut(sk) {
            c.env.emotes = Some(Arc::new(merged));
            c.dirty = true;
            c.full = true;
        }
    }

    /// 7TV / BTTV / FFZ for a Twitch channel: the cached sets at once, then fresh ones.
    async fn load_emotes(&self, sk: String, channel_id: String, reload: bool) {
        let client = self.inner.net.client.clone();
        if !reload {
            if let Some(sets) = load_cached_provider_sets(client.clone(), channel_id.clone()).await {
                self.apply_sets(&sk, &sets);
            }
        }
        let (sets, _fresh) = load_provider_sets(client, channel_id, reload).await;
        self.apply_sets(&sk, &sets);
        flush_cache();
        if let Some(c) = self.inner.chans.lock().unwrap().get_mut(&sk) {
            c.emotes_loading = false;
        }
    }

    async fn open(&self, source: SourceRef) -> Result<Value, String> {
        let sk = source_key(&source);
        let fresh = {
            let mut chans = self.inner.chans.lock().unwrap();
            match chans.get_mut(&sk) {
                Some(c) => {
                    c.dirty = true;
                    c.full = true;
                    false
                }
                None => {
                    chans.insert(sk.clone(), Chan::new(source.clone()));
                    true
                }
            }
        };
        if !fresh {
            return Ok(json!({"sk": sk}));
        }
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source.clone()).await;
        {
            let mut r = self.inner.routes.lock().unwrap();
            r.inflight -= 1;
            match &res {
                Ok(id) => {
                    r.subs.insert(id.clone(), sk.clone());
                    let _ = self.inner.inbox.send(Inbox::Reset(sk.clone()));
                    for f in r.pending.remove(id).unwrap_or_default() {
                        let _ = self.inner.inbox.send(Inbox::Frame(sk.clone(), f));
                    }
                }
                Err(_) => {}
            }
            if r.inflight == 0 {
                r.pending.clear();
            }
        }
        let id = match res {
            Ok(id) => id,
            Err(e) => {
                self.inner.chans.lock().unwrap().remove(&sk);
                return Err(e);
            }
        };
        if let Some(c) = self.inner.chans.lock().unwrap().get_mut(&sk) {
            c.sub = Some(id);
        }
        if source.platform == Platform::Twitch {
            let s = self.clone();
            let (sk2, src2) = (sk.clone(), source.clone());
            self.inner.net.rt.spawn(async move { s.load_badges_for(sk2, src2).await });
        }
        Ok(json!({"sk": sk}))
    }

    async fn close(&self, sk: &str) {
        let sub = self.inner.chans.lock().unwrap().remove(sk).and_then(|c| c.sub);
        if let Some(id) = sub {
            self.inner.routes.lock().unwrap().subs.remove(&id);
            self.inner.bridge.unsubscribe(id).await;
        }
    }

    fn source_of(&self, sk: &str) -> Result<SourceRef, String> {
        self.inner.chans.lock().unwrap().get(sk).map(|c| c.source.clone()).ok_or_else(|| "that chat is not open".to_string())
    }

    async fn send(&self, sk: &str, text: &str, reply_to: Option<&str>) -> Result<Value, String> {
        let source = self.source_of(sk)?;
        let path = match source.platform {
            Platform::Twitch => "/api/chat/send",
            Platform::Rumble => "/api/rumble/send",
            Platform::Kick => "/api/kick/send",
            Platform::X => "/api/x/send",
            Platform::Youtube => "/api/youtube/send",
        };
        let mut body = json!({"text": text});
        if let Some(r) = reply_to.filter(|r| !r.is_empty()) {
            body["replyTo"] = json!(r);
        }
        let r = relay_api(&self.inner.bridge, &source, path, Some("POST"), Some(body)).await;
        if r.ok {
            return Ok(json!({"ok": true}));
        }
        let msg = match &r.body {
            Value::Object(o) => o.get("error").map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.to_string())).unwrap_or_default(),
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        Err(if r.status == 403 && msg == "relogin" {
            "Log in again to grant chat permissions".into()
        } else if msg.is_empty() {
            "send failed".into()
        } else {
            msg
        })
    }

    pub fn call(&self, method: String, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let s = self.clone();
        Box::pin(async move {
            let b = &s.inner.bridge;
            match method.as_str() {
                "app.info" => Ok(serde_json::to_value(b.app_info()).unwrap_or(Value::Null)),
                "accounts.get" => Ok(serde_json::to_value(b.accounts_get().await).unwrap_or(Value::Null)),
                "accounts.login" => b.accounts_login(platform_of(&args)?, args.get("args").cloned().unwrap_or(json!({}))).await,
                "accounts.cancel" => {
                    b.accounts_cancel(platform_of(&args)?).await;
                    Ok(Value::Null)
                }
                "accounts.logout" => {
                    b.accounts_logout(platform_of(&args)?).await;
                    Ok(Value::Null)
                }
                "settings.get" => Ok(serde_json::to_value(b.settings_get()).unwrap_or(Value::Null)),
                "settings.set" => Ok(serde_json::to_value(b.settings_set(args.get("patch").cloned().unwrap_or(json!({}))).await).unwrap_or(Value::Null)),
                "tabs.get" => Ok(std::fs::read(s.inner.data_dir.join("mobile-tabs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!([]))),
                "tabs.set" => {
                    let tabs = args.get("tabs").cloned().unwrap_or(json!([]));
                    let tmp = s.inner.data_dir.join("mobile-tabs.json.tmp");
                    std::fs::write(&tmp, serde_json::to_vec(&tabs).unwrap_or_default()).map_err(|e| e.to_string())?;
                    std::fs::rename(&tmp, s.inner.data_dir.join("mobile-tabs.json")).map_err(|e| e.to_string())?;
                    Ok(Value::Null)
                }
                "chat.open" => {
                    let channel = str_arg(&args, "channel")?.trim().trim_start_matches('#').to_string();
                    if channel.is_empty() {
                        return Err("enter a channel".into());
                    }
                    s.open(SourceRef { platform: platform_of(&args)?, channel }).await
                }
                "chat.close" => {
                    s.close(str_arg(&args, "sk")?).await;
                    Ok(Value::Null)
                }
                "chat.send" => s.send(str_arg(&args, "sk")?, str_arg(&args, "text")?, args.get("replyTo").and_then(|r| r.as_str())).await,
                "chat.reloadEmotes" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let cid = s.inner.chans.lock().unwrap().get(&sk).and_then(|c| c.store.source(&sk).and_then(|st| st.channel_id.clone()));
                    if let Some(cid) = cid {
                        s.load_emotes(sk, cid, true).await;
                    }
                    Ok(Value::Null)
                }
                // the relay's whole API (moderation, user cards, ...): {sk, path, method?, body?}
                "api" => {
                    let source = s.source_of(str_arg(&args, "sk")?)?;
                    let r = relay_api(b, &source, str_arg(&args, "path")?, args.get("method").and_then(|m| m.as_str()), args.get("body").cloned()).await;
                    Ok(json!({"ok": r.ok, "status": r.status, "body": r.body}))
                }
                "open.url" => {
                    b.open_external(str_arg(&args, "url")?.to_string());
                    Ok(Value::Null)
                }
                other => Err(format!("unknown call {other}")),
            }
        })
    }

    /// Run a call on the session's runtime; `done` gets the answer (on a background thread).
    pub fn call_then(&self, method: String, args: Value, done: impl FnOnce(Result<Value, String>) + Send + 'static) {
        let fut = self.call(method, args);
        self.inner.net.rt.spawn(async move { done(fut.await) });
    }

    pub fn shutdown(&self) {
        flush_cache();
        self.inner.backend.shutdown();
    }
}
