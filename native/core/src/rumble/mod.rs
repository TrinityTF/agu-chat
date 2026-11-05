//! Port of src/main/rumble/index.js: the Rumble platform module. See docs/ARCHITECTURE.md ("Platform module").
//!
//! Reading: Rumble has no chat WebSocket. Its own player reads Server-Sent Events from
//!   GET https://<front-end host>.rumble.com/chat/api/chat/<chatId>/stream
//! which is unauthenticated. Chat is per VIDEO, so a source owns re-resolving which video its channel is live
//! on and swapping the SSE connection underneath. An offline channel keeps polling and reattaches by itself.
//!
//! HARD RULES (do not "fix" these): only front-end hosts (wn0, then web7 for chat), never the rumble.com
//! apex; never try to get past a Cloudflare challenge (no TLS impersonation, no scripted browsers, no
//! browser UA).
//!
//! Emits the frames /ws/rumble sends: chatlog, chat {msg}, status, delete, mute, pin, emotes, badges; plus
//! the uniform conn frame. Sending and moderating go through the user's OWN Rumble login (see
//! vendor/rumble_auth.rs); only the u_s session is kept, in ctx.secrets under "rumble".
//!
//! Differences from the JS: the SSE reader is a task whose AbortHandle plays the AbortController (a
//! generation counter tells a stale reader that it was replaced); timers are `util::Timer`s; stop() aborts the
//! source's tasks (`net::Jobs`).
pub mod ua;
pub mod vendor;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::StreamExt;
use serde_json::{json, Map, Value};
use tokio::task::AbortHandle;

use crate::net::{FetchOpts, Jobs};
use crate::platforms::{Accounts, Ctx, Emit, PlatformModule, Source};
use crate::protocol::{AccountState, AccountUser, Frame, Platform};
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};
use crate::util::{js_str_or_empty, js_string, js_to_number, js_trim, now_ms, truthy, Timer};
use ua::set_user_agent;
use vendor::rumble::{drain_frames, fetch_can_moderate, fetch_rumble_emotes, parse_rumble_source, resolve_rumble_source, rumble_fetch, rumble_source_key, rumble_source_url, to_rumble_chat_msg, RumbleSource};
use vendor::rumble_auth::{self as auth, LoginResult, RumbleError, TwoFactor};
use vendor::rumble_hosts::{hosts_for, mark_host};

const MAX_BACKLOG: usize = 200;
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 60_000;
// Rumble's channel page is a ~500 KB fetch, so an idle source re-checks slowly.
const OFFLINE_POLL_MS: u64 = 60_000;
// A live source re-checks liveness rarely; the SSE ending is the primary "it's over" signal.
const LIVE_RECHECK_MS: u64 = 5 * 60_000;
// Rumble sends a `: ` heartbeat even when chat is quiet, so a long silence means a dead connection.
const IDLE_TIMEOUT_MS: i64 = 5 * 60_000;
const SECRET_KEY: &str = "rumble";
const TWO_FACTOR_TTL_MS: i64 = 5 * 60_000;
const CAPS_TTL_MS: i64 = 5 * 60_000;

static APP_CTX: Mutex<Option<Arc<Ctx>>> = Mutex::new(None);

fn app_ctx() -> Option<Arc<Ctx>> {
    APP_CTX.lock().unwrap().clone()
}

pub struct Rumble;

/// `normalizeChannel`
pub fn normalize_channel(input: &str) -> Result<String, String> {
    let src = parse_rumble_source(input).ok_or_else(|| "Enter a Rumble channel name, a rumble.com channel link, or a video link.".to_string())?;
    Ok(if src.kind == "video" { src.id } else { format!("{}/{}", src.space.unwrap_or_default(), src.id.to_lowercase()) })
}

// ---------------------------------------------------------------------------
// Account (the user's own Rumble login)
// ---------------------------------------------------------------------------

static LIVE_SOURCES: LazyLock<Mutex<HashMap<String, Weak<RumbleSource_>>>> = LazyLock::new(|| Mutex::new(HashMap::new())); // normalized channel -> source
/// in memory only, never persisted
static PENDING_2FA: Mutex<Option<(TwoFactor, String, i64)>> = Mutex::new(None); // { challenge, username, expiresAt }

/// `{session, username}` of the stored login, if any.
fn stored_account() -> Option<(String, String)> {
    let ctx = app_ctx()?;
    let a = ctx.secrets.get(SECRET_KEY)?;
    if truthy(&a["session"]) {
        Some((js_string(&a["session"]), js_str_or_empty(Some(&a["username"]))))
    } else {
        None
    }
}

fn refresh_all_conns() {
    let list: Vec<_> = LIVE_SOURCES.lock().unwrap().values().filter_map(|w| w.upgrade()).collect();
    for s in list {
        s.refresh_conn();
    }
}

fn user_json(username: &str) -> Value {
    json!({"connected": true, "user": {"id": "", "login": username, "name": username}})
}

async fn finish_login(session: String, fallback_name: &str) -> Result<Value, String> {
    let who = auth::whoami(&session).await;
    let username = who.map(|w| w.username).filter(|u| !u.is_empty()).unwrap_or_else(|| fallback_name.to_string());
    let ctx = app_ctx().ok_or("Rumble is not initialised.")?;
    ctx.secrets.set(SECRET_KEY, json!({"session": session, "username": username}))?;
    ctx.accounts_changed();
    refresh_all_conns();
    Ok(user_json(&username))
}

struct RumbleAccounts;

impl Accounts for RumbleAccounts {
    fn state(&self) -> AccountState {
        match stored_account() {
            Some((_, u)) => AccountState { connected: true, user: Some(AccountUser { id: String::new(), login: u.clone(), name: u }), ..Default::default() },
            None => AccountState::default(),
        }
    }

    /// {username, password} -> {connected:true,user} | {twoFactor:true}; then {code} finishes a 2FA login.
    fn login(&self, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        Box::pin(async move {
            if app_ctx().is_none() {
                return Err("Rumble is not initialised.".into());
            }
            if truthy(&args["code"]) {
                let p = PENDING_2FA.lock().unwrap().clone();
                let Some((challenge, username, expires_at)) = p.filter(|(_, _, exp)| now_ms() <= *exp) else {
                    *PENDING_2FA.lock().unwrap() = None;
                    return Err("That login attempt expired. Start again.".into());
                };
                let _ = expires_at;
                let session = auth::complete_two_factor(&challenge, js_trim(&js_string(&args["code"]))).await.map_err(|e| e.message)?;
                *PENDING_2FA.lock().unwrap() = None;
                return finish_login(session, &username).await;
            }
            let username = js_trim(&js_str_or_empty(Some(&args["username"]))).to_string();
            let password = js_str_or_empty(Some(&args["password"]));
            if username.is_empty() || password.is_empty() {
                return Err("Rumble username and password are required.".into());
            }
            // The password lives only inside this call: hashed by auth::login(), then dropped. It is never
            // stored or logged, and error messages come from Rumble itself.
            match auth::login(&username, &password).await.map_err(|e| e.message)? {
                LoginResult::TwoFactor(challenge) => {
                    *PENDING_2FA.lock().unwrap() = Some((challenge, username, now_ms() + TWO_FACTOR_TTL_MS));
                    Ok(json!({"twoFactor": true}))
                }
                LoginResult::Session(session) => finish_login(session, &username).await,
            }
        })
    }

    fn cancel(&self) {
        *PENDING_2FA.lock().unwrap() = None;
    }

    fn logout(&self) -> BoxFuture<'static, ()> {
        Box::pin(async {
            *PENDING_2FA.lock().unwrap() = None;
            if let Some((session, _)) = stored_account() {
                auth::logout(&session).await; // best effort; never blocks disconnecting
            }
            if let Some(ctx) = app_ctx() {
                let _ = ctx.secrets.delete(SECRET_KEY); // nothing stored is fine
            }
            refresh_all_conns();
        })
    }
}

// ---------------------------------------------------------------------------
// Routes: POST /api/rumble/send, POST /api/rumble/action, GET /api/rumble/caps
// ---------------------------------------------------------------------------

/// What a route can fail with: our own http error (403/409) or Rumble's.
enum Fail {
    Http(u16, String),
    Rumble(RumbleError),
}

impl From<RumbleError> for Fail {
    fn from(e: RumbleError) -> Self {
        Fail::Rumble(e)
    }
}

// Same mapping as server.js: a refusal Rumble deliberately returned is a 403 the user should read, not a
// gateway failure.
fn fail_res(f: Fail, fallback: &str) -> RouteRes {
    let (status, message) = match f {
        Fail::Http(s, m) => (s, m),
        Fail::Rumble(e) => (if e.refused || e.expired { 403 } else { 502 }, e.message),
    };
    RouteRes { status, body: json!({"error": if message.is_empty() { fallback.to_string() } else { message }}) }
}

static CAPS_CACHE: LazyLock<Mutex<HashMap<String, (bool, i64)>>> = LazyLock::new(|| Mutex::new(HashMap::new())); // chatId -> { canModerate, at }

/// The chat id a SourceRef is live on: a running source's own answer (free), else one fresh resolve.
async fn chat_id_for(channel: &str) -> (Option<String>, String) {
    let Ok(key) = normalize_channel(channel) else { return (None, String::new()) };
    let live = LIVE_SOURCES.lock().unwrap().get(&key).and_then(|w| w.upgrade());
    if let Some(live) = live {
        if let Some(id) = live.chat_id() {
            return (Some(id), live.url());
        }
    }
    // No SSE stream is opened just to answer "what is the id": that would leak a connection per request.
    let Some(src) = parse_rumble_source(&key) else { return (None, String::new()) };
    match resolve_rumble_source(&src).await {
        Ok(res) => (if res.live { res.chat_id } else { None }, res.url),
        Err(_) => (None, String::new()), // JS lets this throw into the router's 500; callers here treat it as "not live"
    }
}

struct Actor {
    session: String,
    chat_id: String,
    referer: String,
}

/// The three things every write needs: the user's session, and the chat id the channel is live on.
async fn require_actor(channel: &str) -> Result<Actor, Fail> {
    let Some((session, _)) = stored_account() else { return Err(Fail::Http(403, "Connect your Rumble account in Settings first".into())) };
    let (chat_id, url) = chat_id_for(channel).await;
    let Some(chat_id) = chat_id else { return Err(Fail::Http(409, "That Rumble channel isn't live right now".into())) };
    // The page a real client would be sitting on when it sends. Best-effort; a generic referer beats none.
    Ok(Actor { session, chat_id, referer: if url.is_empty() { "https://rumble.com/".into() } else { url } })
}

fn ok_res() -> RouteRes {
    RouteRes { status: 200, body: json!({"ok": true}) }
}

fn bad(m: &str) -> RouteRes {
    RouteRes { status: 400, body: json!({"error": m}) }
}

async fn route_send(req: RouteReq) -> Result<RouteRes, RouteError> {
    let text = js_trim(&js_str_or_empty(Some(&req.body["text"]))).to_string();
    if text.is_empty() {
        return Ok(bad("empty message"));
    }
    let r: Result<(), Fail> = async {
        let g = require_actor(&req.source.channel).await?;
        auth::send_message(&g.session, &g.chat_id, &text, Some(&g.referer), None).await?;
        Ok(())
    }
    .await;
    Ok(r.map(|_| ok_res()).unwrap_or_else(|f| fail_res(f, "Rumble rejected the message")))
}

async fn route_action(req: RouteReq) -> Result<RouteRes, RouteError> {
    let b = &req.body;
    let action = js_str_or_empty(Some(&b["action"]));
    let username = js_trim(&js_str_or_empty(Some(&b["username"]))).to_string();
    let r: Result<RouteRes, Fail> = async {
        let g = require_actor(&req.source.channel).await?;
        match action.as_str() {
            "timeout" => {
                if username.is_empty() {
                    return Ok(bad("missing username"));
                }
                // Clamp to a week so a typo can't become an effectively permanent mute dressed up as a timeout.
                let n = js_to_number(Some(&b["seconds"]));
                let n = if n.is_nan() || n == 0.0 { 300.0 } else { n.floor() };
                let seconds = n.max(1.0).min(604_800.0) as u64;
                auth::timeout_user(&g.session, &g.chat_id, &username, seconds).await?;
            }
            "ban" => {
                if username.is_empty() {
                    return Ok(bad("missing username"));
                }
                auth::ban_user(&g.session, &g.chat_id, &username).await?;
            }
            "unban" => {
                if username.is_empty() {
                    return Ok(bad("missing username"));
                }
                auth::unban_user().await?;
            }
            "delete" => {
                let message_id = js_str_or_empty(Some(&b["messageId"]));
                if message_id.is_empty() {
                    return Ok(bad("missing messageId"));
                }
                auth::delete_message(&g.session, &g.chat_id, &message_id).await?;
            }
            _ => return Ok(bad("unknown action")),
        }
        Ok(ok_res())
    }
    .await;
    Ok(r.unwrap_or_else(|f| fail_res(f, "Rumble rejected the action")))
}

// connected and canModerate are answered independently: being logged in has nothing to do with whether
// anyone is streaming. can_moderate needs an AUTHENTICATED read of the chat's init frame (the shared anonymous
// stream always reports false), cached per chat id since it changes only on promotion.
async fn route_caps(req: RouteReq) -> Result<RouteRes, RouteError> {
    let Some((session, _)) = stored_account() else { return Ok(RouteRes { status: 200, body: json!({"connected": false, "canModerate": false}) }) };
    let (chat_id, _) = chat_id_for(&req.source.channel).await;
    let Some(chat_id) = chat_id else { return Ok(RouteRes { status: 200, body: json!({"connected": true, "canModerate": false}) }) };
    let hit = CAPS_CACHE.lock().unwrap().get(&chat_id).copied();
    if let Some((can, at)) = hit {
        if now_ms() - at < CAPS_TTL_MS {
            return Ok(RouteRes { status: 200, body: json!({"connected": true, "canModerate": can}) });
        }
    }
    let can = fetch_can_moderate(&session, &chat_id).await;
    CAPS_CACHE.lock().unwrap().insert(chat_id, (can, now_ms()));
    Ok(RouteRes { status: 200, body: json!({"connected": true, "canModerate": can}) })
}

fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(RouteReq) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<RouteRes, RouteError>> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

impl PlatformModule for Rumble {
    fn platform(&self) -> Platform {
        Platform::Rumble
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        normalize_channel(input)
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        {
            let mut g = APP_CTX.lock().unwrap();
            if g.is_none() {
                *g = Some(ctx.clone());
            }
        }
        set_user_agent(&ctx.user_agent);
        // JS throws "Not a Rumble source" for an unparsable input; the relay has already normalized it.
        let src = parse_rumble_source(&channel).unwrap_or(RumbleSource { kind: "channel", id: channel.clone(), space: Some("c".into()) });
        RumbleSource_::new(channel, src, emit, ctx)
    }

    fn routes(&self) -> Vec<(String, Handler)> {
        vec![
            ("POST /api/rumble/send".into(), handler(route_send)),
            ("POST /api/rumble/action".into(), handler(route_action)),
            ("GET /api/rumble/caps".into(), handler(route_caps)),
        ]
    }

    fn accounts(&self) -> Option<Arc<dyn Accounts>> {
        Some(Arc::new(RumbleAccounts))
    }

    fn init(&self, ctx: Arc<Ctx>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            set_user_agent(&ctx.user_agent);
            *APP_CTX.lock().unwrap() = Some(ctx);
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// The source
// ---------------------------------------------------------------------------

#[derive(PartialEq, Clone, Copy)]
enum Stream {
    Off,
    Connecting,
    Open,
}

struct St {
    status: Map<String, Value>,
    backlog: Vec<Value>,
    badges: Value,
    emotes: Value,
    // Rumble only re-sends a user with the message that mentions them, so the side tables accumulate per chat id.
    users: HashMap<String, Value>,
    channels: HashMap<String, Value>,
    stream_state: Stream,
    chat_disabled: bool, // 204: a property of the video, reported once, not retried
    stream_gen: u64,
    stream_abort: Option<AbortHandle>,
    reconnect_delay: u64,
    reconnect_timer: Timer,
    reconnect_pending: bool,
    resolve_timer: Timer,
    idle_timer: Timer,
    last_frame_at: i64,
    host_index: usize,
    resolving: bool,
    resolve_failures: u32,
    stopped: bool,
    started: bool,
    resolved_once: bool, // until the first answer (or failure), the source is still "connecting"
    last_conn_json: String,
}

/// (named with a trailing underscore: `RumbleSource` is the parsed-source struct of the vendor file)
pub struct RumbleSource_ {
    me: Weak<RumbleSource_>,
    channel: String,
    src: RumbleSource,
    key: String,
    emit_fn: Emit,
    ctx: Arc<Ctx>,
    jobs: Jobs,
    st: Mutex<St>,
}

fn status_eq(a: &Map<String, Value>, b: &Map<String, Value>) -> bool {
    ["live", "chatId", "title", "watching", "error", "canModerate"].iter().all(|k| a.get(*k) == b.get(*k))
}

fn conn_frame(st: &St, connected: bool) -> Value {
    let s = &st.status;
    let live = truthy(&s["live"]);
    let (state, detail): (&str, String) = if !st.started || !st.resolved_once {
        ("connecting", "Connecting to Rumble...".into())
    } else if truthy(&s["error"]) && (st.chat_disabled || st.resolve_failures >= 3) {
        (if st.chat_disabled { "unavailable" } else { "error" }, js_string(&s["error"]))
    } else if !live {
        ("offline", "Not live".into())
    } else if st.stream_state == Stream::Open {
        ("open", "Live".into())
    } else {
        ("connecting", "Connecting to Rumble chat...".into())
    };
    let can_send = connected && live && !st.chat_disabled;
    let mut f = json!({"type": "conn", "state": state, "detail": detail, "live": live});
    let title = js_str_or_empty(s.get("title"));
    if !title.is_empty() {
        f["title"] = json!(title);
    }
    f["canSend"] = json!(can_send);
    if !can_send {
        f["readOnlyReason"] = json!(if !connected { "Connect your Rumble account to chat.".to_string() } else if st.chat_disabled { js_str_or_empty(s.get("error")) } else { "Not live".to_string() });
    }
    f
}

impl RumbleSource_ {
    fn new(channel: String, src: RumbleSource, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        let key = rumble_source_key(&src);
        let mut status = Map::new();
        status.insert("live".into(), json!(false));
        status.insert("chatId".into(), Value::Null);
        status.insert("title".into(), json!(""));
        status.insert("author".into(), json!(""));
        status.insert("category".into(), json!(""));
        status.insert("url".into(), json!(rumble_source_url(&src)));
        status.insert("watching".into(), Value::Null);
        status.insert("error".into(), Value::Null);
        Arc::new_cyclic(|me| RumbleSource_ {
            me: me.clone(),
            channel,
            src,
            key,
            emit_fn: emit,
            ctx,
            jobs: Jobs::new(),
            st: Mutex::new(St {
                status,
                backlog: Vec::new(),
                badges: json!({}),
                emotes: json!({}),
                users: HashMap::new(),
                channels: HashMap::new(),
                stream_state: Stream::Off,
                chat_disabled: false,
                stream_gen: 0,
                stream_abort: None,
                reconnect_delay: RECONNECT_BASE_MS,
                reconnect_timer: Timer::new(),
                reconnect_pending: false,
                resolve_timer: Timer::new(),
                idle_timer: Timer::new(),
                last_frame_at: 0,
                host_index: 0,
                resolving: false,
                resolve_failures: 0,
                stopped: false,
                started: false,
                resolved_once: false,
                last_conn_json: String::new(),
            }),
        })
    }

    fn stopped(&self) -> bool {
        self.st.lock().unwrap().stopped
    }

    fn conn(&self) -> Value {
        let connected = stored_account().is_some();
        conn_frame(&self.st.lock().unwrap(), connected)
    }

    fn publish_conn(&self) {
        let connected = stored_account().is_some();
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st, connected);
            let j = f.to_string();
            if j == st.last_conn_json {
                return;
            }
            st.last_conn_json = j;
            f
        };
        (self.emit_fn)(f);
    }

    fn chatlog_frame(&self) -> Value {
        let st = self.st.lock().unwrap();
        json!({
            "type": "chatlog",
            "source": {"key": self.key, "kind": self.src.kind, "id": self.src.id, "space": self.src.space},
            "status": Value::Object(st.status.clone()),
            "badges": st.badges,
            "emotes": st.emotes,
            "messages": st.backlog,
        })
    }

    fn broadcast(&self, frame: Frame) {
        if !self.stopped() {
            (self.emit_fn)(frame);
        }
    }

    fn push_backlog(&self, msg: Value) {
        let mut st = self.st.lock().unwrap();
        st.backlog.push(msg);
        if st.backlog.len() > MAX_BACKLOG {
            let cut = st.backlog.len() - MAX_BACKLOG;
            st.backlog.drain(..cut);
        }
    }

    fn set_status(&self, next: Vec<(&str, Value)>) {
        let changed = {
            let mut st = self.st.lock().unwrap();
            let before = st.status.clone();
            for (k, v) in next {
                st.status.insert(k.to_string(), v);
            }
            (!status_eq(&before, &st.status)).then(|| Value::Object(st.status.clone()))
        };
        if let Some(status) = changed {
            self.broadcast(json!({"type": "status", "status": status}));
        }
        self.publish_conn();
    }

    pub fn chat_id(&self) -> Option<String> {
        let st = self.st.lock().unwrap();
        if truthy(&st.status["live"]) && truthy(&st.status["chatId"]) { Some(js_string(&st.status["chatId"])) } else { None }
    }

    pub fn url(&self) -> String {
        js_str_or_empty(self.st.lock().unwrap().status.get("url"))
    }

    pub fn refresh_conn(&self) {
        if !self.stopped() {
            self.publish_conn();
        }
    }

    async fn load_emotes(self: Arc<Self>, chat_id: String) {
        let found = fetch_rumble_emotes(&chat_id).await;
        {
            let mut st = self.st.lock().unwrap();
            if st.stopped || js_str_or_empty(st.status.get("chatId")) != chat_id {
                return; // moved on to another broadcast
            }
            st.emotes = found.clone();
        }
        if found.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            self.broadcast(json!({"type": "emotes", "emotes": found}));
        }
    }

    // Badge definitions change only when Rumble ships new artwork, so broadcast on change.
    fn set_badges(&self, next: &Value) {
        if !next.is_object() && !next.is_array() {
            return;
        }
        {
            let mut st = self.st.lock().unwrap();
            if next.to_string() == st.badges.to_string() {
                return;
            }
            st.badges = next.clone();
        }
        self.broadcast(json!({"type": "badges", "badges": next}));
    }

    // ---------- resolution lifecycle ----------

    async fn ensure_resolved(self: Arc<Self>) {
        {
            let mut st = self.st.lock().unwrap();
            if st.resolving || st.stopped {
                return;
            }
            st.resolving = true;
        }
        let r = resolve_rumble_source(&self.src).await;
        if !self.stopped() {
            match r {
                Ok(res) => self.apply_resolved(res),
                Err(msg) => {
                    let (failures, _) = {
                        let mut st = self.st.lock().unwrap();
                        st.resolve_failures += 1;
                        st.resolved_once = true;
                        (st.resolve_failures, ())
                    };
                    // Surface only once it has persisted, so one bad fetch doesn't flash an error at the user.
                    if failures >= 3 {
                        self.set_status(vec![("error", json!(msg))]);
                    } else {
                        self.publish_conn();
                    }
                }
            }
        }
        self.st.lock().unwrap().resolving = false;
        self.schedule_resolve();
    }

    fn apply_resolved(self: &Arc<Self>, res: vendor::rumble::Resolved) {
        let (changed, cur_error, cur_url, chat_disabled) = {
            let mut st = self.st.lock().unwrap();
            st.resolve_failures = 0;
            st.resolved_once = true;
            let cur = if truthy(&st.status["chatId"]) { Some(js_string(&st.status["chatId"])) } else { None };
            let changed = res.chat_id != cur;
            if changed {
                st.chat_disabled = false;
            }
            (changed, st.status["error"].clone(), js_str_or_empty(st.status.get("url")), st.chat_disabled)
        };
        self.set_status(vec![
            ("live", json!(res.live)),
            ("chatId", res.chat_id.clone().map(Value::from).unwrap_or(Value::Null)),
            ("title", json!(res.title)),
            // Only a live stream's category means anything; between broadcasts there is no "current" one.
            ("category", json!(if res.live { res.category.clone() } else { String::new() })),
            ("author", json!(res.author)),
            ("url", json!(if res.url.is_empty() { cur_url } else { res.url.clone() })),
            ("watching", res.watching.map(Value::from).unwrap_or(Value::Null)),
            ("error", if chat_disabled { cur_error } else { Value::Null }),
        ]);

        if !res.live || res.chat_id.is_none() {
            // Between broadcasts: drop the upstream stream but KEEP the backlog, so a viewer who was reading
            // when the stream ended still sees the last messages.
            self.close_stream();
        } else if changed || self.st.lock().unwrap().stream_state == Stream::Off {
            if changed {
                // A new broadcast: the old chat's users/messages belong to a different video.
                {
                    let mut st = self.st.lock().unwrap();
                    st.users.clear();
                    st.channels.clear();
                    st.backlog.clear();
                    st.badges = json!({});
                    st.emotes = json!({});
                }
                let f = self.chatlog_frame();
                self.broadcast(f);
            }
            self.close_stream();
            self.open_stream();
            let (s, id) = (self.clone(), res.chat_id.clone().unwrap());
            self.jobs.spawn(s.load_emotes(id));
        }
    }

    fn schedule_resolve(self: &Arc<Self>) {
        let mut st = self.st.lock().unwrap();
        st.resolve_timer.clear();
        if st.stopped {
            return;
        }
        // Back off hard on repeated failures so a renamed/deleted channel doesn't become a fetch loop.
        let base = if truthy(&st.status["live"]) { LIVE_RECHECK_MS } else { OFFLINE_POLL_MS };
        let delay = if st.resolve_failures > 0 { (base * 2u64.pow(st.resolve_failures.min(4))).min(15 * 60_000) } else { base };
        let s = self.clone();
        st.resolve_timer.set(Duration::from_millis(delay), async move {
            let s2 = s.clone();
            s.jobs.spawn(s2.ensure_resolved());
        });
    }

    // ---------- SSE lifecycle ----------

    fn open_stream(self: &Arc<Self>) {
        let (chat_id, gen) = {
            let mut st = self.st.lock().unwrap();
            if st.stream_state != Stream::Off || !truthy(&st.status["chatId"]) || st.stopped || st.chat_disabled {
                return;
            }
            st.stream_state = Stream::Connecting;
            st.stream_gen += 1;
            (js_string(&st.status["chatId"]), st.stream_gen)
        };
        self.publish_conn();
        let s = self.clone();
        let h = tokio::spawn(async move {
            if let Err(err) = s.clone().read_stream(chat_id, gen).await {
                if s.st.lock().unwrap().stream_gen != gen {
                    return; // our own teardown
                }
                let _ = err; // lastError: only kept for debugging in the JS
                s.on_stream_end();
            }
        });
        let mut st = self.st.lock().unwrap();
        if st.stream_gen == gen && !st.stopped {
            st.stream_abort = Some(h.abort_handle());
        } else {
            h.abort();
        }
    }

    fn close_stream(&self) {
        let abort = {
            let mut st = self.st.lock().unwrap();
            st.reconnect_timer.clear();
            st.reconnect_pending = false;
            st.idle_timer.clear();
            st.stream_state = Stream::Off;
            st.stream_gen += 1;
            st.stream_abort.take()
        };
        if let Some(a) = abort {
            a.abort();
        }
    }

    fn on_stream_end(self: &Arc<Self>) {
        if self.st.lock().unwrap().stream_state == Stream::Off {
            return;
        }
        self.close_stream();
        self.publish_conn();
        self.schedule_reconnect();
    }

    fn schedule_reconnect(self: &Arc<Self>) {
        let mut st = self.st.lock().unwrap();
        if st.stopped || !truthy(&st.status["live"]) || st.chat_disabled || st.reconnect_pending {
            return;
        }
        // Jitter so sources don't retry in lockstep after a shared network blip.
        let cap = 1000.0f64.min(1.0f64.max(st.reconnect_delay as f64 / 4.0));
        let delay = st.reconnect_delay + (rand::random::<f64>() * cap).floor() as u64;
        st.reconnect_pending = true;
        let s = self.clone();
        st.reconnect_timer.set(Duration::from_millis(delay), async move {
            {
                let mut st = s.st.lock().unwrap();
                st.reconnect_pending = false;
                // Move to the next host on every retry: a closed host, or a 404 from one front-end for a chat id
                // another serves happily, is exactly the failure the host list exists for.
                st.host_index += 1;
            }
            s.open_stream();
        });
        st.reconnect_delay = (st.reconnect_delay * 2).min(RECONNECT_MAX_MS);
    }

    // Reads one SSE connection to completion. Rumble never closes a healthy stream, so returning at all means
    // the broadcast ended or the connection broke: both handled by the caller's on_stream_end.
    async fn read_stream(self: Arc<Self>, chat_id: String, gen: u64) -> Result<(), String> {
        let host = {
            let hosts = hosts_for("chat");
            let idx = self.st.lock().unwrap().host_index;
            hosts[idx % hosts.len()].clone()
        };
        let url = format!("https://{host}.rumble.com/chat/api/chat/{}/stream", vendor::rumble::enc(&chat_id));
        // timeout 0: the whole point is a connection that stays open; teardown is the task abort.
        let res = match rumble_fetch(&url, FetchOpts::new(0).header("Accept", "text/event-stream")).await {
            Ok(r) => r,
            Err(e) => {
                if self.st.lock().unwrap().stream_gen == gen {
                    mark_host(&host, "chat", false);
                }
                return Err(e.message);
            }
        };
        if res.status().is_success() {
            mark_host(&host, "chat", true);
            self.st.lock().unwrap().host_index = 0;
        }

        // 204 is Rumble's "this video has no chat": a property of the video, so report it and stop rather than retry.
        if res.status().as_u16() == 204 {
            {
                let mut st = self.st.lock().unwrap();
                st.chat_disabled = true;
                st.stream_state = Stream::Off;
            }
            self.set_status(vec![("error", json!("Chat is disabled for this stream."))]);
            return Ok(());
        }
        if !res.status().is_success() {
            return Err(format!("rumble stream {}", res.status().as_u16()));
        }

        {
            let mut st = self.st.lock().unwrap();
            if st.stream_gen != gen {
                return Ok(());
            }
            st.stream_state = Stream::Open;
            st.reconnect_delay = RECONNECT_BASE_MS;
            st.last_frame_at = now_ms();
        }
        self.start_idle_watchdog(gen);
        self.publish_conn();

        let mut stream = res.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            {
                let mut st = self.st.lock().unwrap();
                if st.stream_gen != gen {
                    return Ok(());
                }
                st.last_frame_at = now_ms();
            }
            buf.extend_from_slice(&chunk);
            // SSE frames are separated by a blank line; anything after the last separator is a partial frame.
            for frame in drain_frames(&mut buf) {
                self.handle_frame(&frame);
            }
            if buf.len() > 1_000_000 {
                buf.clear(); // an unbounded line must not grow this without limit
            }
        }
        Err("rumble stream ended".into())
    }

    fn start_idle_watchdog(self: &Arc<Self>, gen: u64) {
        let s = self.clone();
        self.st.lock().unwrap().idle_timer.every(Duration::from_secs(30), move || {
            let s = s.clone();
            async move {
                let idle = {
                    let st = s.st.lock().unwrap();
                    if st.stream_gen != gen {
                        return;
                    }
                    now_ms() - st.last_frame_at >= IDLE_TIMEOUT_MS
                };
                if idle {
                    s.ctx.log(&format!("[rumble] {}: idle {IDLE_TIMEOUT_MS}ms, reconnecting", s.key));
                    s.on_stream_end();
                }
            }
        });
    }

    fn handle_frame(self: &Arc<Self>, frame: &str) {
        // Lines starting with ':' are heartbeat comments and carry nothing.
        let data_lines: Vec<&str> = frame.split('\n').filter_map(|l| l.strip_prefix("data:")).map(|l| l.trim_start()).collect();
        if data_lines.is_empty() {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(&data_lines.join("\n")) else { return }; // a truncated or non-JSON frame is not worth killing the stream
        let data = &event["data"];
        if !truthy(data) {
            return;
        }

        // Side tables first: a message in this same frame may reference them.
        {
            let mut st = self.st.lock().unwrap();
            for u in data["users"].as_array().into_iter().flatten() {
                if !u["id"].is_null() {
                    st.users.insert(js_string(&u["id"]), u.clone());
                }
            }
            for c in data["channels"].as_array().into_iter().flatten() {
                if !c["id"].is_null() {
                    st.channels.insert(js_string(&c["id"]), c.clone());
                }
            }
        }
        let to_msg = |raw: &Value| {
            let st = self.st.lock().unwrap();
            to_rumble_chat_msg(raw, &st.users, &st.channels)
        };

        match event["type"].as_str().unwrap_or("") {
            "init" => {
                // Rumble ships badge artwork and labels with the chat itself (icon paths are relative to rumble.com).
                if truthy(&data["config"]["badges"]) {
                    self.set_badges(&data["config"]["badges"]);
                }
                let msgs: Vec<Value> = data["messages"].as_array().cloned().unwrap_or_default();
                self.seed_backlog(&msgs);
                self.set_status(vec![("canModerate", json!(truthy(&data["can_moderate"])))]);
            }
            "messages" => {
                for raw in data["messages"].as_array().into_iter().flatten() {
                    let msg = to_msg(raw);
                    self.push_backlog(msg.clone());
                    self.broadcast(json!({"type": "chat", "msg": msg}));
                }
            }
            "delete_messages" => self.apply_deletes(data["message_ids"].as_array().cloned().unwrap_or_default()),
            // Rumble hides every past message from a muted user rather than deleting individual ones.
            "mute_users" => self.apply_mutes(data["user_ids"].as_array().cloned().unwrap_or_default()),
            "pin_message" => {
                let msg = if truthy(&data["message"]) { to_msg(&data["message"]) } else { Value::Null };
                self.broadcast(json!({"type": "pin", "msg": msg}));
            }
            "unpin_message" => self.broadcast(json!({"type": "pin", "msg": null})),
            // Rumble ships new event types (raids, gifted subs, live gates) without warning; ignoring the unknown
            // ones is the whole reason this is a match and not an assertion.
            _ => {}
        }
    }

    fn seed_backlog(&self, raw_messages: &[Value]) {
        let added = {
            let mut st = self.st.lock().unwrap();
            let mut seen: HashSet<String> = st.backlog.iter().map(|m| js_string(&m["id"])).collect();
            let mut added = 0;
            for raw in raw_messages {
                let msg = to_rumble_chat_msg(raw, &st.users, &st.channels);
                if !seen.insert(js_string(&msg["id"])) {
                    continue;
                }
                st.backlog.push(msg);
                added += 1;
            }
            st.backlog.sort_by_key(|m| m["ts"].as_i64().unwrap_or(0));
            if st.backlog.len() > MAX_BACKLOG {
                let cut = st.backlog.len() - MAX_BACKLOG;
                st.backlog.drain(..cut);
            }
            added
        };
        // History lands after the subscriber's hello, so re-send the whole log once it arrives. Doubles as the
        // resync after a reconnect or a swap to a new broadcast; silent when nothing new came.
        if added > 0 {
            let f = self.chatlog_frame();
            self.broadcast(f);
        }
    }

    fn apply_deletes(&self, ids: Vec<Value>) {
        let mut set: Vec<String> = Vec::new();
        for i in &ids {
            let s = js_string(i);
            if !set.contains(&s) {
                set.push(s);
            }
        }
        if set.is_empty() {
            return;
        }
        for msg in self.st.lock().unwrap().backlog.iter_mut() {
            if set.contains(&js_string(&msg["id"])) {
                msg["deleted"] = json!(true);
            }
        }
        self.broadcast(json!({"type": "delete", "ids": set}));
    }

    fn apply_mutes(&self, user_ids: Vec<Value>) {
        let mut set: Vec<String> = Vec::new();
        for i in &user_ids {
            let s = js_string(i);
            if !set.contains(&s) {
                set.push(s);
            }
        }
        if set.is_empty() {
            return;
        }
        for msg in self.st.lock().unwrap().backlog.iter_mut() {
            if set.contains(&js_string(&msg["user"]["id"])) {
                msg["deleted"] = json!(true);
            }
        }
        self.broadcast(json!({"type": "mute", "userIds": set}));
    }
}

impl Source for RumbleSource_ {
    fn start(self: Arc<Self>) {
        self.st.lock().unwrap().started = true;
        if let Ok(norm) = normalize_channel(&self.channel) {
            LIVE_SOURCES.lock().unwrap().insert(norm, Arc::downgrade(&self));
        }
        let f = self.conn();
        self.st.lock().unwrap().last_conn_json = f.to_string();
        (self.emit_fn)(f);
        let s = self.clone();
        self.jobs.spawn(s.ensure_resolved());
    }

    fn stop(&self) {
        self.st.lock().unwrap().stopped = true;
        if let Ok(norm) = normalize_channel(&self.channel) {
            let mut l = LIVE_SOURCES.lock().unwrap();
            if l.get(&norm).map(|w| Weak::ptr_eq(w, &self.me)).unwrap_or(false) {
                l.remove(&norm);
            }
        }
        self.close_stream();
        self.st.lock().unwrap().resolve_timer.clear();
        self.jobs.abort_all();
    }

    fn snapshot(&self) -> Vec<Frame> {
        vec![self.chatlog_frame(), self.conn()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_channel_forms() {
        let n = |s: &str| normalize_channel(s);
        assert_eq!(n("Foo"), Ok("c/foo".into()));
        assert_eq!(n("c/Foo"), Ok("c/foo".into()));
        assert_eq!(n("user/FooBar"), Ok("user/foobar".into()));
        assert_eq!(n("https://rumble.com/c/Foo"), Ok("c/foo".into()));
        assert_eq!(n("rumble.com/user/Foo?x=1"), Ok("user/foo".into()));
        assert_eq!(n("https://rumble.com/v7eh2s2-some-title.html"), Ok("v7eh2s2".into()));
        assert_eq!(n("v7capuk"), Ok("v7capuk".into()));
        assert_eq!(n("embed/V7CAPUK"), Ok("v7capuk".into()));
        let e = "Enter a Rumble channel name, a rumble.com channel link, or a video link.";
        assert_eq!(n(""), Err(e.into()));
        assert_eq!(n("a b"), Err(e.into()));
        assert_eq!(n("https://rumble.com/"), Err(e.into()));
        // stable: normalizing the normalized form gives itself
        assert_eq!(n("c/foo"), Ok("c/foo".into()));
        assert_eq!(n("v7capuk"), Ok("v7capuk".into()));
    }
}
