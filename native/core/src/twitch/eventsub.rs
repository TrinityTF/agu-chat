//! Port of src/main/twitch/eventsub.js: ONE EventSub WebSocket session for the whole app, carrying
//! subscriptions for every open Twitch source the user may moderate. Ported from render/lib/eventsub.js, which
//! ran one session per channel on the server; Twitch caps WebSocket connections at 3 per client+user, and one
//! session can hold many subscriptions (300), so here it is one session, many channels.
//!
//! Per attached source (a channel the user broadcasts or moderates) it subscribes to the named mod feed
//! (channel.moderate v2), AutoMod holds, and suspicious-user events, all with moderator_user_id = the user. On
//! the user's OWN channel it also hears channel-point redeems and Bits Power-ups (IRC never sees a redeem
//! without text).
//!
//! The same session also carries the user's whispers (`user.whisper.message`, keyed under WHISPER_KEY in `subs`)
//! for as long as whisper.rs wants them, so they arrive with no Twitch channel open too.
//!
//! It never sends anything on the socket (Twitch closes with 4001 on client traffic).
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::access::{access as get_access, invalidate as invalidate_access};
use super::auth;
use super::helix::helix;
use super::rt::{log, user_agent};
use super::source::TwitchSource;
use super::whisper;
use crate::util::Timer;
use crate::ws::{self, WsEvent, WsHandle};

const WS_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 60_000;
const DEDUPE_CAP: usize = 500;
/// on top of Twitch's own keepalive_timeout_seconds
const WELCOME_GRACE_MS: u64 = 5000;
const RETRY_SOURCE_MS: u64 = 45_000;
/// Twitch's per-WebSocket cap
const MAX_SUBSCRIPTIONS: usize = 300;

/// `AGU_EVENTSUB_URL` points a test at a fake server; the real endpoint otherwise.
fn ws_url() -> String {
    std::env::var("AGU_EVENTSUB_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| WS_URL.to_string())
}

const MOD_SUBS: [(&str, &str); 5] = [
    ("channel.moderate", "2"),
    ("automod.message.hold", "2"),
    ("automod.message.update", "2"),
    ("channel.suspicious_user.message", "1"),
    ("channel.suspicious_user.update", "1"),
];
const REDEEM_SUB: (&str, &str) = ("channel.channel_points_custom_reward_redemption.add", "1");
const POWER_UP_SUB: (&str, &str) = ("channel.custom_power_up_redemption.add", "1");
const WHISPER_SUB: (&str, &str) = (whisper::EVENT_TYPE, "1");
/// Where the whisper subscription sits in `subs` / `retry_timers`: a channel id is all digits, so it cannot clash.
const WHISPER_KEY: &str = "@whispers";

// Scope precheck, so a token that predates one of these scopes does not make Twitch refuse subscriptions one
// by one: the 4 scopes below plus one of read/manage per channel.moderate category.
const REQUIRED_SCOPES: [&str; 4] = ["moderator:manage:automod", "moderator:read:suspicious_users", "moderator:read:moderators", "moderator:read:vips"];
const ONE_OF_SCOPE_PAIRS: [[&str; 2]; 6] = [
    ["moderator:read:blocked_terms", "moderator:manage:blocked_terms"],
    ["moderator:read:chat_settings", "moderator:manage:chat_settings"],
    ["moderator:read:unban_requests", "moderator:manage:unban_requests"],
    ["moderator:read:banned_users", "moderator:manage:banned_users"],
    ["moderator:read:chat_messages", "moderator:manage:chat_messages"],
    ["moderator:read:warnings", "moderator:manage:warnings"],
];

pub fn has_required_scopes(scope_list: &[String]) -> bool {
    let has = |s: &str| scope_list.iter().any(|x| x == s);
    REQUIRED_SCOPES.iter().all(|s| has(s)) && ONE_OF_SCOPE_PAIRS.iter().all(|pair| pair.iter().any(|s| has(s)))
}

/// Routes one decoded event to the owning source's handler. A malformed event must never kill the session.
pub fn dispatch(source: &TwitchSource, sub_type: &str, event: &Value) {
    match sub_type {
        "channel.moderate" => source.handle_moderate(event),
        "automod.message.hold" => source.handle_automod_hold(event),
        "automod.message.update" => source.handle_automod_update(event),
        "channel.suspicious_user.message" => source.handle_suspicious_message(event),
        "channel.suspicious_user.update" => source.handle_suspicious_update(event),
        t if t == REDEEM_SUB.0 => source.handle_redeem(event),
        t if t == POWER_UP_SUB.0 => source.handle_power_up(event),
        _ => {}
    }
}

struct Sock {
    id: u64,
    handle: WsHandle,
    /// a session_reconnect handoff socket that has not been adopted yet (`ws.__reconnect`)
    handoff: bool,
}

struct State {
    attached: HashMap<String, Arc<TwitchSource>>, // channelId -> source
    subs: HashMap<String, HashMap<String, String>>, // channelId -> (type -> subscription id)
    /// whisper.rs wants the user's whispers, with or without attached channels
    whispers: bool,
    ws: Option<Sock>,
    reconnect_socket: Option<Sock>, // session_reconnect handoff in flight
    ws_seq: u64,
    session_id: Option<String>,
    session_user: Option<String>, // user id the session's subscriptions belong to
    keepalive_sec: u64,
    watchdog: Timer,
    reconnect_timer: Timer,
    reconnect_delay: u64,
    retry_timers: HashMap<String, Timer>, // channelId -> timer
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
}

pub struct EventSubManager {
    st: Mutex<State>,
    auth_listener: Mutex<Option<u64>>,
}

impl EventSubManager {
    /// Must be called inside the tokio runtime.
    pub fn new() -> Arc<Self> {
        let m = Arc::new(EventSubManager {
            st: Mutex::new(State {
                attached: HashMap::new(),
                subs: HashMap::new(),
                whispers: false,
                ws: None,
                reconnect_socket: None,
                ws_seq: 0,
                session_id: None,
                session_user: None,
                keepalive_sec: 10,
                watchdog: Timer::new(),
                reconnect_timer: Timer::new(),
                reconnect_delay: RECONNECT_BASE_MS,
                retry_timers: HashMap::new(),
                seen: HashSet::new(),
                seen_order: VecDeque::new(),
            }),
            auth_listener: Mutex::new(None),
        });
        let weak = Arc::downgrade(&m);
        let id = auth::EVENTS.on(Arc::new(move |_| {
            if let Some(m) = weak.upgrade() {
                m.on_account_change();
            }
        }));
        *m.auth_listener.lock().unwrap() = Some(id);
        m
    }

    pub fn shutdown(&self) {
        if let Some(id) = self.auth_listener.lock().unwrap().take() {
            auth::EVENTS.off(id);
        }
        self.close();
    }

    pub fn status(&self) -> Value {
        let s = self.st.lock().unwrap();
        json!({
            "connected": s.session_id.is_some(),
            "sessionId": s.session_id,
            "attached": s.attached.keys().collect::<Vec<_>>(),
            "subs": s.subs.iter().map(|(id, m)| (id.clone(), m.keys().cloned().collect::<Vec<_>>())).collect::<HashMap<_, _>>(),
        })
    }

    // ---------- source lifecycle ----------

    pub async fn attach(self: &Arc<Self>, source: Arc<TwitchSource>) {
        let Some(id) = source.channel_id() else { return };
        self.st.lock().unwrap().attached.insert(id, source.clone());
        self.evaluate(source).await;
    }

    pub fn detach(&self, source: &Arc<TwitchSource>) {
        let Some(id) = source.channel_id() else { return };
        let mut s = self.st.lock().unwrap();
        if !s.attached.get(&id).map(|x| Arc::ptr_eq(x, source)).unwrap_or(false) {
            return;
        }
        s.attached.remove(&id);
        if let Some(mut t) = s.retry_timers.remove(&id) {
            t.clear();
        }
        let mine = s.subs.remove(&id);
        if s.attached.is_empty() && !s.whispers {
            Self::close_locked(&mut s);
        }
        drop(s);
        // Free the subscriptions on Twitch's side; cheap and best-effort (they die with the socket anyway).
        if let Some(mine) = mine {
            for sub_id in mine.into_values() {
                if sub_id.is_empty() {
                    continue;
                }
                tokio::spawn(async move {
                    let _ = helix("/eventsub/subscriptions", "DELETE", crate::q! {"id" => sub_id}, None).await;
                });
            }
        }
    }

    fn on_account_change(self: &Arc<Self>) {
        let rec = auth::current();
        let uid = rec.map(|r| r.user.id.clone());
        let sources: Vec<Arc<TwitchSource>> = {
            let mut s = self.st.lock().unwrap();
            if uid != s.session_user {
                Self::close_locked(&mut s);
                s.session_user = uid;
                drop(s);
                invalidate_access();
                self.st.lock().unwrap().attached.values().cloned().collect()
            } else {
                s.attached.values().cloned().collect()
            }
        };
        for source in sources {
            let me = self.clone();
            tokio::spawn(async move { me.evaluate(source).await });
        }
        if self.st.lock().unwrap().whispers {
            let me = self.clone();
            tokio::spawn(async move { me.evaluate_whispers().await });
        }
    }

    // ---------- whispers ----------

    /// Starts or stops listening for the user's whispers (whisper.rs calls it as conversations come and go).
    pub fn set_whispers(self: &Arc<Self>, on: bool) {
        let (changed, stale) = {
            let mut s = self.st.lock().unwrap();
            let changed = s.whispers != on;
            s.whispers = on;
            let mut stale = None;
            if changed && !on {
                if let Some(mut t) = s.retry_timers.remove(WHISPER_KEY) {
                    t.clear();
                }
                stale = s.subs.remove(WHISPER_KEY).and_then(|m| m.into_values().next());
                if s.attached.is_empty() {
                    Self::close_locked(&mut s);
                }
            }
            (changed, stale)
        };
        if let Some(sub_id) = stale.filter(|i| !i.is_empty()) {
            tokio::spawn(async move {
                let _ = helix("/eventsub/subscriptions", "DELETE", crate::q! {"id" => sub_id}, None).await;
            });
        }
        if changed && on {
            let me = self.clone();
            tokio::spawn(async move { me.evaluate_whispers().await });
        }
    }

    // A login from before whispers has no scope for them: then the session is not opened for this alone.
    async fn evaluate_whispers(self: &Arc<Self>) {
        let Some(rec) = auth::current() else { return };
        if !whisper::has_scopes(&rec.scopes) {
            return;
        }
        {
            let mut s = self.st.lock().unwrap();
            if !s.whispers {
                return;
            }
            if s.session_user.is_none() {
                s.session_user = Some(rec.user.id.clone());
            }
        }
        self.ensure_session();
        if self.st.lock().unwrap().session_id.is_some() {
            self.subscribe_whispers().await;
        }
    }

    async fn subscribe_whispers(self: &Arc<Self>) {
        let session_id = self.st.lock().unwrap().session_id.clone();
        let (Some(session_id), Some(rec)) = (session_id, auth::current()) else { return };
        {
            let s = self.st.lock().unwrap();
            if !s.whispers || s.subs.get(WHISPER_KEY).map(|m| m.contains_key(WHISPER_SUB.0)).unwrap_or(false) {
                return;
            }
        }
        if !whisper::has_scopes(&rec.scopes) {
            return;
        }
        let body = json!({"type": WHISPER_SUB.0, "version": WHISPER_SUB.1, "condition": {"user_id": rec.user.id}, "transport": {"method": "websocket", "session_id": session_id}});
        let result = helix("/eventsub/subscriptions", "POST", Vec::new(), Some(body)).await;
        let mut s = self.st.lock().unwrap();
        if s.session_id.as_deref() != Some(session_id.as_str()) || !s.whispers {
            return;
        }
        match result {
            Ok(v) => {
                s.subs.entry(WHISPER_KEY.into()).or_default().insert(WHISPER_SUB.0.into(), v["data"][0]["id"].as_str().unwrap_or("").to_string());
            }
            Err(err) if err.status == 409 => {
                s.subs.entry(WHISPER_KEY.into()).or_default().insert(WHISPER_SUB.0.into(), String::new()); // already on this session
            }
            Err(err) => {
                log(&format!("eventsub: whispers failed ({}) {}", err.status, err.message));
                self.retry_whispers_later(&mut s);
            }
        }
    }

    fn retry_whispers_later(self: &Arc<Self>, s: &mut State) {
        if s.retry_timers.contains_key(WHISPER_KEY) {
            return;
        }
        let me = self.clone();
        let mut t = Timer::new();
        t.set(Duration::from_millis(RETRY_SOURCE_MS), async move {
            me.st.lock().unwrap().retry_timers.remove(WHISPER_KEY);
            me.evaluate_whispers().await;
        });
        s.retry_timers.insert(WHISPER_KEY.into(), t);
    }

    fn is_attached(&self, id: &str, source: &Arc<TwitchSource>) -> bool {
        self.st.lock().unwrap().attached.get(id).map(|x| Arc::ptr_eq(x, source)).unwrap_or(false)
    }

    // Decides what this user may hear on this channel, sets the source's status, and subscribes.
    async fn evaluate(self: &Arc<Self>, source: Arc<TwitchSource>) {
        let Some(id) = source.channel_id() else { return };
        let Some(rec) = auth::current() else { return source.set_esub(false, Some("relogin"), None) };
        {
            let mut s = self.st.lock().unwrap();
            if s.session_user.is_none() {
                s.session_user = Some(rec.user.id.clone());
            }
        }
        let acc = get_access(&id).await;
        if !self.is_attached(&id, &source) {
            return;
        }
        if !(acc.is_broadcaster || acc.is_mod == Some(true)) {
            if acc.is_mod == Some(false) {
                return source.set_esub(false, None, None); // simply not a mod here: nothing to show
            }
            return source.set_esub(false, Some(if rec.scopes.iter().any(|s| s == "user:read:moderated_channels") { "error" } else { "relogin" }), None);
        }
        if !has_required_scopes(&rec.scopes) {
            return source.set_esub(false, Some("relogin"), None);
        }
        let have_core = self.st.lock().unwrap().subs.get(&id).map(|m| m.contains_key(MOD_SUBS[0].0)).unwrap_or(false);
        if !have_core {
            source.set_esub(false, Some("connecting"), None);
        }
        self.ensure_session();
        let has_session = self.st.lock().unwrap().session_id.is_some();
        if has_session {
            self.subscribe_source(source).await;
        }
    }

    fn retry_later(self: &Arc<Self>, source: &Arc<TwitchSource>) {
        let Some(id) = source.channel_id() else { return };
        let mut s = self.st.lock().unwrap();
        if s.retry_timers.contains_key(&id) || !s.attached.get(&id).map(|x| Arc::ptr_eq(x, source)).unwrap_or(false) {
            return;
        }
        let (me, src, cid) = (self.clone(), source.clone(), id.clone());
        let mut t = Timer::new();
        t.set(Duration::from_millis(RETRY_SOURCE_MS), async move {
            me.st.lock().unwrap().retry_timers.remove(&cid);
            if me.is_attached(&cid, &src) {
                me.evaluate(src).await;
            }
        });
        s.retry_timers.insert(id, t);
    }

    // ---------- session ----------

    fn ensure_session(self: &Arc<Self>) {
        let mut s = self.st.lock().unwrap();
        if s.ws.is_some() || s.reconnect_timer.is_set() {
            return;
        }
        self.connect_locked(&mut s);
    }

    fn make_sock(self: &Arc<Self>, s: &mut State, url: String, handoff: bool) -> Sock {
        s.ws_seq += 1;
        let id = s.ws_seq;
        let me = self.clone();
        let handle = ws::connect(url, user_agent(), move |ev| me.on_ws(id, ev));
        Sock { id, handle, handoff }
    }

    fn connect_locked(self: &Arc<Self>, s: &mut State) {
        let sock = self.make_sock(s, ws_url(), false);
        s.ws = Some(sock);
    }

    fn on_ws(self: &Arc<Self>, id: u64, ev: WsEvent) {
        match ev {
            WsEvent::Open => {}
            WsEvent::Message(data) => self.on_message(id, &data),
            WsEvent::Close(..) | WsEvent::Error(_) => {
                if let WsEvent::Error(e) = &ev {
                    log(&format!("eventsub socket error: {e}"));
                }
                self.drop_socket(id);
            }
        }
    }

    fn drop_socket(self: &Arc<Self>, id: u64) {
        let mut s = self.st.lock().unwrap();
        if s.reconnect_socket.as_ref().map(|k| k.id == id).unwrap_or(false) {
            // The handoff socket died before its welcome. The old one is still alive and subscribed; its own
            // close (or the watchdog) drives a normal reconnect.
            s.reconnect_socket = None;
            return;
        }
        if !s.ws.as_ref().map(|k| k.id == id).unwrap_or(false) {
            return; // superseded by a completed handoff
        }
        s.ws = None;
        let sources = Self::session_lost_locked(&mut s);
        self.schedule_reconnect_locked(&mut s);
        drop(s);
        for (src, reason) in sources {
            src.set_esub(false, Some(&reason), None);
        }
    }

    /// The session is gone, so every subscription on it is too. Returns what to tell the sources (they are
    /// told after the lock is released).
    fn session_lost_locked(s: &mut State) -> Vec<(Arc<TwitchSource>, String)> {
        s.watchdog.clear();
        s.session_id = None;
        s.subs.clear();
        s.attached
            .values()
            .filter(|src| {
                let (active, reason) = src.esub_state();
                active || reason.as_deref() == Some("connecting")
            })
            .map(|src| (src.clone(), "error".to_string()))
            .collect()
    }

    fn close_locked(s: &mut State) {
        s.watchdog.clear();
        s.reconnect_timer.clear();
        s.reconnect_delay = RECONNECT_BASE_MS;
        if let Some(k) = s.ws.take() {
            k.handle.close();
        }
        if let Some(k) = s.reconnect_socket.take() {
            k.handle.close();
        }
        s.session_id = None;
        s.subs.clear();
    }

    fn close(&self) {
        let mut s = self.st.lock().unwrap();
        Self::close_locked(&mut s);
    }

    /// Something wants the session: an attached channel or the whispers.
    fn wanted_locked(s: &State) -> bool {
        !s.attached.is_empty() || s.whispers
    }

    fn schedule_reconnect_locked(self: &Arc<Self>, s: &mut State) {
        if s.reconnect_timer.is_set() || !Self::wanted_locked(s) {
            return;
        }
        let me = self.clone();
        let delay = s.reconnect_delay;
        s.reconnect_timer.set(Duration::from_millis(delay), async move {
            let mut s = me.st.lock().unwrap();
            s.reconnect_timer.clear();
            if Self::wanted_locked(&s) {
                me.connect_locked(&mut s);
            }
        });
        s.reconnect_delay = (s.reconnect_delay * 2).min(RECONNECT_MAX_MS);
    }

    fn reset_watchdog_locked(self: &Arc<Self>, s: &mut State) {
        let me = self.clone();
        let ms = (if s.keepalive_sec == 0 { 10 } else { s.keepalive_sec }) * 1000 + WELCOME_GRACE_MS;
        s.watchdog.set(Duration::from_millis(ms), async move { me.on_watchdog() });
    }

    // Nothing (not even a keepalive) within keepalive_timeout + grace: assume the connection is dead.
    fn on_watchdog(self: &Arc<Self>) {
        let mut s = self.st.lock().unwrap();
        s.ws = None; // dropping the socket ends it
        let sources = Self::session_lost_locked(&mut s);
        self.schedule_reconnect_locked(&mut s);
        drop(s);
        for (src, reason) in sources {
            src.set_esub(false, Some(&reason), None);
        }
    }

    fn on_message(self: &Arc<Self>, ws_id: u64, data: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(data) else { return };
        let Some(kind) = msg["metadata"]["message_type"].as_str().filter(|k| !k.is_empty()) else { return };
        let is_main = {
            let mut s = self.st.lock().unwrap();
            let known = s.ws.as_ref().map(|k| k.id == ws_id).unwrap_or(false) || s.reconnect_socket.as_ref().map(|k| k.id == ws_id).unwrap_or(false);
            if !known {
                return; // a socket we let go of (removeAllListeners in JS)
            }
            // Traffic on the adopted socket resets the watchdog; a handoff socket only counts once adopted.
            let main = s.ws.as_ref().map(|k| k.id == ws_id).unwrap_or(false);
            if main {
                self.reset_watchdog_locked(&mut s);
            }
            main
        };
        match kind {
            "session_welcome" => {
                let me = self.clone();
                tokio::spawn(async move { me.on_welcome(ws_id, msg).await });
            }
            "notification" => {
                if is_main {
                    self.on_notification(&msg);
                }
            }
            "session_reconnect" => self.on_session_reconnect(&msg),
            "revocation" => self.on_revocation(&msg),
            _ => {}
        }
    }

    async fn on_welcome(self: &Arc<Self>, ws_id: u64, msg: Value) {
        let session = &msg["payload"]["session"];
        let Some(sid) = session["id"].as_str().filter(|s| !s.is_empty()).map(String::from) else { return };
        let keepalive = crate::util::js_to_number(session.get("keepalive_timeout_seconds"));
        let sources: Vec<Arc<TwitchSource>> = {
            let mut s = self.st.lock().unwrap();
            if s.reconnect_socket.as_ref().map(|k| k.id == ws_id && k.handoff).unwrap_or(false) {
                return self.complete_handoff_locked(&mut s, &sid, keepalive);
            }
            if !s.ws.as_ref().map(|k| k.id == ws_id).unwrap_or(false) {
                return;
            }
            s.session_id = Some(sid);
            s.keepalive_sec = if keepalive.is_nan() || keepalive == 0.0 { 10 } else { keepalive as u64 };
            s.reconnect_delay = RECONNECT_BASE_MS;
            self.reset_watchdog_locked(&mut s);
            s.attached.values().cloned().collect()
        };
        // Twitch drops a session that has not subscribed within 10s of the welcome, so go in parallel.
        let futs = sources.into_iter().map(|src| {
            let me = self.clone();
            async move { me.subscribe_source(src).await }
        });
        tokio::join!(futures_util::future::join_all(futs), self.subscribe_whispers());
    }

    async fn subscribe_source(self: &Arc<Self>, source: Arc<TwitchSource>) {
        let Some(id) = source.channel_id() else { return };
        let session_id = self.st.lock().unwrap().session_id.clone();
        let rec = auth::current();
        let (Some(session_id), Some(rec)) = (session_id, rec) else { return };
        if !self.is_attached(&id, &source) {
            return;
        }
        let acc = get_access(&id).await;
        if !(acc.is_broadcaster || acc.is_mod == Some(true)) || !has_required_scopes(&rec.scopes) {
            return;
        }
        struct Wanted {
            ty: &'static str,
            version: &'static str,
            cond: Value,
            core: bool,
        }
        let mut wanted: Vec<Wanted> = {
            let mut s = self.st.lock().unwrap();
            let have = s.subs.entry(id.clone()).or_default();
            MOD_SUBS
                .iter()
                .filter(|(t, _)| !have.contains_key(*t))
                .map(|(t, v)| Wanted { ty: t, version: v, cond: json!({"broadcaster_user_id": id, "moderator_user_id": rec.user.id}), core: true })
                .collect()
        };
        if acc.is_broadcaster {
            let have = |t: &str| self.st.lock().unwrap().subs.get(&id).map(|m| m.contains_key(t)).unwrap_or(false);
            if (rec.scopes.iter().any(|s| s == "channel:read:redemptions") || rec.scopes.iter().any(|s| s == "channel:manage:redemptions")) && !have(REDEEM_SUB.0) {
                wanted.push(Wanted { ty: REDEEM_SUB.0, version: REDEEM_SUB.1, cond: json!({"broadcaster_user_id": id}), core: false });
            }
            if rec.scopes.iter().any(|s| s == "bits:read") && !have(POWER_UP_SUB.0) {
                wanted.push(Wanted { ty: POWER_UP_SUB.0, version: POWER_UP_SUB.1, cond: json!({"broadcaster_user_id": id}), core: false });
            }
        }
        {
            let s = self.st.lock().unwrap();
            let total: usize = s.subs.values().map(|m| m.len()).sum();
            if total + wanted.len() > MAX_SUBSCRIPTIONS {
                drop(s);
                log(&format!("eventsub: subscription cap reached, not subscribing {}", source.login()));
                return source.set_esub(false, Some("error"), None);
            }
        }
        let results = futures_util::future::join_all(wanted.iter().map(|s| {
            let body = json!({"type": s.ty, "version": s.version, "condition": s.cond, "transport": {"method": "websocket", "session_id": session_id}});
            helix("/eventsub/subscriptions", "POST", Vec::new(), Some(body))
        }))
        .await;
        if self.st.lock().unwrap().session_id.as_deref() != Some(session_id.as_str()) || !self.is_attached(&id, &source) {
            return;
        }
        let mut denied = false;
        let mut limited = false;
        let mut core_failed = false;
        {
            let mut s = self.st.lock().unwrap();
            let have = s.subs.entry(id.clone()).or_default();
            for (w, r) in wanted.iter().zip(results) {
                match r {
                    Ok(v) => {
                        have.insert(w.ty.to_string(), v["data"][0]["id"].as_str().unwrap_or("").to_string());
                    }
                    Err(err) => {
                        if err.status == 409 {
                            have.insert(w.ty.to_string(), String::new()); // already subscribed on this session
                            continue;
                        }
                        log(&format!("eventsub: {} for {} failed ({}) {}", w.ty, source.login(), err.status, err.message));
                        if !w.core {
                            continue; // redeems are a bonus
                        }
                        core_failed = true;
                        if err.status == 401 || err.status == 403 {
                            denied = true;
                        }
                        if err.status == 429 || (err.status == 400 && err.message.to_lowercase().contains("limit")) {
                            limited = true;
                        }
                    }
                }
            }
        }
        let all_core = {
            let s = self.st.lock().unwrap();
            MOD_SUBS.iter().all(|(t, _)| s.subs.get(&id).map(|m| m.contains_key(*t)).unwrap_or(false))
        };
        if !core_failed && all_core {
            source.set_esub(true, None, Some(true));
            if acc.is_broadcaster {
                source.refresh_rewards();
            }
        } else if denied && !limited {
            source.set_esub(false, Some("relogin"), None);
            invalidate_access(); // maybe the mod grant is gone
        } else {
            source.set_esub(false, Some("error"), None);
            self.retry_later(&source);
        }
    }

    fn on_notification(&self, msg: &Value) {
        if let Some(id) = msg["metadata"]["message_id"].as_str().filter(|s| !s.is_empty()) {
            let mut s = self.st.lock().unwrap();
            if s.seen.contains(id) {
                return; // Twitch may redeliver
            }
            s.seen.insert(id.to_string());
            s.seen_order.push_back(id.to_string());
            if s.seen_order.len() > DEDUPE_CAP {
                if let Some(old) = s.seen_order.pop_front() {
                    s.seen.remove(&old);
                }
            }
        }
        let sub = &msg["payload"]["subscription"];
        let event = &msg["payload"]["event"];
        if sub.is_null() || event.is_null() {
            return;
        }
        if sub["type"] == WHISPER_SUB.0 {
            return whisper::on_event(event);
        }
        let bid = crate::util::js_str_or_empty(sub["condition"].get("broadcaster_user_id"));
        let source = self.st.lock().unwrap().attached.get(&bid).cloned();
        if let (Some(source), Some(ty)) = (source, sub["type"].as_str()) {
            dispatch(&source, ty, event);
        }
    }

    fn on_session_reconnect(self: &Arc<Self>, msg: &Value) {
        let Some(url) = msg["payload"]["session"]["reconnect_url"].as_str().filter(|u| !u.is_empty()).map(String::from) else { return };
        let mut s = self.st.lock().unwrap();
        if s.reconnect_socket.is_some() {
            return;
        }
        let sock = self.make_sock(&mut s, url, true);
        s.reconnect_socket = Some(sock);
    }

    // Adopt the handoff socket once ITS welcome arrives, then close the old one. Subscriptions carry over on
    // Twitch's side: do NOT subscribe again.
    fn complete_handoff_locked(self: &Arc<Self>, s: &mut State, session_id: &str, keepalive: f64) {
        let old = s.ws.take();
        s.session_id = Some(session_id.to_string());
        if !keepalive.is_nan() && keepalive != 0.0 {
            s.keepalive_sec = keepalive as u64;
        }
        let mut new = s.reconnect_socket.take().unwrap();
        new.handoff = false;
        s.ws = Some(new);
        self.reset_watchdog_locked(s);
        if let Some(old) = old {
            old.handle.close();
        }
    }

    fn on_revocation(&self, msg: &Value) {
        let sub = &msg["payload"]["subscription"];
        let Some(ty) = sub["type"].as_str().filter(|t| !t.is_empty()) else { return };
        let id = if ty == WHISPER_SUB.0 { WHISPER_KEY.to_string() } else { crate::util::js_str_or_empty(sub["condition"].get("broadcaster_user_id")) };
        let status = sub["status"].as_str().unwrap_or("").to_string();
        let source = {
            let mut s = self.st.lock().unwrap();
            if let Some(m) = s.subs.get_mut(&id) {
                m.remove(ty);
            }
            s.attached.get(&id).cloned()
        };
        if status == "authorization_revoked" {
            tokio::spawn(auth::validate()); // the token itself may be dead: validate refreshes or logs out
        }
        let Some(source) = source else { return };
        if !MOD_SUBS.iter().any(|(t, _)| *t == ty) {
            return;
        }
        if status == "moderator_removed" {
            invalidate_access();
        }
        source.set_esub(false, if status == "moderator_removed" { None } else { Some("relogin") }, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_scopes() {
        let mut have: Vec<String> = REQUIRED_SCOPES.iter().map(|s| s.to_string()).collect();
        assert!(!has_required_scopes(&have));
        for p in ONE_OF_SCOPE_PAIRS {
            have.push(p[1].to_string());
        }
        assert!(has_required_scopes(&have));
        have.retain(|s| s != "moderator:manage:warnings");
        assert!(!has_required_scopes(&have));
    }
}
