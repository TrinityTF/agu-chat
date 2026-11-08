//! Port of src/main/x/index.js: the X (Twitter) platform module. See docs/ARCHITECTURE.md.
//!
//! Reads x.com's live-chat stream (newline-delimited JSON, backfill first) with a GUEST token and, for names and
//! sending, the user's own x.com session (auth.rs). Nothing goes through the Agu Chat website and nothing is
//! paid: no X API v2, only the web endpoints x.com's own client calls. Emits the frames /ws/x sends: chatlog
//! {status, messages}, chat {messages}, status {status}, plus the uniform conn frame.
//!
//! WHAT IS AND IS NOT POSSIBLE AS A GUEST (measured; see vendor/x_live.rs):
//!  * Given a broadcast id, state, title, chat history and live chat all work anonymously.
//!  * Getting the id from a handle is auth-gated at X's end. The public profile only carries a broadcast link
//!    if the streamer tweeted it, so a handle source finds the stream only then; pasting the broadcast link
//!    (x.com/i/broadcasts/<id>) always works.
//!  * Names: events carry only a user id and the lookup needs a login. So X chat is shown only while logged in:
//!    events wait (250 ms batches) while their authors' names are looked up (names.rs), and a message whose
//!    author's username X does not give is dropped, never shown under a made-up name. Logged out, no stream is
//!    read and the column says to log in (the broadcast itself is still found, so sending works as soon as the
//!    login is there). A refused session (401/403) is an expired login (auth.rs).
//!  * Sending: `POST /api/x/send`, x.com's own `sendLiveChatMutation` with the session, to the broadcast this
//!    source reads. X's error text is passed on as is.
//!
//! Differences from the JS: timers are `util::Timer`s, the stream loop is a task owned by the current chat
//! (its AbortHandle is the chat's AbortController), `stop()` aborts the source's tasks (`net::Jobs`).
pub mod auth;
pub mod names;
pub mod ua;
pub mod vendor;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;

use serde_json::{json, Map, Value};
use tokio::task::AbortHandle;

use crate::net::Jobs;
use crate::platforms::{Accounts, Ctx, Emit, PlatformModule, Source};
use crate::protocol::{AccountState, Frame, Platform};
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};
use crate::util::{js_str_or_empty, js_string, js_trim, now_ms, truthy, Timer};
use ua::set_user_agent;
use names::Known;
use vendor::x_chat::{read_live_chat, to_x_chat_msg};
use vendor::x_live::{broadcast_id_in_url, broadcast_states, find_live, normalize_handle, FindLive};

const MAX_BACKLOG: usize = 300;
// x.com's own reconnect delays: a second after a clean end, thirty after an error answer. 503 is X shedding load.
const STREAM_RETRY_MS: u64 = 1000;
const STREAM_ERROR_RETRY_MS: u64 = 30_000;
// A stream that ends this soon without a single event is not a clean end, whatever its status said.
const STREAM_MIN_HEALTHY_MS: i64 = 3000;
const STREAM_FAILS_BEFORE_ERROR: u32 = 3;
// How often a live broadcast is asked whether it is still RUNNING.
const LIVE_CHECK_MS: u64 = 30_000;
const OFFLINE_POLL_MS: u64 = 60_000;
const MAX_RECHECK_MS: u64 = 5 * 60_000;
// Events are held this long so the ~100-message backfill arrives as one chatlog rather than 100 frames.
const BATCH_MS: u64 = 250;

// A name lookup that fails (network, 5xx) is retried this many times, one second, two, four apart, and then the batch is dropped.
const LOOKUP_RETRIES: u32 = 3;
// Events waiting for their names never pile up past this (the oldest go first).
const MAX_PENDING: usize = 2000;
const NOT_LIVE_REASON: &str = "Not live";

/// normalized channel -> the broadcast id its source is reading right now (what sending needs). Like Kick's USER_IDS.
static BROADCASTS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn set_broadcast(norm: &str, id: Option<&str>) {
    let mut m = BROADCASTS.lock().unwrap();
    match id {
        Some(id) => m.insert(norm.to_string(), id.to_string()),
        None => m.remove(norm),
    };
}

/// "broadcast:<id>" for a broadcast link/id, otherwise the lowercased X username.
pub fn normalize_channel(input: &str) -> Result<String, String> {
    let raw = js_trim(input);
    if raw.is_empty() {
        return Err("Enter an X username or a broadcast link.".into());
    }
    if let Some(id) = raw.strip_prefix("broadcast:").filter(|id| (13..=14).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())) {
        return Ok(format!("broadcast:{id}"));
    }
    // Only a URL counts as a broadcast: a bare 13-character word is far likelier a username.
    if let Some(id) = broadcast_id_in_url(raw) {
        return Ok(format!("broadcast:{id}"));
    }
    let handle = normalize_handle(raw);
    if handle.is_empty() {
        return Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into());
    }
    Ok(handle.to_lowercase())
}

pub struct X;

impl PlatformModule for X {
    fn platform(&self) -> Platform {
        Platform::X
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        normalize_channel(input)
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        set_user_agent(&ctx.user_agent);
        auth::ensure_ctx(&ctx); // sources can be created before init() in tests
        // JS normalizes again here; the relay hands over an already normalized channel.
        let norm = normalize_channel(&channel).unwrap_or(channel);
        XSource::new(norm, emit, ctx)
    }

    fn routes(&self) -> Vec<(String, Handler)> {
        vec![("POST /api/x/send".into(), Arc::new(|req| Box::pin(route_send(req))))]
    }

    fn accounts(&self) -> Option<Arc<dyn Accounts>> {
        Some(Arc::new(XAccounts))
    }

    fn init(&self, ctx: Arc<Ctx>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            auth::init(ctx);
            Ok(())
        })
    }
}

struct XAccounts;

impl Accounts for XAccounts {
    fn state(&self) -> AccountState {
        auth::state()
    }
    fn login(&self, _args: Value) -> BoxFuture<'static, Result<Value, String>> {
        Box::pin(auth::login())
    }
    fn cancel(&self) {
        auth::cancel();
    }
    fn logout(&self) -> BoxFuture<'static, ()> {
        Box::pin(auth::logout())
    }
}

fn route_err(status: u16, msg: &str) -> RouteRes {
    RouteRes { status, body: json!({"error": msg}) }
}

/// POST /api/x/send {text, replyTo?}: the message goes to the broadcast this column is reading. Length is X's call,
/// its error comes back as the toast.
async fn route_send(req: RouteReq) -> Result<RouteRes, RouteError> {
    let text = js_str_or_empty(Some(&req.body["text"])).trim().to_string();
    if text.is_empty() {
        return Ok(route_err(400, "empty message"));
    }
    if !auth::is_logged_in() {
        return Ok(route_err(401, if auth::is_expired() { auth::EXPIRED_MESSAGE } else { auth::LOGGED_OUT_REASON }));
    }
    let norm = normalize_channel(&req.source.channel).unwrap_or_else(|_| req.source.channel.clone());
    let Some(broadcast) = BROADCASTS.lock().unwrap().get(&norm).cloned() else { return Ok(route_err(409, NOT_LIVE_REASON)) };
    let reply = js_str_or_empty(Some(&req.body["replyTo"]));
    match auth::send_chat(&broadcast, &text, Some(reply.as_str()).filter(|r| !r.is_empty())).await {
        Ok(()) => Ok(RouteRes { status: 200, body: json!({"ok": true}) }),
        Err(e) => Ok(route_err(e.status, &e.message)),
    }
}

struct Chat {
    id: u64,
    broadcast_id: String,
    cursor: String,
    fails: u32,
    hello: bool,
    task: Option<AbortHandle>,
}

struct St {
    status: Map<String, Value>,
    backlog: Vec<Value>,
    seen: HashSet<String>,
    chat: Option<Chat>,
    next_chat: u64,
    pending: Vec<Value>,
    pending_timer: Timer,
    pending_armed: bool,
    /// the chat whose events a flush task is looking names up for right now
    flushing: Option<u64>,
    /// an auth change asked for a fresh resolve while a tick was running
    rerun: bool,
    auth_listener: Option<u64>,
    poll_timer: Timer,
    polling: bool,
    soft_misses: u32,
    resolved_once: bool,
    stopped: bool,
    last_conn_json: String,
}

pub struct XSource {
    norm: String,
    pinned: String,
    handle: String,
    emit_fn: Emit,
    ctx: Arc<Ctx>,
    jobs: Jobs,
    st: Mutex<St>,
}

fn conn_frame(st: &St, pinned: bool) -> Value {
    let s = &st.status;
    let error = js_str_or_empty(s.get("error"));
    let state = js_str_or_empty(s.get("state"));
    let live = truthy(&s["live"]);
    let logged_in = auth::is_logged_in();
    let (st_name, detail) = if !logged_in {
        // No login, no chat: names (and so every message) need the session.
        ("offline", auth::logged_out_detail().to_string())
    } else if !st.resolved_once {
        ("connecting", "Looking for a live X broadcast...".to_string())
    } else if !error.is_empty() {
        ("error", error)
    } else if live {
        ("open", "Live".to_string())
    } else {
        (
            "offline",
            if pinned {
                if !state.is_empty() && state != "RUNNING" { format!("This broadcast is not running ({}).", state.to_lowercase()) } else { "This broadcast is not running.".to_string() }
            } else {
                "Not live. X only reveals a broadcast from a handle when the streamer tweets the link: paste the broadcast link to follow it directly.".to_string()
            },
        )
    };
    let mut f = json!({"type": "conn", "state": st_name, "detail": detail, "live": live});
    let title = js_str_or_empty(s.get("title"));
    if !title.is_empty() {
        f["title"] = json!(title);
    }
    let can_send = logged_in && live;
    f["canSend"] = json!(can_send);
    if !can_send {
        f["readOnlyReason"] = json!(if logged_in { NOT_LIVE_REASON } else { auth::LOGGED_OUT_REASON });
    }
    f
}

fn chatlog_of(st: &St) -> Value {
    json!({"type": "chatlog", "status": Value::Object(st.status.clone()), "messages": st.backlog})
}

impl XSource {
    fn new(norm: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        let pinned = norm.strip_prefix("broadcast:").unwrap_or("").to_string();
        let handle = if pinned.is_empty() { norm.clone() } else { String::new() };
        let mut status = Map::new();
        status.insert("live".into(), json!(false));
        status.insert("broadcastId".into(), Value::Null);
        status.insert("handle".into(), json!(handle));
        status.insert("title".into(), json!(""));
        status.insert("url".into(), json!(""));
        status.insert("state".into(), json!(""));
        status.insert("error".into(), Value::Null);
        status.insert("notice".into(), Value::Null);
        Arc::new(XSource {
            norm,
            pinned,
            handle,
            emit_fn: emit,
            ctx,
            jobs: Jobs::new(),
            st: Mutex::new(St {
                status,
                backlog: Vec::new(),
                seen: HashSet::new(),
                chat: None,
                next_chat: 1,
                pending: Vec::new(),
                pending_timer: Timer::new(),
                pending_armed: false,
                flushing: None,
                rerun: false,
                auth_listener: None,
                poll_timer: Timer::new(),
                polling: false,
                soft_misses: 0,
                resolved_once: false,
                stopped: false,
                last_conn_json: String::new(),
            }),
        })
    }

    fn emit(&self, f: Frame) {
        (self.emit_fn)(f);
    }

    fn stopped(&self) -> bool {
        self.st.lock().unwrap().stopped
    }

    fn is_current(&self, id: u64) -> bool {
        self.st.lock().unwrap().chat.as_ref().map(|c| c.id) == Some(id)
    }

    fn recheck_delay(&self) -> u64 {
        let sm = self.st.lock().unwrap().soft_misses.min(3);
        (OFFLINE_POLL_MS * 2u64.pow(sm)).min(MAX_RECHECK_MS)
    }

    fn publish_conn(&self) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st, !self.pinned.is_empty());
            let j = f.to_string();
            if j == st.last_conn_json {
                return;
            }
            st.last_conn_json = j;
            f
        };
        self.emit(f);
    }

    fn set_status(&self, next: Vec<(&str, Value)>) {
        let changed = {
            let mut st = self.st.lock().unwrap();
            let mut changed = false;
            for (k, v) in next {
                if st.status.get(k) != Some(&v) {
                    changed = true;
                    st.status.insert(k.to_string(), v);
                }
            }
            let live_id = if truthy(&st.status["live"]) { st.status.get("broadcastId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(String::from) } else { None };
            set_broadcast(&self.norm, live_id.as_deref());
            changed.then(|| Value::Object(st.status.clone()))
        };
        if let Some(status) = changed {
            self.emit(json!({"type": "status", "status": status}));
        }
        self.publish_conn();
    }

    // ---------- resolution ----------

    /// Which broadcast should this source be reading? A pinned id is the only reliable answer.
    /// Returns (found, state).
    async fn resolve(&self) -> Result<(FindLive, String), String> {
        if !self.pinned.is_empty() {
            let states = broadcast_states(&[self.pinned.clone()]).await?;
            let s = states.into_iter().find(|(id, _)| *id == self.pinned).map(|(_, s)| s);
            return Ok(match s {
                Some(s) if s.state == "RUNNING" => (FindLive { live: true, broadcast_id: self.pinned.clone(), title: s.title }, String::new()),
                other => (FindLive::default(), other.map(|s| s.state).unwrap_or_default()),
            });
        }
        // Free when it works, which is only when the streamer tweets the link.
        let found = find_live(&self.handle).await?;
        Ok(if found.live { (found, String::new()) } else { (FindLive::default(), String::new()) })
    }

    async fn tick_resolve(self: &Arc<Self>) -> u64 {
        let found = match self.resolve().await {
            Ok(f) => f,
            Err(e) => {
                if self.stopped() {
                    return 0;
                }
                {
                    let mut st = self.st.lock().unwrap();
                    st.soft_misses += 1;
                    st.resolved_once = true;
                }
                self.set_status(vec![("live", json!(false)), ("error", json!(if e.is_empty() { "lookup failed".to_string() } else { e })), ("notice", Value::Null)]);
                return self.recheck_delay();
            }
        };
        if self.stopped() {
            return 0;
        }
        self.st.lock().unwrap().resolved_once = true;
        let (found, state) = found;

        if !found.live || found.broadcast_id.is_empty() {
            // NOT a soft miss: "they are not streaming" is the ordinary answer, and backing off on it would delay
            // noticing a stream that starts. Title and id are cleared so yesterday's doesn't read as running.
            self.st.lock().unwrap().soft_misses = 0;
            self.set_status(vec![
                ("live", json!(false)),
                ("broadcastId", Value::Null),
                ("title", json!("")),
                ("state", json!(state)),
                ("url", json!("")),
                ("error", Value::Null),
                ("notice", Value::Null),
            ]);
            return OFFLINE_POLL_MS;
        }

        let logged_in = auth::is_logged_in();
        let chat_id = {
            let mut st = self.st.lock().unwrap();
            st.soft_misses = 0;
            let cur = js_str_or_empty(st.status.get("broadcastId"));
            if !cur.is_empty() && cur != found.broadcast_id {
                st.backlog.clear();
                st.seen.clear();
            }
            if !logged_in {
                // Without a login there are no names and so no messages: the broadcast is only noted (sending
                // needs it once the login is there) and no stream is read.
                None
            } else {
            let id = st.next_chat;
            st.next_chat += 1;
            st.chat = Some(Chat { id, broadcast_id: found.broadcast_id.clone(), cursor: String::new(), fails: 0, hello: false, task: None });
            Some(id)
            }
        };
        self.set_status(vec![
            ("live", json!(true)),
            ("broadcastId", json!(found.broadcast_id)),
            ("title", json!(found.title)),
            ("state", json!("RUNNING")),
            ("url", json!(format!("https://x.com/i/broadcasts/{}", found.broadcast_id))),
            ("error", Value::Null),
            ("notice", Value::Null),
        ]);
        match chat_id {
            Some(id) => self.start_stream(id, found.broadcast_id),
            // looked at again soon, so a login that happened meanwhile starts the chat
            None => return LIVE_CHECK_MS,
        }
        LIVE_CHECK_MS
    }

    /// Reading a live broadcast: is it still running? Only an explicit answer ends it.
    async fn tick_live(self: &Arc<Self>, chat_id: u64, broadcast_id: String) -> u64 {
        let mut s = None;
        match broadcast_states(&[broadcast_id.clone()]).await {
            Ok(states) => s = states.into_iter().find(|(id, _)| *id == broadcast_id).map(|(_, s)| s),
            Err(e) => self.ctx.log(&format!("[x:{}] state check failed: {e}", self.norm)),
        }
        if self.stopped() || !self.is_current(chat_id) {
            return 0;
        }
        if let Some(s) = s.filter(|s| !s.state.is_empty() && s.state != "RUNNING") {
            self.drop_chat();
            self.set_status(vec![
                ("live", json!(false)),
                ("broadcastId", Value::Null),
                ("title", json!("")),
                ("state", json!(s.state)),
                ("url", json!("")),
                ("error", Value::Null),
                ("notice", Value::Null),
            ]);
            // A handle source goes straight back to resolving, which picks up a follow-on stream.
            return if self.pinned.is_empty() { 0 } else { OFFLINE_POLL_MS };
        }
        LIVE_CHECK_MS
    }

    async fn tick(self: Arc<Self>) {
        let cur = {
            let mut st = self.st.lock().unwrap();
            if st.stopped || st.polling {
                return;
            }
            st.polling = true;
            st.chat.as_ref().map(|c| (c.id, c.broadcast_id.clone()))
        };
        let next = match cur {
            Some((id, b)) => self.tick_live(id, b).await,
            None => self.tick_resolve().await,
        };
        let (stopped, rerun) = {
            let mut st = self.st.lock().unwrap();
            st.polling = false;
            (st.stopped, std::mem::take(&mut st.rerun))
        };
        if !stopped {
            self.schedule(if rerun { 0 } else { next });
        }
    }

    /// A sync fn on purpose: building the timer future inside `tick` would make its type recursive.
    fn schedule(self: &Arc<Self>, ms: u64) {
        let s = self.clone();
        self.st.lock().unwrap().poll_timer.set(Duration::from_millis(ms), async move {
            let s2 = s.clone();
            s.jobs.spawn(s2.tick());
        });
    }

    // ---------- the chat stream ----------

    fn start_stream(self: &Arc<Self>, chat_id: u64, broadcast_id: String) {
        let s = self.clone();
        let h = tokio::spawn(async move { s.stream_loop(chat_id, broadcast_id).await });
        let mut st = self.st.lock().unwrap();
        let stopped = st.stopped;
        match st.chat.as_mut().filter(|c| c.id == chat_id) {
            Some(c) if !stopped => c.task = Some(h.abort_handle()),
            _ => h.abort(),
        }
    }

    /// Keep X's live-chat stream open while this chat is current, reconnecting the way x.com does.
    async fn stream_loop(self: Arc<Self>, chat_id: u64, broadcast_id: String) {
        while self.is_current(chat_id) {
            let opened_at = now_ms();
            let cursor = self.st.lock().unwrap().chat.as_ref().map(|c| c.cursor.clone()).unwrap_or_default();
            let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut delay = STREAM_RETRY_MS;
            let (s2, ev2) = (self.clone(), events.clone());
            let r = read_live_chat(&broadcast_id, &cursor, move |ev| {
                ev2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                s2.on_live_event(chat_id, ev);
            })
            .await;
            let r = r.and_then(|_| {
                if events.load(std::sync::atomic::Ordering::SeqCst) == 0 && now_ms() - opened_at < STREAM_MIN_HEALTHY_MS {
                    Err(vendor::x_chat::XError { message: "live-chat ended at once".into(), status: None })
                } else {
                    Ok(())
                }
            });
            match r {
                Ok(()) => self.stream_recovered(chat_id),
                Err(e) => {
                    if !self.is_current(chat_id) {
                        break;
                    }
                    let fails = {
                        let mut st = self.st.lock().unwrap();
                        match st.chat.as_mut() {
                            Some(c) => {
                                c.fails += 1;
                                c.fails
                            }
                            None => break,
                        }
                    };
                    delay = if e.status == Some(503) { STREAM_RETRY_MS } else { STREAM_ERROR_RETRY_MS.min(STREAM_RETRY_MS * 2u64.pow(fails.min(20))) };
                    self.ctx.log(&format!("[x:{}] live-chat failed ({fails}): {}", self.norm, e.message));
                    if fails >= STREAM_FAILS_BEFORE_ERROR {
                        self.set_status(vec![("error", json!(format!("X chat unavailable: {}", e.message)))]);
                    }
                }
            }
            if !self.is_current(chat_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    fn stream_recovered(&self, chat_id: u64) {
        let clear = {
            let mut st = self.st.lock().unwrap();
            let Some(c) = st.chat.as_mut().filter(|c| c.id == chat_id) else { return };
            if c.fails == 0 {
                return;
            }
            c.fails = 0;
            js_str_or_empty(st.status.get("error")).starts_with("X chat unavailable")
        };
        if clear {
            self.set_status(vec![("error", Value::Null)]);
        }
    }

    fn on_live_event(self: &Arc<Self>, chat_id: u64, ev: Value) {
        if !ev.is_object() {
            return;
        }
        {
            let mut st = self.st.lock().unwrap();
            let Some(c) = st.chat.as_mut().filter(|c| c.id == chat_id) else { return };
            // The cursor is the last `ts` seen, whatever kind of event carried it -- that is what x.com reconnects from.
            if truthy(&ev["ts"]) {
                let ts = js_string(&ev["ts"]);
                if c.cursor.is_empty() || ts > c.cursor {
                    c.cursor = ts;
                }
            }
        }
        self.stream_recovered(chat_id);
        if ev["chatType"] != 1 || !truthy(&ev["message"]) || !truthy(&ev["userId"]) {
            return;
        }
        let mut st = self.st.lock().unwrap();
        st.pending.push(ev);
        if st.pending.len() > MAX_PENDING {
            let cut = st.pending.len() - MAX_PENDING;
            st.pending.drain(..cut);
        }
        // A running flush takes what arrives meanwhile when it is done with its batch.
        if !st.pending_armed && st.flushing != Some(chat_id) {
            st.pending_armed = true;
            let s = self.clone();
            st.pending_timer.set(Duration::from_millis(BATCH_MS), async move {
                {
                    let mut st = s.st.lock().unwrap();
                    st.pending_armed = false;
                    if st.flushing == Some(chat_id) {
                        return;
                    }
                    st.flushing = Some(chat_id);
                }
                let s2 = s.clone();
                s.jobs.spawn(s2.flush_task(chat_id));
            });
        }
    }

    /// Hand the waiting events on, batch after batch, until none are left. Each batch waits for its authors' names.
    async fn flush_task(self: Arc<Self>, chat_id: u64) {
        loop {
            let batch = {
                let mut st = self.st.lock().unwrap();
                let current = st.chat.as_ref().map(|c| c.id) == Some(chat_id);
                if !current || st.pending.is_empty() || st.stopped {
                    if st.flushing == Some(chat_id) {
                        st.flushing = None;
                    }
                    return;
                }
                std::mem::take(&mut st.pending)
            };
            let msgs = self.name_batch(chat_id, &batch).await;
            self.deliver(chat_id, msgs);
        }
    }

    /// The batch's messages, each with its author's name; those whose author has no username are left out.
    /// Unknown authors are looked up (retrying a failed call with backoff, then the batch is dropped).
    async fn name_batch(&self, chat_id: u64, batch: &[Value]) -> Vec<Value> {
        let ids = names::unknown_ids(batch.iter().map(|ev| js_string(&ev["userId"])));
        if !ids.is_empty() {
            let mut attempt = 0;
            loop {
                match names::fetch_names(&ids).await {
                    Ok(()) => break,
                    Err(e) if e.is_auth() => {
                        self.ctx.log(&format!("[x:{}] name lookup refused ({}), login expired", self.norm, e.status));
                        return Vec::new();
                    }
                    Err(e) => {
                        attempt += 1;
                        self.ctx.log(&format!("[x:{}] name lookup failed ({attempt}): {}", self.norm, e.message));
                        if attempt > LOOKUP_RETRIES || !self.is_current(chat_id) {
                            return Vec::new();
                        }
                        tokio::time::sleep(Duration::from_millis(1000 * (1 << (attempt - 1)))).await;
                    }
                }
            }
        }
        if !auth::is_logged_in() {
            return Vec::new();
        }
        batch
            .iter()
            .filter_map(|ev| match names::cached(&js_string(&ev["userId"])) {
                Known::User(u) => to_x_chat_msg(ev, &u),
                _ => None,
            })
            .collect()
    }

    /// Add a named batch to the backlog and emit it: the first batch is the backfill and goes out as one chatlog.
    fn deliver(&self, chat_id: u64, msgs: Vec<Value>) {
        let frame = {
            let mut st = self.st.lock().unwrap();
            if st.chat.as_ref().map(|c| c.id) != Some(chat_id) {
                return;
            }
            let mut fresh: Vec<Value> = Vec::new();
            for msg in msgs {
                if !st.seen.insert(js_string(&msg["id"])) {
                    continue;
                }
                fresh.push(msg);
            }
            fresh.sort_by_key(|m| m["ts"].as_i64().unwrap_or(0));
            st.backlog.extend(fresh.iter().cloned());
            if st.backlog.len() > MAX_BACKLOG {
                let cut = st.backlog.len() - MAX_BACKLOG;
                st.backlog.drain(..cut);
            }
            if st.seen.len() > MAX_BACKLOG * 4 {
                st.seen = st.backlog.iter().map(|m| js_string(&m["id"])).collect();
            }
            let hello = st.chat.as_ref().map(|c| c.hello).unwrap_or(true);
            if !hello {
                // The first batch is the backfill: one chatlog.
                st.chat.as_mut().unwrap().hello = true;
                Some(chatlog_of(&st))
            } else if !fresh.is_empty() {
                Some(json!({"type": "chat", "messages": fresh}))
            } else {
                None
            }
        };
        if let Some(f) = frame {
            self.emit(f);
        }
    }

    /// The login changed (in, out, expired): start over, so a login brings the backfill in as a chatlog and a
    /// logout empties the column.
    fn on_auth_change(self: &Arc<Self>) {
        if self.stopped() {
            return;
        }
        self.drop_chat();
        let f = {
            let mut st = self.st.lock().unwrap();
            st.backlog.clear();
            st.seen.clear();
            st.soft_misses = 0;
            st.rerun = true;
            chatlog_of(&st)
        };
        self.emit(f);
        self.publish_conn();
        // the running tick picks `rerun` up when it ends; otherwise resolve now
        if !self.st.lock().unwrap().polling {
            self.schedule(0);
        }
    }

    fn drop_chat(&self) {
        let task = {
            let mut st = self.st.lock().unwrap();
            let t = st.chat.take().and_then(|c| c.task);
            st.pending.clear();
            st.pending_timer.clear();
            st.pending_armed = false;
            t
        };
        if let Some(t) = task {
            t.abort();
        }
    }
}

impl Source for XSource {
    fn start(self: Arc<Self>) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st, !self.pinned.is_empty());
            st.last_conn_json = f.to_string();
            f
        };
        self.emit(f);
        // conn (canSend, the login ask) and the chat itself follow the login
        let me: Weak<XSource> = Arc::downgrade(&self);
        let id = auth::EVENTS.on(Arc::new(move |_| {
            if let Some(s) = me.upgrade() {
                s.on_auth_change();
            }
        }));
        self.st.lock().unwrap().auth_listener = Some(id);
        let s = self.clone();
        self.jobs.spawn(s.tick());
    }

    fn stop(&self) {
        {
            let mut st = self.st.lock().unwrap();
            st.stopped = true;
            if let Some(id) = st.auth_listener.take() {
                auth::EVENTS.off(id);
            }
        }
        set_broadcast(&self.norm, None);
        self.drop_chat();
        self.st.lock().unwrap().poll_timer.clear();
        self.jobs.abort_all();
    }

    fn snapshot(&self) -> Vec<Frame> {
        let st = self.st.lock().unwrap();
        vec![chatlog_of(&st), conn_frame(&st, !self.pinned.is_empty())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_channel_forms() {
        let n = |s: &str| normalize_channel(s);
        let id = "1DGleVdEapOJL";
        let id14 = "2qVWZNBJRbVaJN";
        assert_eq!(n("EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("@EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("https://x.com/EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("twitter.com/EsfandTV/"), Ok("esfandtv".into()));
        assert_eq!(n(&format!("https://x.com/i/broadcasts/{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("x.com/i/broadcasts/{id14}?s=20")), Ok(format!("broadcast:{id14}")));
        assert_eq!(n(&format!("https://twitter.com/broadcasts/{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("broadcast:{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("broadcast:{id14}")), Ok(format!("broadcast:{id14}")));
        // a bare 13-character word is a username, not a broadcast
        assert_eq!(n(id), Ok(id.to_lowercase()));
        // a 15-character id is not a broadcast; the JS then reads the URL as the profile "i" (x.com/i), and so does this
        assert_eq!(n(&format!("https://x.com/i/broadcasts/{id}XX")), Ok("i".into()));
        assert_eq!(n("broadcast:short"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        assert_eq!(n(""), Err("Enter an X username or a broadcast link.".into()));
        assert_eq!(n("   "), Err("Enter an X username or a broadcast link.".into()));
        assert_eq!(n("has space"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        assert_eq!(n("this_name_is_way_too_long"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        // stable
        assert_eq!(n("esfandtv"), Ok("esfandtv".into()));
        assert_eq!(n(&format!("broadcast:{id}")), Ok(format!("broadcast:{id}")));
    }
}
