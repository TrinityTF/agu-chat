//! The Android app's Rust side: one `Session` owns the core backend and a chat store per open view (tab). Kotlin
//! talks to it with JSON commands (`call`) and hears back through JSON events (the `Sink`):
//!
//! - `rows`     `{vk, reset, upsert: [row], remove: [key]}`: a view's rows to add or replace (in list order) and rows
//!              gone. `reset` means the list is exactly `upsert` (a snapshot, or the emotes changed every line).
//! - `conn`     `{sk, conn}`: a channel's connection state (`ConnFrame`).
//! - `activity` `{vk, kind}`: new messages in a view ("message", or "highlight" for a mention / highlight rule).
//! - `alert`    `{sound: {file, volume} | null, vibrate}`: play the sound / vibrate for a highlight.
//! - `mentions` `{unread, total}`: the bell's count (the list is the `mentions.list` call).
//! - `accounts` the `AccountsState` map; `settings` the settings object.
//! - `login.webview` / `login.show` / `login.close` / `login.forget`: the cookie logins' sign-in screen (`login.rs`).
//!
//! A view holds one or more channels from any platform, merged into one list. A channel shown in several views is
//! subscribed once (`hub.rs`); its frames go to every view that holds it. Frames are applied off the UI thread and
//! the diffs are sent at most every `FLUSH_MS`, so a busy chat costs the bridge a few calls a second.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agu_chat_core::protocol::{AccountsState, AguBridge, Platform, SourceRef};
use agu_chat_core::twitch::whisper;
use agu_chat_core::{AppMeta, Backend};
use agu_chat_model::alerts::ping_samples;
use agu_chat_model::ignores::IgnoreCache;
use agu_chat_model::mentions::{classify, mention_id, row_deleted, Alert, Mentions};
use agu_chat_model::mod_caps::relay_api;
use agu_chat_model::net::Net;
use agu_chat_model::store::{source_key, ChatStore};
use agu_chat_model::twitch::badges::BadgeMap;
use agu_chat_model::twitch::emotes::{flush_cache, init_cache, merge_emote_sets, EmoteSets};
use agu_chat_model::twitch::use_twitch_data::{load_badges, load_cached_provider_sets, load_provider_sets, ProviderSets};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::alerts::{Alerter, Wants};
use crate::hub::{Added, Hub};
use crate::view::{row_json, Env, Rules};

/// Events to the app: (kind, JSON payload). Called from background threads.
pub type Sink = Arc<dyn Fn(&str, &Value) + Send + Sync>;

const FLUSH_MS: u64 = 60;

/// What is known of one channel, whichever views show it.
#[derive(Default)]
struct SrcData {
    badges: Arc<BadgeMap>,
    emotes: Option<Arc<EmoteSets>>,
    emotes_loading: bool,
    /// the last `conn` frame, to tell a view that opens on a channel that is already connected
    conn: Option<Value>,
}

/// One tab's list: a store merging all its channels, and what the app was last sent of it.
struct View {
    store: ChatStore,
    /// what the app was last sent: row key -> the row's Arc address (a changed row is a new Arc)
    sent: HashMap<String, usize>,
    sent_order: Vec<String>,
    ignore_cache: IgnoreCache,
    dirty: bool,
    /// everything must be sent again (snapshot replay, emotes or badges arrived, settings changed)
    full: bool,
    /// rows up to this arrival number were looked at for alerts
    seen_ord: u64,
}

impl View {
    fn new() -> Self {
        View { store: ChatStore::default(), sent: HashMap::new(), sent_order: Vec::new(), ignore_cache: IgnoreCache::default(), dirty: true, full: true, seen_ord: 0 }
    }

    /// The rows event since the last one, or None when nothing changed.
    fn diff(&mut self, vk: &str, rules: &Rules, src: &HashMap<String, SrcData>) -> Option<Value> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let rows = rules.shown(self.store.rows(), &mut self.ignore_cache);
        let now: Vec<(String, usize)> = rows.iter().map(|r| (r.key.clone(), Arc::as_ptr(r) as usize)).collect();
        let keys_now: HashSet<&str> = now.iter().map(|(k, _)| k.as_str()).collect();
        let removed: Vec<String> = self.sent_order.iter().filter(|k| !keys_now.contains(k.as_str())).cloned().collect();
        let new_count = now.iter().filter(|(k, _)| !self.sent.contains_key(k)).count();
        // new rows must all sit at the end of the list for an incremental update; history arriving in the middle
        // (or anything else unusual) sends the whole list
        let tail_ok = now.iter().skip(now.len() - new_count).all(|(k, _)| !self.sent.contains_key(k));
        let full = self.full || !tail_ok;
        self.full = false;
        let mut envs: HashMap<String, Env> = HashMap::new();
        let mut upsert = Vec::new();
        for (r, (k, p)) in rows.iter().zip(now.iter()) {
            if full || self.sent.get(k) != Some(p) {
                let env = envs.entry(r.sk.clone()).or_insert_with(|| env_of(&r.sk, src, Some(&self.store)));
                let mut v = row_json(r, env);
                rules.decorate(&mut v, r);
                upsert.push(v);
            }
        }
        self.sent = now.iter().cloned().collect();
        self.sent_order = now.into_iter().map(|(k, _)| k).collect();
        if !full && upsert.is_empty() && removed.is_empty() {
            return None;
        }
        Some(json!({"vk": vk, "reset": full, "upsert": upsert, "remove": if full { vec![] } else { removed }}))
    }
}

fn env_of(sk: &str, src: &HashMap<String, SrcData>, store: Option<&ChatStore>) -> Env {
    let mut env = Env::default();
    if let Some(d) = src.get(sk) {
        env.badges = d.badges.clone();
        env.emotes = d.emotes.clone();
    }
    if let Some(st) = store.and_then(|s| s.source(sk)) {
        env.known = st.known.clone();
    }
    env
}

fn whisper_or_channel(s: &SourceRef) -> String {
    if s.platform == Platform::Twitch {
        if let Some(login) = whisper::login_of(&s.channel) {
            return login;
        }
    }
    s.channel.clone()
}

struct State {
    hub: Hub,
    views: HashMap<String, View>,
    src: HashMap<String, SrcData>,
    rules: Rules,
    alerter: Alerter,
    mentions: Mentions,
    mentions_open: bool,
    /// the (unread, total) the app was last told
    mentions_sent: (usize, usize),
    /// the app is on screen (an alert vibrates only in the background)
    active: bool,
}

type Out = Vec<(&'static str, Value)>;

impl State {
    fn new() -> State {
        State { hub: Hub::default(), views: HashMap::new(), src: HashMap::new(), rules: Rules::default(), alerter: Alerter::default(), mentions: Mentions::default(), mentions_open: false, mentions_sent: (0, 0), active: true }
    }

    /// Everything shown depends on these (settings, accounts): draw all rows again.
    fn redraw_all(&mut self) {
        for v in self.views.values_mut() {
            v.dirty = true;
            v.full = true;
            v.ignore_cache = IgnoreCache::default();
        }
    }

    fn touch_sk(&mut self, sk: &str) {
        for vk in self.hub.views_of(sk) {
            if let Some(v) = self.views.get_mut(&vk) {
                v.dirty = true;
                v.full = true;
            }
        }
    }

    /// Alerts, tab activity and the mentions list for the rows that arrived since the view was last looked at.
    fn scan(&mut self, vk: &str, out: &mut Out) {
        let State { hub, views, rules, alerter, mentions, mentions_open, active, .. } = self;
        let Some(view) = views.get_mut(vk) else { return };
        let now = Instant::now();
        let mut max_ord = view.seen_ord;
        let mut activity: Option<&str> = None;
        for r in view.store.rows().iter() {
            if r.ord <= view.seen_ord {
                continue;
            }
            max_ord = max_ord.max(r.ord);
            let (own, hidden) = (rules.is_own(r), rules.hidden(r));
            let Some(alert) = classify(r, own, hidden, rules.me_login(), rules.kick_me(), || rules.rule_of(r)) else { continue };
            activity = activity.or(Some("message"));
            if alert.red() {
                activity = Some("highlight");
                let channel = hub.source(&r.sk).map(|s| whisper_or_channel(&s)).unwrap_or_default();
                mentions.add(vk, &channel, r.clone(), *mentions_open);
            }
            let wants = match alert {
                Alert::Plain => continue,
                Alert::Mention => Wants { sound: rules.settings.mention_sound, sound_file: String::new(), flash: rules.settings.mention_flash },
                Alert::Rule(rule) => Wants { sound: rule.sound, sound_file: rule.sound_file.clone(), flash: rule.flash },
            };
            if let Some(f) = alerter.once(&mention_id(r), &wants, &rules.settings, *active, now) {
                out.push(("alert", json!({"sound": f.sound.map(|(file, volume)| json!({"file": file, "volume": volume})), "vibrate": f.vibrate})));
            }
        }
        view.seen_ord = max_ord;
        if let Some(kind) = activity {
            out.push(("activity", json!({"vk": vk, "kind": kind})));
        }
        // mentions whose message was deleted since leave the list
        if !mentions.is_empty() {
            let gone: Vec<String> = view.store.rows().iter().filter(|r| row_deleted(r)).map(|r| mention_id(r)).collect();
            if !gone.is_empty() {
                mentions.remove(&gone);
            }
        }
    }

    /// A `mentions` event when the bell's numbers changed.
    fn mentions_event(&mut self, out: &mut Out) {
        let now = (self.mentions.unread(), self.mentions.len());
        if now != self.mentions_sent {
            self.mentions_sent = now;
            out.push(("mentions", json!({"unread": now.0, "total": now.1})));
        }
    }
}

#[derive(Default)]
struct Routes {
    /// the live subscriptions: sub id -> the source key their frames are for
    subs: HashMap<String, String>,
    pending: HashMap<String, Vec<Value>>,
    inflight: usize,
}

enum Inbox {
    /// a frame of a channel: to every view holding it, or only to `only` (a view's replayed snapshot)
    Frame { sk: String, only: Option<String>, f: Value },
    Reset(String),
}

struct Inner {
    bridge: Arc<dyn AguBridge>,
    backend: Arc<Backend>,
    net: Arc<Net>,
    sink: Sink,
    data_dir: PathBuf,
    st: Mutex<State>,
    routes: Arc<Mutex<Routes>>,
    inbox: mpsc::UnboundedSender<Inbox>,
    login: crate::login::LoginBridge,
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

/// `[{platform, channel}]` -> sources. Twitch and Kick names are case-insensitive, so they are kept lowercase: the
/// same channel typed two ways is one source.
fn channels_arg(v: &Value) -> Result<Vec<SourceRef>, String> {
    let list = v.get("channels").and_then(|c| c.as_array()).ok_or("missing channels")?;
    let mut out = Vec::new();
    for c in list {
        let platform = platform_of(c)?;
        let mut channel = c.get("channel").and_then(|s| s.as_str()).unwrap_or("").trim().trim_start_matches('#').to_string();
        if matches!(platform, Platform::Twitch | Platform::Kick) {
            channel = channel.to_lowercase();
        }
        if channel.is_empty() {
            return Err("enter a channel".into());
        }
        out.push(SourceRef { platform, channel });
    }
    if out.is_empty() {
        return Err("enter a channel".into());
    }
    Ok(out)
}

/// The saved tabs, in the current shape `[{name, channels: [{platform, channel}]}]`. The first Android versions saved
/// `[{platform, channel}]` (one channel per tab).
pub fn migrate_tabs(v: Value) -> Value {
    let Some(list) = v.as_array() else { return json!([]) };
    Value::Array(
        list.iter()
            .filter_map(|t| {
                if let Some(chs) = t.get("channels").and_then(|c| c.as_array()) {
                    let chs: Vec<Value> = chs.iter().filter(|c| c.get("platform").is_some() && c.get("channel").is_some()).cloned().collect();
                    (!chs.is_empty()).then(|| json!({"name": t.get("name").and_then(|n| n.as_str()).unwrap_or(""), "channels": chs}))
                } else if t.get("platform").is_some() && t.get("channel").is_some() {
                    Some(json!({"name": "", "channels": [{"platform": t["platform"], "channel": t["channel"]}]}))
                } else {
                    None
                }
            })
            .collect(),
    )
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
        let mut state = State::new();
        state.rules = Rules::new(bridge.settings_get(), AccountsState::new());
        let inner = Arc::new(Inner { bridge: bridge.clone(), backend, net: net.clone(), sink: sink.clone(), data_dir, st: Mutex::new(state), routes: Arc::new(Mutex::new(Routes::default())), inbox: tx.clone(), login: crate::login::LoginBridge::new(sink.clone()) });
        inner.login.install();

        // frames can arrive before `subscribe` answers with their id: hold them while a subscribe is in flight
        let routes = inner.routes.clone();
        let tx2 = tx.clone();
        std::mem::forget(bridge.on_frame(Arc::new(move |sub, frame| {
            let mut r = routes.lock().unwrap();
            if let Some(sk) = r.subs.get(sub) {
                let _ = tx2.send(Inbox::Frame { sk: sk.clone(), only: None, f: frame.clone() });
            } else if r.inflight > 0 {
                let list = r.pending.entry(sub.clone()).or_default();
                if list.len() < 4096 {
                    list.push(frame.clone());
                }
            }
        })));
        let (i2, s2) = (inner.clone(), sink.clone());
        std::mem::forget(bridge.on_accounts(Arc::new(move |a| {
            {
                let mut st = i2.st.lock().unwrap();
                let settings = st.rules.settings.clone();
                st.rules = Rules::new(settings, a.clone());
                st.redraw_all();
            }
            s2(&"accounts", &serde_json::to_value(a).unwrap_or(Value::Null))
        })));
        let (i3, s3) = (inner.clone(), sink.clone());
        std::mem::forget(bridge.on_settings(Arc::new(move |s| {
            {
                let mut st = i3.st.lock().unwrap();
                let accounts = st.rules.accounts.clone();
                st.rules = Rules::new(s.clone(), accounts);
                st.redraw_all();
            }
            s3(&"settings", &serde_json::to_value(s).unwrap_or(Value::Null))
        })));

        let session = Session { inner };
        let s = session.clone();
        net.rt.spawn(async move { s.pump(rx).await });
        // the accounts are needed to tell your own lines and your name apart
        let s = session.clone();
        net.rt.spawn(async move {
            let a = s.inner.bridge.accounts_get().await;
            let mut st = s.inner.st.lock().unwrap();
            let settings = st.rules.settings.clone();
            st.rules = Rules::new(settings, a);
            st.redraw_all();
        });
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

    fn emit(&self, out: Out) {
        for (kind, v) in out {
            (self.inner.sink)(kind, &v);
        }
    }

    fn take(&self, m: Inbox) {
        let mut out: Out = Vec::new();
        {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            match m {
                Inbox::Reset(sk) => {
                    for vk in st.hub.views_of(&sk) {
                        if let Some(v) = st.views.get_mut(&vk) {
                            v.store.reset_source(&sk);
                            v.dirty = true;
                            v.full = true;
                        }
                    }
                }
                Inbox::Frame { sk, only, f } => {
                    if f.get("type").and_then(|t| t.as_str()) == Some("conn") {
                        if only.is_none() {
                            // a replayed snapshot's conn is the one the app already has
                            out.push(("conn", json!({"sk": sk, "conn": f})));
                        }
                        st.src.entry(sk.clone()).or_default().conn = Some(f.clone());
                    }
                    let platform = st.hub.source(&sk).map(|s| s.platform);
                    let mut emotes_for: Vec<String> = Vec::new();
                    for vk in st.hub.views_of(&sk) {
                        if only.as_deref().is_some_and(|o| o != vk) {
                            continue;
                        }
                        let Some(v) = st.views.get_mut(&vk) else { continue };
                        let before = v.store.source(&sk).and_then(|s| s.channel_id.clone());
                        if v.store.apply(&sk, &f) {
                            v.dirty = true;
                        }
                        let after = v.store.source(&sk).and_then(|s| s.channel_id.clone());
                        if platform == Some(Platform::Twitch) && after.is_some() && after != before {
                            emotes_for.extend(after);
                        }
                    }
                    if let Some(cid) = emotes_for.into_iter().next() {
                        let d = st.src.entry(sk.clone()).or_default();
                        if d.emotes.is_none() && !d.emotes_loading {
                            d.emotes_loading = true;
                            let s = self.clone();
                            self.inner.net.rt.spawn(async move { s.load_emotes(sk, cid, false).await });
                        }
                    }
                }
            }
        }
        self.emit(out);
    }

    fn flush(&self) {
        let mut out: Out = Vec::new();
        {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            let vks: Vec<String> = st.views.iter().filter(|(_, v)| v.dirty).map(|(k, _)| k.clone()).collect();
            for vk in vks {
                st.scan(&vk, &mut out);
                let State { views, rules, src, .. } = st;
                if let Some(e) = views.get_mut(&vk).and_then(|v| v.diff(&vk, rules, src)) {
                    out.push(("rows", e));
                }
            }
            st.mentions_event(&mut out);
        }
        self.emit(out);
    }

    async fn load_badges_for(&self, sk: String, source: SourceRef) {
        let Some((map, cid)) = load_badges(&self.inner.bridge, &source, &sk).await else { return };
        let start_emotes = {
            let mut st = self.inner.st.lock().unwrap();
            let d = st.src.entry(sk.clone()).or_default();
            d.badges = Arc::new(map);
            let go = cid.is_some() && d.emotes.is_none() && !d.emotes_loading;
            if go {
                d.emotes_loading = true;
            }
            st.touch_sk(&sk);
            cid.filter(|_| go)
        };
        if let Some(cid) = start_emotes {
            self.load_emotes(sk, cid, false).await;
        }
    }

    fn apply_sets(&self, sk: &str, sets: &ProviderSets) {
        let merged = merge_emote_sets(&sets.seven, &sets.bttv, &sets.ffz);
        let mut st = self.inner.st.lock().unwrap();
        if !st.hub.is_held(sk) {
            return;
        }
        st.src.entry(sk.to_string()).or_default().emotes = Some(Arc::new(merged));
        st.touch_sk(sk);
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
        if let Some(d) = self.inner.st.lock().unwrap().src.get_mut(&sk) {
            d.emotes_loading = false;
        }
    }

    /// Subscribe a source nobody held, for good: its frames go to every view that holds it.
    async fn subscribe_new(&self, source: SourceRef) -> Result<(), String> {
        let sk = source_key(&source);
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source.clone()).await;
        {
            let mut r = self.inner.routes.lock().unwrap();
            r.inflight -= 1;
            if let Ok(id) = &res {
                r.subs.insert(id.clone(), sk.clone());
                let _ = self.inner.inbox.send(Inbox::Reset(sk.clone()));
                for f in r.pending.remove(id).unwrap_or_default() {
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: None, f });
                }
            }
            if r.inflight == 0 {
                r.pending.clear();
            }
        }
        let id = res?;
        // every view that held it may have closed meanwhile
        let held = {
            let mut st = self.inner.st.lock().unwrap();
            let held = st.hub.is_held(&sk);
            if held {
                st.hub.set_sub(&sk, id.clone());
            }
            held
        };
        if !held {
            self.end_sub(Some(id)).await;
            return Ok(());
        }
        if source.platform == Platform::Twitch {
            let s = self.clone();
            self.inner.net.rt.spawn(async move { s.load_badges_for(sk, source).await });
        }
        Ok(())
    }

    /// A view opened on a channel another view already holds: replay its current state into that view only. The
    /// replay is a short-lived second subscription (the core sends the snapshot to each new subscriber).
    async fn replay_into(&self, source: SourceRef, vk: String) -> Result<(), String> {
        let sk = source_key(&source);
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source).await;
        {
            let mut r = self.inner.routes.lock().unwrap();
            r.inflight -= 1;
            if let Ok(id) = &res {
                for f in r.pending.remove(id).unwrap_or_default() {
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: Some(vk.clone()), f });
                }
            }
            if r.inflight == 0 {
                r.pending.clear();
            }
        }
        let id = res?;
        self.end_sub(Some(id)).await;
        Ok(())
    }

    async fn end_sub(&self, id: Option<String>) {
        if let Some(id) = id {
            self.inner.routes.lock().unwrap().subs.remove(&id);
            self.inner.bridge.unsubscribe(id).await;
        }
    }

    /// Make a view hold exactly `channels` (creating it when `vk` is new). Returns `{vk, sks}`. A channel that
    /// cannot be opened fails the call and is left out, unless `lenient` (restoring saved tabs): then it stays and
    /// shows its error as its connection state.
    async fn set_view(&self, vk: Option<String>, channels: Vec<SourceRef>, lenient: bool) -> Result<Value, String> {
        let vk = vk.filter(|v| !v.is_empty()).unwrap_or_else(|| format!("v{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        let (change, before, out) = {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            let before = st.hub.sources_of(&vk);
            let change = st.hub.set_view(&vk, &channels);
            let sources = st.hub.sources_of(&vk);
            let view = st.views.entry(vk.clone()).or_insert_with(View::new);
            view.store.sync(&sources);
            view.dirty = true;
            view.full = true;
            let mut out: Out = Vec::new();
            for (sk, _) in &change.released {
                st.src.remove(sk);
            }
            for a in &change.added {
                if let Added::Held(s) = a {
                    // another view already has the channel: the app already knows its connection
                    if let Some(c) = st.src.get(&source_key(s)).and_then(|d| d.conn.clone()) {
                        out.push(("conn", json!({"sk": source_key(s), "conn": c})));
                    }
                }
            }
            (change, before, out)
        };
        self.emit(out);
        for (_, sub) in &change.released {
            self.end_sub(sub.clone()).await;
        }
        let mut failed: Vec<(String, String)> = Vec::new();
        for a in &change.added {
            let res = match a {
                Added::New(s) => self.subscribe_new(s.clone()).await,
                Added::Held(s) => self.replay_into(s.clone(), vk.clone()).await,
            };
            if let Err(e) = res {
                failed.push((source_key(a.source()), e));
            }
        }
        if !failed.is_empty() {
            if lenient {
                for (sk, e) in &failed {
                    let f = json!({"type": "conn", "state": "error", "detail": e, "canSend": false, "readOnlyReason": e});
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: None, f });
                }
            } else {
                // leave the view as it was (a new view is not created)
                if before.is_empty() {
                    self.close_view(&vk).await;
                } else {
                    let _ = Box::pin(self.set_view(Some(vk.clone()), before, true)).await;
                }
                return Err(failed.remove(0).1);
            }
        }
        let sks: Vec<String> = self.inner.st.lock().unwrap().hub.sources_of(&vk).iter().map(source_key).collect();
        Ok(json!({"vk": vk, "sks": sks}))
    }

    async fn close_view(&self, vk: &str) {
        let released = {
            let mut st = self.inner.st.lock().unwrap();
            st.views.remove(vk);
            let r = st.hub.close_view(vk);
            for (sk, _) in &r {
                st.src.remove(sk);
            }
            r
        };
        for (_, sub) in released {
            self.end_sub(sub).await;
        }
    }

    fn source_of(&self, sk: &str) -> Result<SourceRef, String> {
        self.inner.st.lock().unwrap().hub.source(sk).ok_or_else(|| "that chat is not open".to_string())
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

    /// The Mentions screen's list, newest first.
    fn mentions_list(&self) -> Value {
        let st = self.inner.st.lock().unwrap();
        let list: Vec<Value> = st
            .mentions
            .entries()
            .iter()
            .rev()
            .map(|e| {
                let env = env_of(&e.row.sk, &st.src, None);
                let mut row = row_json(&e.row, &env);
                st.rules.decorate(&mut row, &e.row);
                json!({"id": e.id, "vk": e.leaf_id, "sk": e.row.sk, "channel": e.channel, "platform": e.row.platform.as_str(), "ts": e.row.ts, "read": e.read, "whisper": agu_chat_model::mentions::is_whisper_key(&e.row.sk), "row": row})
            })
            .collect();
        Value::Array(list)
    }

    pub fn call(&self, method: String, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let s = self.clone();
        Box::pin(async move {
            let b = &s.inner.bridge;
            match method.as_str() {
                "app.info" => Ok(serde_json::to_value(b.app_info()).unwrap_or(Value::Null)),
                // the app is on screen or in the background
                "app.active" => {
                    s.inner.st.lock().unwrap().active = args.get("active").and_then(|a| a.as_bool()).unwrap_or(true);
                    Ok(Value::Null)
                }
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
                // the sign-in screen's answer to a `login.webview` / `login.show` event
                "login.result" => s.inner.login.answer(&args).map(|_| Value::Null),
                "settings.get" => Ok(serde_json::to_value(b.settings_get()).unwrap_or(Value::Null)),
                "settings.set" => Ok(serde_json::to_value(b.settings_set(args.get("patch").cloned().unwrap_or(json!({}))).await).unwrap_or(Value::Null)),
                "tabs.get" => Ok(migrate_tabs(std::fs::read(s.inner.data_dir.join("mobile-tabs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!([])))),
                "tabs.set" => {
                    let tabs = migrate_tabs(args.get("tabs").cloned().unwrap_or(json!([])));
                    let tmp = s.inner.data_dir.join("mobile-tabs.json.tmp");
                    std::fs::write(&tmp, serde_json::to_vec(&tabs).unwrap_or_default()).map_err(|e| e.to_string())?;
                    std::fs::rename(&tmp, s.inner.data_dir.join("mobile-tabs.json")).map_err(|e| e.to_string())?;
                    Ok(Value::Null)
                }
                // {vk?, channels: [{platform, channel}], lenient?} -> {vk, sks}: open a view, or change what it holds
                "view.open" | "view.set" => {
                    let vk = args.get("vk").and_then(|v| v.as_str()).map(|v| v.to_string());
                    if method == "view.set" && !vk.as_deref().is_some_and(|v| s.inner.st.lock().unwrap().hub.has_view(v)) {
                        return Err("that tab is not open".into());
                    }
                    let lenient = args.get("lenient").and_then(|l| l.as_bool()).unwrap_or(false);
                    s.set_view(vk, channels_arg(&args)?, lenient).await
                }
                "view.close" => {
                    s.close_view(str_arg(&args, "vk")?).await;
                    Ok(Value::Null)
                }
                "chat.send" => s.send(str_arg(&args, "sk")?, str_arg(&args, "text")?, args.get("replyTo").and_then(|r| r.as_str())).await,
                "chat.reloadEmotes" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let cid = {
                        let st = s.inner.st.lock().unwrap();
                        st.hub.views_of(&sk).iter().find_map(|vk| st.views.get(vk).and_then(|v| v.store.source(&sk).and_then(|x| x.channel_id.clone())))
                    };
                    if let Some(cid) = cid {
                        s.load_emotes(sk, cid, true).await;
                    }
                    Ok(Value::Null)
                }
                // the Mentions screen: opening it marks everything read (the bell's count goes), closing it ends that
                "mentions.open" => {
                    let open = args.get("open").and_then(|o| o.as_bool()).unwrap_or(true);
                    let mut out: Out = Vec::new();
                    {
                        let mut st = s.inner.st.lock().unwrap();
                        st.mentions_open = open;
                        if open {
                            st.mentions.mark_all_read();
                        }
                        st.mentions_event(&mut out);
                    }
                    s.emit(out);
                    Ok(Value::Null)
                }
                "mentions.list" => Ok(s.mentions_list()),
                // the built-in alert sound: 16-bit mono PCM
                "alerts.ping" => {
                    let rate = 44100u32;
                    let pcm: Vec<i32> = ping_samples(rate).into_iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i32).collect();
                    Ok(json!({"rate": rate, "pcm": pcm}))
                }
                // the relay's whole API (moderation, user cards, ...): {sk, path, method?, body?}
                "api" => {
                    let source = s.source_of(str_arg(&args, "sk")?)?;
                    let r = relay_api(b, &source, str_arg(&args, "path")?, args.get("method").and_then(|m| m.as_str()), args.get("body").cloned()).await;
                    Ok(json!({"ok": r.ok, "status": r.status, "body": r.body}))
                }
                // debuggable builds only (the app checks): feed a frame to an open chat as if the platform sent it,
                // to see real rows without a busy channel
                "dev.frame" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let frame = args.get("frame").cloned().ok_or("missing frame")?;
                    let _ = s.inner.inbox.send(Inbox::Frame { sk, only: None, f: frame });
                    Ok(Value::Null)
                }
                // debuggable builds only: pretend this Twitch account is logged in (to see your name highlighted)
                "dev.me" => {
                    let login = str_arg(&args, "login")?.to_string();
                    let mut st = s.inner.st.lock().unwrap();
                    let mut accounts = st.rules.accounts.clone();
                    if let Ok(a) = serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": login, "name": login}})) {
                        accounts.insert(Platform::Twitch, a);
                    }
                    let settings = st.rules.settings.clone();
                    st.rules = Rules::new(settings, accounts);
                    st.redraw_all();
                    Ok(Value::Null)
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

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Settings;

    #[test]
    fn single_channel_tabs_from_the_first_versions_become_one_channel_lists() {
        let old = json!([{"platform": "twitch", "channel": "a_g_u__"}, {"platform": "kick", "channel": "agu"}]);
        let v = migrate_tabs(old);
        assert_eq!(
            v,
            json!([
                {"name": "", "channels": [{"platform": "twitch", "channel": "a_g_u__"}]},
                {"name": "", "channels": [{"platform": "kick", "channel": "agu"}]},
            ])
        );
        // the new shape passes through, junk is dropped
        let new = json!([{"name": "Me", "channels": [{"platform": "twitch", "channel": "a"}, {"platform": "kick", "channel": "a"}]}, {"x": 1}, {"name": "empty", "channels": []}]);
        assert_eq!(migrate_tabs(new.clone()), json!([new[0].clone()]));
        assert_eq!(migrate_tabs(json!("nope")), json!([]));
    }

    #[test]
    fn channels_are_normalised_and_checked() {
        let c = channels_arg(&json!({"channels": [{"platform": "twitch", "channel": "#A_G_U__"}, {"platform": "youtube", "channel": "AbC"}]})).unwrap();
        assert_eq!(c[0].channel, "a_g_u__");
        assert_eq!(c[1].channel, "AbC", "YouTube ids keep their case");
        assert!(channels_arg(&json!({"channels": []})).is_err());
        assert!(channels_arg(&json!({"channels": [{"platform": "twitch", "channel": " "}]})).is_err());
        assert!(channels_arg(&json!({"channels": [{"platform": "nope", "channel": "a"}]})).is_err());
    }

    fn state_with_two_views() -> State {
        let mut st = State::new();
        let a = SourceRef { platform: Platform::Twitch, channel: "agu".into() };
        for vk in ["v1", "v2"] {
            let c = st.hub.set_view(vk, std::slice::from_ref(&a));
            assert!(c.created);
            let mut v = View::new();
            v.store.sync(std::slice::from_ref(&a));
            st.views.insert(vk.into(), v);
        }
        st
    }

    fn feed(st: &mut State, sk: &str, f: &Value) {
        for vk in st.hub.views_of(sk) {
            let v = st.views.get_mut(&vk).unwrap();
            v.dirty |= v.store.apply(sk, f);
        }
    }

    fn chat(id: &str, text: &str) -> Value {
        json!({"type": "chat", "msg": {"id": id, "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": text, "emotes": "", "badges": ""}})
    }

    #[test]
    fn a_frame_reaches_every_view_with_the_channel_and_alerts_once() {
        let mut st = state_with_two_views();
        let mut s: Settings = serde_json::from_value(json!({"highlightRules": [{"id": "r", "enabled": true, "kind": "phrase", "pattern": "gg", "caseSensitive": false, "color": "#ff9f1a", "sound": true, "soundFile": "", "flash": false}]})).unwrap();
        s.highlight_sound_focused = true;
        st.rules = Rules::new(s, AccountsState::new());
        feed(&mut st, "twitch:agu", &chat("1", "gg"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        st.scan("v2", &mut out);
        let alerts = out.iter().filter(|(k, _)| *k == "alert").count();
        assert_eq!(alerts, 1, "the message is in two tabs but rings once");
        let acts: Vec<&Value> = out.iter().filter(|(k, _)| *k == "activity").map(|(_, v)| v).collect();
        assert_eq!(acts.len(), 2);
        assert!(acts.iter().all(|a| a["kind"] == "highlight"));
        assert_eq!(st.mentions.len(), 1, "one mention however many tabs");
        // both views draw the row with the rule's colour
        for vk in ["v1", "v2"] {
            let State { views, rules, src, .. } = &mut st;
            let e = views.get_mut(vk).unwrap().diff(vk, rules, src).expect("rows");
            assert_eq!(e["upsert"][0]["highlight"]["color"], "#ff9f1a");
        }
    }

    #[test]
    fn closing_a_view_leaves_the_other_one_its_rows() {
        let mut st = state_with_two_views();
        feed(&mut st, "twitch:agu", &chat("1", "hi"));
        st.views.remove("v1");
        assert!(st.hub.close_view("v1").is_empty());
        feed(&mut st, "twitch:agu", &chat("2", "again"));
        let State { views, rules, src, .. } = &mut st;
        let e = views.get_mut("v2").unwrap().diff("v2", rules, src).unwrap();
        assert_eq!(e["upsert"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_merged_view_lists_both_platforms_in_one_list_with_their_platform() {
        let mut st = State::new();
        let (t, k) = (SourceRef { platform: Platform::Twitch, channel: "agu".into() }, SourceRef { platform: Platform::Kick, channel: "agu".into() });
        st.hub.set_view("v", &[t.clone(), k.clone()]);
        let mut v = View::new();
        v.store.sync(&[t, k]);
        st.views.insert("v".into(), v);
        feed(&mut st, "twitch:agu", &chat("1", "from twitch"));
        feed(&mut st, "kick:agu", &json!({"type": "chat", "msg": {"id": "k1", "ts": 2, "text": "from kick", "user": {"id": "3", "login": "kim", "name": "Kim", "color": "#00ff00"}}}));
        let State { views, rules, src, .. } = &mut st;
        let e = views.get_mut("v").unwrap().diff("v", rules, src).unwrap();
        let platforms: Vec<&str> = e["upsert"].as_array().unwrap().iter().map(|r| r["platform"].as_str().unwrap()).collect();
        assert_eq!(platforms.len(), 2);
        assert!(platforms.contains(&"twitch") && platforms.contains(&"kick"));
    }

    #[test]
    fn your_name_goes_to_the_mentions_list_and_the_bell() {
        let mut st = state_with_two_views();
        let mut acc = AccountsState::new();
        acc.insert(Platform::Twitch, serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": "agu", "name": "Agu"}})).unwrap());
        st.rules = Rules::new(Settings::default(), acc);
        feed(&mut st, "twitch:agu", &chat("1", "hello @agu"));
        feed(&mut st, "twitch:agu", &chat("2", "plain"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        st.mentions_event(&mut out);
        assert_eq!(st.mentions.len(), 1);
        let bell = out.iter().find(|(k, _)| *k == "mentions").map(|(_, v)| v.clone()).unwrap();
        assert_eq!(bell, json!({"unread": 1, "total": 1}));
        st.mentions.mark_all_read();
        st.mentions_event(&mut out);
        assert_eq!(out.last().unwrap().1, json!({"unread": 0, "total": 1}));
    }
}
