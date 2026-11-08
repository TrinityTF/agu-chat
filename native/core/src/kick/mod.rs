//! Port of src/main/kick/index.js: the Kick platform module. See docs/ARCHITECTURE.md.
//!
//! HOW CHAT GETS HERE. Kick's own web client reads chat over a public Pusher websocket, and so does this:
//!   1. GET  kick.com/api/v2/channels/<slug>   -> channel id, chatroom id, live status (honest UA)
//!   2. POST web.kick.com/api/v1/realtime/channels/<id>/chat/connection -> the CURRENT Pusher app key + cluster
//!   3. wss://ws-<cluster>.pusher.com/app/<key> -> subscribe chatrooms.<chatroomId>.v2 and channel.<channelId>
//! Everything is unauthenticated and read-only, and every request carries the honest product UA.
//!
//! Emits the frames /ws/kick sends (chatlog, chat, status, emotes, banned) plus the uniform conn frame, plus
//! {type:"delete", ids}, {type:"clear"}, {type:"pin", message|null}.
//!
//! SENDING (not in the JS): Kick's token exchange needs a client secret, so the login goes through the Agu Chat
//! broker (auth.rs) and messages are posted with the user's own token through Kick's public API. Reading stays
//! anonymous and works without a login.
//!
//! Differences from the JS: timers are `util::Timer`s, the socket is `ws::connect` (whose `Error` event ends
//! the socket without a `Close`, so it is handled as one), `ws.terminate()` is followed by the close handling
//! by hand for the same reason, and `stop()` aborts the source's tasks (`net::Jobs`) instead of an
//! `AbortController`.
pub mod auth;
pub mod vendor;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};

use futures_util::future::BoxFuture;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::net::Jobs;
use crate::platforms::{Accounts, Ctx, Emit, PlatformModule, Source};
use crate::protocol::{AccountState, Frame, Platform};
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};
use crate::util::{js_str_or_empty, js_string, now_ms, parse_rfc3339_ms, truthy, uuid_v4, Timer};
use crate::ws::{self, WsEvent, WsHandle};
use vendor::kick::{badges_mark_mod, channel_user_is_mod, get_channel_user, fetch_history, fetch_kick_emotes, get_kick_channel, get_realtime_descriptor, parse_kick_source, sub_badge_for, to_kick_chat_msg, KickChannel, SubBadge};

const MAX_BACKLOG: usize = 300;
// Kick's chat works whether or not the channel is live, so the socket stays up; this only keeps the header's
// live pill, title, category and viewer count honest (and catches a missed StreamerIsLive event).
const STATUS_POLL_MS: u64 = 60_000;
const NOT_FOUND_RETRY_MS: u64 = 2 * 60_000;
const RECONNECT_BASE_MS: u64 = 1000;
const RECONNECT_MAX_MS: u64 = 60_000;
// Pusher's server asks for activity within ~120s. We ping when quiet and reconnect when a ping goes unanswered.
const PING_AFTER_MS: i64 = 60_000;
const DEAD_AFTER_MS: i64 = 150_000;
// Only a fallback for a failed descriptor fetch: the descriptor is authoritative (observed 2026-11-03).
const FALLBACK_DESCRIPTOR: (&str, &str) = ("34bf7a0ff419a2a775b9", "us2");
const PUSHER_VERSION: &str = "8.4.0";

static USER_IDS: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

static LAST_DESCRIPTOR: LazyLock<Mutex<Option<(String, String)>>> = LazyLock::new(|| Mutex::new(None));

pub struct Kick;

impl PlatformModule for Kick {
    fn platform(&self) -> Platform {
        Platform::Kick
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        parse_kick_source(input).ok_or_else(|| "Enter a Kick channel name or a kick.com link.".to_string())
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        // JS normalizes again here; the relay hands over an already normalized slug, and a stray input falls back to itself.
        let slug = parse_kick_source(&channel).unwrap_or(channel);
        auth::ensure_ctx(&ctx); // sources can be created before init() in tests
        KickSource::new(slug, emit, ctx)
    }

    fn routes(&self) -> Vec<(String, Handler)> {
        vec![("POST /api/kick/send".into(), Arc::new(|req| Box::pin(route_send(req)))), ("GET /api/kick/caps".into(), Arc::new(|req| Box::pin(route_caps(req)))), ("POST /api/kick/action".into(), Arc::new(|req| Box::pin(route_action(req)))), ("GET /api/kick/user".into(), Arc::new(|req| Box::pin(route_user(req))))]
    }

    fn accounts(&self) -> Option<Arc<dyn Accounts>> {
        Some(Arc::new(KickAccounts))
    }

    fn init(&self, ctx: Arc<Ctx>) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async move {
            auth::init(ctx);
            Ok(())
        })
    }
}

struct KickAccounts;

impl Accounts for KickAccounts {
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

/// The broadcaster's user id (what Kick's chat API wants), from the reader's own channel lookup when it has run.
async fn broadcaster_id(slug: &str, ua: &str) -> Result<i64, String> {
    if let Some(id) = USER_IDS.lock().unwrap().get(slug).copied() {
        return Ok(id);
    }
    let c = get_kick_channel(slug, ua).await?.ok_or_else(|| format!("No Kick channel called \"{slug}\"."))?;
    let id = c.user_id.parse::<i64>().map_err(|_| "Kick did not say who owns this channel".to_string())?;
    USER_IDS.lock().unwrap().insert(slug.to_string(), id);
    Ok(id)
}

/// POST /api/kick/send {text, replyTo?}
async fn route_send(req: RouteReq) -> Result<RouteRes, RouteError> {
    let text = js_str_or_empty(Some(&req.body["text"])).trim().to_string();
    if text.is_empty() {
        return Ok(route_err(400, "empty message"));
    }
    if !auth::is_logged_in() {
        return Ok(route_err(401, auth::LOGGED_OUT_REASON));
    }
    let slug = parse_kick_source(&req.source.channel).unwrap_or_else(|| req.source.channel.clone());
    let id = match broadcaster_id(&slug, &req.ctx.user_agent).await {
        Ok(id) => id,
        Err(e) => return Ok(route_err(502, &e)),
    };
    let reply = js_str_or_empty(Some(&req.body["replyTo"]));
    match auth::send_chat(&text, id, Some(reply.as_str()).filter(|r| !r.is_empty())).await {
        Ok(()) => Ok(RouteRes { status: 200, body: json!({"ok": true}) }),
        Err(e) => Ok(route_err(e.status, &e.message)),
    }
}

const SCOPE_BAN: &str = "moderation:ban";
const SCOPE_DELETE: &str = "moderation:chat_message:manage";
const API_BASE: &str = "https://api.kick.com/public/v1";
const MOD_CACHE_MS: i64 = 10 * 60_000;
const MAX_TIMEOUT_MINUTES: i64 = 10_080;

/// (channel slug, my user id) -> (when asked, is a mod there)
static MOD_CACHE: LazyLock<Mutex<HashMap<(String, String), (i64, bool)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// channel slug -> my own chat messages there carried a moderator / broadcaster badge (the fallback when kick.com's
/// user lookup is unavailable)
static MOD_SEEN: LazyLock<Mutex<HashMap<String, bool>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remember it when a message of the logged-in account carries a moderator badge in this channel.
fn note_own_badges(slug: &str, msg: &Value) {
    let Some(me) = auth::me() else { return };
    if msg["user"]["id"].as_str() == Some(me.id.as_str()) && badges_mark_mod(&msg["badges"]) {
        MOD_SEEN.lock().unwrap().insert(slug.to_string(), true);
    }
}

/// Is the logged-in account the broadcaster or a moderator of this channel? Cached for ten minutes.
async fn is_mod_here(slug: &str, ua: &str) -> bool {
    let Some(me) = auth::me() else { return false };
    if !me.slug.is_empty() && me.slug.eq_ignore_ascii_case(slug) {
        return true;
    }
    let key = (slug.to_string(), me.id.clone());
    if let Some((at, v)) = MOD_CACHE.lock().unwrap().get(&key).copied() {
        if now_ms() - at < MOD_CACHE_MS {
            return v;
        }
    }
    let seen = MOD_SEEN.lock().unwrap().get(slug).copied().unwrap_or(false);
    // This lookup takes the USERNAME (`Agu_TF`, case-insensitive); the slug (`agu-tf`) answers 404.
    let who = if me.name.is_empty() { me.slug.clone() } else { me.name.clone() };
    match get_channel_user(slug, &who, ua).await {
        Ok(u) => {
            let v = u.map(|u| channel_user_is_mod(&u)).unwrap_or(false) || seen;
            MOD_CACHE.lock().unwrap().insert(key, (now_ms(), v));
            v
        }
        Err(_) => seen,
    }
}

/// GET /api/kick/user?name=<username>: what the user card shows for a Kick chatter, in the shape the Twitch card
/// reads ({user: {login, displayName, avatar, createdAt}, followedAt, ban}). kick.com's channel-user lookup, keyed
/// by USERNAME (the slug answers 404). No login needed.
async fn route_user(req: RouteReq) -> Result<RouteRes, RouteError> {
    let name = req.query_get("name").unwrap_or("").trim().to_string();
    if name.is_empty() || name.len() > 64 || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Ok(route_err(400, "bad name"));
    }
    let slug = parse_kick_source(&req.source.channel).unwrap_or_else(|| req.source.channel.clone());
    match get_channel_user(&slug, &name, &req.ctx.user_agent).await {
        Ok(Some(u)) => Ok(RouteRes { status: 200, body: kick_card_user(&u) }),
        Ok(None) => Ok(route_err(404, "No such Kick user")),
        Err(e) => Ok(route_err(502, &e)),
    }
}

/// kick.com's channel-user answer -> the card's shape.
pub fn kick_card_user(u: &Value) -> Value {
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = json!({
        "user": {"login": s(&u["slug"]), "displayName": s(&u["username"]), "avatar": s(&u["profile_pic"]), "createdAt": s(&u["created_at"])},
        "followedAt": u["following_since"].as_str().map(Value::from).unwrap_or(Value::Null),
    });
    if u["banned"].is_object() {
        let b = &u["banned"];
        let expires = b["expires_at"].as_str().map(String::from);
        out["ban"] = json!({"permanent": b["permanent"].as_bool().unwrap_or(expires.is_none()), "expiresAt": expires, "reason": s(&b["reason"])});
    } else if u["banned"].as_bool() == Some(true) {
        out["ban"] = json!({"permanent": true, "expiresAt": Value::Null, "reason": ""});
    }
    out
}

/// GET /api/kick/caps: `connected` follows the login. canModerate (timeout, ban, unban) needs the moderation:ban scope and
/// canDelete the moderation:chat_message:manage scope, and both need the account to be the broadcaster or a moderator of
/// this channel (kick.com's user lookup, else a moderator badge seen on the account's own messages).
async fn route_caps(req: RouteReq) -> Result<RouteRes, RouteError> {
    let connected = auth::is_logged_in();
    let (mut can_mod, mut can_del) = (false, false);
    if connected && (auth::has_scope(SCOPE_BAN) || auth::has_scope(SCOPE_DELETE)) {
        let slug = parse_kick_source(&req.source.channel).unwrap_or_else(|| req.source.channel.clone());
        if is_mod_here(&slug, &req.ctx.user_agent).await {
            can_mod = auth::has_scope(SCOPE_BAN);
            can_del = auth::has_scope(SCOPE_DELETE);
        }
    }
    Ok(RouteRes { status: 200, body: json!({"connected": connected, "canModerate": can_mod, "canDelete": can_del}) })
}

/// A failure the UI toasts verbatim: the body is the plain message, not {error}.
fn action_err(status: u16, msg: &str) -> RouteRes {
    RouteRes { status, body: Value::String(msg.to_string()) }
}

/// Whole seconds -> Kick's timeout minutes: rounded up, 1 minute to 7 days.
pub fn timeout_minutes(seconds: f64) -> i64 {
    ((seconds / 60.0).ceil() as i64).clamp(1, MAX_TIMEOUT_MINUTES)
}

/// Kick message ids are UUIDs; anything else never reaches the URL.
pub fn valid_message_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// `POST/DELETE /moderation/bans` body: a timeout carries `duration` (minutes), a ban does not.
pub fn ban_body(broadcaster: i64, user: i64, duration_minutes: Option<i64>) -> Value {
    let mut b = json!({"broadcaster_user_id": broadcaster, "user_id": user});
    if let Some(d) = duration_minutes {
        b["duration"] = json!(d);
    }
    b
}

/// What to send to Kick for one action.
#[derive(Debug, PartialEq)]
pub struct ActionPlan {
    pub method: &'static str,
    pub url: String,
    pub body: Option<Value>,
    pub scope: &'static str,
}

/// Validates the request fields and builds the Kick call. Err is a 400 message.
pub fn plan_action(action: &str, user_id: &str, message_id: &str, seconds: Option<f64>, broadcaster: i64) -> Result<ActionPlan, String> {
    let user = || user_id.trim().parse::<i64>().map_err(|_| "Kick user id is missing or not a number".to_string());
    let bans = format!("{API_BASE}/moderation/bans");
    match action {
        "timeout" => {
            let s = seconds.filter(|s| s.is_finite() && *s > 0.0).ok_or("A timeout needs a duration")?;
            Ok(ActionPlan { method: "POST", url: bans, body: Some(ban_body(broadcaster, user()?, Some(timeout_minutes(s)))), scope: SCOPE_BAN })
        }
        "ban" => Ok(ActionPlan { method: "POST", url: bans, body: Some(ban_body(broadcaster, user()?, None)), scope: SCOPE_BAN }),
        "unban" => Ok(ActionPlan { method: "DELETE", url: bans, body: Some(ban_body(broadcaster, user()?, None)), scope: SCOPE_BAN }),
        "delete" => {
            if !valid_message_id(message_id) {
                return Err("Kick message id is missing or invalid".into());
            }
            Ok(ActionPlan { method: "DELETE", url: format!("{API_BASE}/chat/{message_id}"), body: None, scope: SCOPE_DELETE })
        }
        _ => Err("unknown action".into()),
    }
}

/// POST /api/kick/action {action: timeout|ban|unban|delete, userId, messageId, seconds}
async fn route_action(req: RouteReq) -> Result<RouteRes, RouteError> {
    if !auth::is_logged_in() {
        return Ok(action_err(401, auth::LOGGED_OUT_REASON));
    }
    let action = js_str_or_empty(Some(&req.body["action"]));
    let user_id = js_str_or_empty(Some(&req.body["userId"]));
    let message_id = js_str_or_empty(Some(&req.body["messageId"]));
    let slug = parse_kick_source(&req.source.channel).unwrap_or_else(|| req.source.channel.clone());
    let broadcaster = match broadcaster_id(&slug, &req.ctx.user_agent).await {
        Ok(id) => id,
        Err(e) => return Ok(action_err(502, &e)),
    };
    let plan = match plan_action(&action, &user_id, &message_id, req.body["seconds"].as_f64(), broadcaster) {
        Ok(p) => p,
        Err(e) => return Ok(action_err(400, &e)),
    };
    if !auth::has_scope(plan.scope) {
        return Ok(action_err(403, "Log in to Kick again to allow moderation"));
    }
    match auth::kick_api(plan.method, &plan.url, plan.body.as_ref()).await {
        Ok((status, _)) if (200..300).contains(&status) => Ok(RouteRes { status: 200, body: json!({"ok": true}) }),
        Ok((status, v)) => Ok(action_err(status, &auth::action_error_message(status, &v))),
        Err(e) => Ok(action_err(e.status, &e.message)),
    }
}

struct St {
    info_present: bool, // `info !== null`
    status: Map<String, Value>,
    backlog: Vec<Value>,
    emotes: Value,
    sub_badges: Vec<SubBadge>,
    ws: Option<(u64, Arc<WsHandle>)>,
    next_ws: u64,
    phase: String, // connecting | open | unavailable | error
    phase_detail: String,
    reconnect_delay: u64,
    reconnect_timer: Timer,
    reconnect_pending: bool,
    status_timer: Timer,
    status_poll_armed: bool,
    watchdog: Timer,
    last_msg_at: i64,
    pinged_at: i64,
    failures: u32,
    stopped: bool,
    last_conn_json: String,
    auth_listener: Option<u64>,
}

pub struct KickSource {
    me: Weak<KickSource>,
    slug: String,
    emit_fn: Emit,
    ctx: Arc<Ctx>,
    client_id: String,
    jobs: Jobs,
    st: Mutex<St>,
}

fn conn_frame(st: &St) -> Value {
    let mut detail = st.phase_detail.clone();
    if st.phase == "open" {
        detail = if truthy(&st.status["live"]) { "Live".into() } else { "Not live (chat is connected)".into() };
    }
    let mut f = json!({"type": "conn", "state": st.phase, "detail": detail, "live": truthy(&st.status["live"])});
    let title = js_str_or_empty(st.status.get("title"));
    if !title.is_empty() {
        f["title"] = json!(title);
    }
    let can_send = auth::is_logged_in();
    f["canSend"] = json!(can_send);
    if !can_send {
        f["readOnlyReason"] = json!(auth::LOGGED_OUT_REASON);
    }
    f
}

fn chatlog_frame(st: &St) -> Value {
    json!({"type": "chatlog", "source": {"userId": st.status["userId"], "slug": st.status["slug"]}, "status": Value::Object(st.status.clone()), "emotes": st.emotes, "messages": st.backlog})
}

impl KickSource {
    pub fn new(slug: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        let mut status = Map::new();
        status.insert("userId".into(), json!(""));
        status.insert("slug".into(), json!(slug));
        status.insert("live".into(), json!(false));
        status.insert("title".into(), json!(""));
        status.insert("category".into(), json!(""));
        status.insert("viewers".into(), json!(0));
        status.insert("url".into(), json!(format!("https://kick.com/{slug}")));
        status.insert("error".into(), Value::Null);
        Arc::new_cyclic(|me| KickSource {
            me: me.clone(),
            slug,
            emit_fn: emit,
            ctx,
            client_id: uuid_v4(),
            jobs: Jobs::new(),
            st: Mutex::new(St {
                info_present: false,
                status,
                backlog: Vec::new(),
                emotes: json!({}),
                sub_badges: Vec::new(),
                ws: None,
                next_ws: 1,
                phase: "connecting".into(),
                phase_detail: "Connecting to Kick...".into(),
                reconnect_delay: RECONNECT_BASE_MS,
                reconnect_timer: Timer::new(),
                reconnect_pending: false,
                status_timer: Timer::new(),
                status_poll_armed: false,
                watchdog: Timer::new(),
                last_msg_at: 0,
                pinged_at: 0,
                failures: 0,
                stopped: false,
                last_conn_json: String::new(),
                auth_listener: None,
            }),
        })
    }

    fn emit(&self, f: Frame) {
        (self.emit_fn)(f);
    }

    fn stopped(&self) -> bool {
        self.st.lock().unwrap().stopped
    }

    /// Emit the conn frame only when it changed.
    fn publish_conn(&self) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st);
            let j = f.to_string();
            if j == st.last_conn_json {
                return;
            }
            st.last_conn_json = j;
            f
        };
        self.emit(f);
    }

    fn set_phase(&self, next: &str, detail: &str) {
        {
            let mut st = self.st.lock().unwrap();
            st.phase = next.into();
            st.phase_detail = detail.into();
        }
        self.publish_conn();
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
            changed.then(|| Value::Object(st.status.clone()))
        };
        if let Some(status) = changed {
            self.emit(json!({"type": "status", "status": status}));
        }
        self.publish_conn();
    }

    /// Put the channel's own artwork on a subscriber badge; everything else keeps its chip.
    fn dress_badges(&self, mut msg: Value) -> Value {
        let subs = self.st.lock().unwrap().sub_badges.clone();
        if subs.is_empty() {
            return msg;
        }
        if let Some(badges) = msg["badges"].as_array_mut() {
            for b in badges.iter_mut() {
                if b["type"] == "subscriber" {
                    b["url"] = json!(sub_badge_for(&subs, &b["count"]));
                }
            }
        }
        msg
    }

    fn push_backlog(&self, msg: Value) {
        let mut st = self.st.lock().unwrap();
        st.backlog.push(msg);
        if st.backlog.len() > MAX_BACKLOG {
            let cut = st.backlog.len() - MAX_BACKLOG;
            st.backlog.drain(..cut);
        }
    }

    // ---------- channel + status ----------

    fn apply_channel(&self, c: &KickChannel) {
        let slug = if c.slug.is_empty() { self.slug.clone() } else { c.slug.clone() };
        if let Ok(id) = c.user_id.parse::<i64>() {
            USER_IDS.lock().unwrap().insert(slug.clone(), id);
        }
        {
            let mut st = self.st.lock().unwrap();
            st.info_present = true;
            if !c.sub_badges.is_empty() || st.sub_badges.is_empty() {
                st.sub_badges = c.sub_badges.clone(); // `c.subBadges || subBadges` (an empty array is truthy)
            }
        }
        self.set_status(vec![
            ("userId", json!(c.user_id)),
            ("slug", json!(slug)),
            ("live", json!(c.live)),
            ("title", json!(c.title)),
            ("category", json!(c.category)),
            ("viewers", c.viewers.clone()),
            ("url", json!(format!("https://kick.com/{slug}"))),
            ("error", Value::Null),
        ]);
    }

    async fn refresh_status(self: Arc<Self>) {
        match get_kick_channel(&self.slug, &self.ctx.user_agent).await {
            Ok(Some(c)) => {
                if !self.stopped() {
                    self.apply_channel(&c);
                }
            }
            Ok(None) => {}
            Err(e) => {
                // Status is decoration; the socket keeps working without it.
                if !self.stopped() {
                    self.ctx.log(&format!("[kick:{}] status refresh failed: {e}", self.slug));
                }
            }
        }
    }

    fn schedule_status_poll(self: &Arc<Self>) {
        let s = self.clone();
        let mut st = self.st.lock().unwrap();
        st.status_poll_armed = true;
        st.status_timer.set(Duration::from_millis(STATUS_POLL_MS), async move {
            s.st.lock().unwrap().status_poll_armed = false;
            let s2 = s.clone();
            s2.refresh_status().await;
            if !s.stopped() {
                s.schedule_status_poll();
            }
        });
    }

    // ---------- connect lifecycle ----------

    async fn connect(self: Arc<Self>) {
        if self.stopped() {
            return;
        }
        if let Err(msg) = self.clone().connect_inner().await {
            if self.stopped() {
                return;
            }
            let failures = {
                let mut st = self.st.lock().unwrap();
                st.failures += 1;
                st.failures
            };
            let msg = if msg.is_empty() { "Kick unreachable".to_string() } else { msg };
            self.ctx.log(&format!("[kick:{}] connect failed ({failures}): {msg}", self.slug));
            self.set_status(vec![("error", json!(msg))]);
            self.set_phase("error", &msg);
            self.schedule_reconnect();
        }
    }

    async fn connect_inner(self: Arc<Self>) -> Result<(), String> {
        let ua = self.ctx.user_agent.clone();
        let c = get_kick_channel(&self.slug, &ua).await?;
        if self.stopped() {
            return Ok(());
        }
        let Some(c) = c else {
            let m = format!("No Kick channel called \"{}\".", self.slug);
            self.set_status(vec![("error", json!(m))]);
            self.set_phase("unavailable", &m);
            self.retry_later(NOT_FOUND_RETRY_MS);
            return Ok(());
        };
        if c.chatroom_id.is_empty() {
            return Err("Kick returned a channel without a chat room".into());
        }
        let first_time = !self.st.lock().unwrap().info_present;
        self.apply_channel(&c);

        if first_time {
            // Backlog and emotes are decoration on a working chat: failures leave them empty.
            let (hist, em) = tokio::join!(fetch_history(&c.channel_id, &ua), fetch_kick_emotes(&self.slug, &ua, false));
            if self.stopped() {
                return Ok(());
            }
            let frame = self.seed_backlog(hist.unwrap_or_default(), em);
            self.emit(frame);
        }

        let desc = match get_realtime_descriptor(&c.channel_id, &ua, &self.client_id).await {
            Ok(d) => {
                *LAST_DESCRIPTOR.lock().unwrap() = Some(d.clone());
                d
            }
            Err(e) => {
                if self.stopped() {
                    return Ok(());
                }
                // A failed descriptor lookup falls back to the last key that worked (or the one observed when this was
                // written) rather than leaving chat dark over a hiccup on a side endpoint.
                self.ctx.log(&format!("[kick:{}] realtime descriptor failed, using last known key: {e}", self.slug));
                LAST_DESCRIPTOR.lock().unwrap().clone().unwrap_or_else(|| (FALLBACK_DESCRIPTOR.0.to_string(), FALLBACK_DESCRIPTOR.1.to_string()))
            }
        };
        if self.stopped() {
            return Ok(());
        }
        self.open_socket(&c, desc);
        Ok(())
    }

    /// The first connect's history + emotes -> the chatlog frame.
    fn seed_backlog(&self, hist: Vec<Value>, emotes: Value) -> Value {
        let mut seen: std::collections::HashSet<String> = self.st.lock().unwrap().backlog.iter().map(|m| js_string(&m["id"])).collect();
        self.st.lock().unwrap().emotes = emotes;
        for raw in hist {
            let m = self.dress_badges(to_kick_chat_msg(&raw));
            note_own_badges(&self.slug, &m);
            let id = js_str_or_empty(Some(&m["id"]));
            if id.is_empty() || !seen.insert(id) {
                continue;
            }
            self.push_backlog(m);
        }
        chatlog_frame(&self.st.lock().unwrap())
    }

    /// A sync fn on purpose: building the timer future inside `connect_inner` would make its type recursive.
    fn retry_later(self: &Arc<Self>, ms: u64) {
        let s = self.clone();
        self.st.lock().unwrap().reconnect_timer.set(Duration::from_millis(ms), async move {
            let s2 = s.clone();
            s.jobs.spawn(s2.connect());
        });
    }

    fn schedule_reconnect(self: &Arc<Self>) {
        let mut st = self.st.lock().unwrap();
        if st.stopped || st.reconnect_pending {
            return;
        }
        // Jitter so sources don't retry in lockstep after a shared network blip.
        let cap = 1000.0f64.min(1.0f64.max(st.reconnect_delay as f64 / 4.0));
        let jitter = (rand::random::<f64>() * cap).floor() as u64;
        let delay = st.reconnect_delay + jitter;
        st.reconnect_pending = true;
        let s = self.clone();
        st.reconnect_timer.set(Duration::from_millis(delay), async move {
            s.st.lock().unwrap().reconnect_pending = false;
            let s2 = s.clone();
            s.jobs.spawn(s2.connect());
        });
        st.reconnect_delay = (st.reconnect_delay * 2).min(RECONNECT_MAX_MS);
    }

    fn open_socket(self: &Arc<Self>, c: &KickChannel, desc: (String, String)) {
        let (app_key, cluster) = desc;
        let url = format!("wss://ws-{cluster}.pusher.com/app/{app_key}?protocol=7&client=js&version={PUSHER_VERSION}&flash=false");
        let room = format!("chatrooms.{}.v2", c.chatroom_id);
        let chan = format!("channel.{}", c.channel_id);
        let mut st = self.st.lock().unwrap();
        if st.stopped {
            return;
        }
        let id = st.next_ws;
        st.next_ws += 1;
        let me = self.me.clone();
        let (room2, chan2) = (room.clone(), chan.clone());
        let handle = ws::connect(url, self.ctx.user_agent.clone(), move |ev| {
            if let Some(s) = me.upgrade() {
                s.on_ws(id, &room2, &chan2, ev);
            }
        });
        st.ws = Some((id, Arc::new(handle)));
        st.last_msg_at = now_ms();
        st.pinged_at = 0;
        let s = self.clone();
        st.watchdog.every(Duration::from_secs(15), move || {
            let s = s.clone();
            async move { s.watchdog_tick(id) }
        });
    }

    fn watchdog_tick(self: &Arc<Self>, id: u64) {
        let (act, quiet) = {
            let mut st = self.st.lock().unwrap();
            if st.ws.as_ref().map(|w| w.0) != Some(id) {
                return;
            }
            let quiet = now_ms() - st.last_msg_at;
            if quiet > DEAD_AFTER_MS {
                (1, quiet)
            } else if quiet > PING_AFTER_MS && st.pinged_at == 0 {
                st.pinged_at = now_ms();
                (2, quiet)
            } else {
                (0, quiet)
            }
        };
        match act {
            1 => {
                self.ctx.log(&format!("[kick:{}] silent for {}s, reconnecting", self.slug, (quiet as f64 / 1000.0).round() as i64));
                let h = self.st.lock().unwrap().ws.as_ref().filter(|w| w.0 == id).map(|w| w.1.clone());
                if let Some(h) = h {
                    h.terminate();
                }
                // terminate() reports nothing here (unlike `ws`, where it emits 'close'): do the close handling.
                self.on_ws_closed(id);
            }
            2 => self.ws_send(id, &json!({"event": "pusher:ping", "data": {}}).to_string()),
            _ => {}
        }
    }

    fn ws_send(&self, id: u64, text: &str) {
        let h = self.st.lock().unwrap().ws.as_ref().filter(|w| w.0 == id).map(|w| w.1.clone());
        if let Some(h) = h {
            h.send(text);
        }
    }

    fn on_ws(self: &Arc<Self>, id: u64, room: &str, chan: &str, ev: WsEvent) {
        match ev {
            WsEvent::Open => {}
            WsEvent::Message(raw) => {
                {
                    let mut st = self.st.lock().unwrap();
                    if st.ws.as_ref().map(|w| w.0) != Some(id) || st.stopped {
                        return;
                    }
                    st.last_msg_at = now_ms();
                    st.pinged_at = 0;
                }
                if let Ok(m) = serde_json::from_str::<Value>(&raw) {
                    self.handle_pusher(id, &m, room, chan);
                }
            }
            WsEvent::Close(..) => self.on_ws_closed(id),
            WsEvent::Error(e) => {
                // 'error' is always followed by 'close' in `ws`; here it ends the socket, so it is handled as the close too.
                if !self.stopped() {
                    self.ctx.log(&format!("[kick:{}] socket error: {e}", self.slug));
                }
                self.on_ws_closed(id);
            }
        }
    }

    fn on_ws_closed(self: &Arc<Self>, id: u64) {
        {
            let mut st = self.st.lock().unwrap();
            if st.ws.as_ref().map(|w| w.0) != Some(id) {
                return;
            }
            st.ws = None;
            st.watchdog.clear();
            if st.stopped {
                return;
            }
        }
        self.set_phase("connecting", "Reconnecting to Kick chat...");
        self.schedule_reconnect();
    }

    fn handle_pusher(self: &Arc<Self>, id: u64, m: &Value, room: &str, chan: &str) {
        let Some(ev) = m["event"].as_str().filter(|e| !e.is_empty()) else { return };
        if ev == "pusher:connection_established" {
            self.ws_send(id, &json!({"event": "pusher:subscribe", "data": {"auth": "", "channel": room}}).to_string());
            self.ws_send(id, &json!({"event": "pusher:subscribe", "data": {"auth": "", "channel": chan}}).to_string());
            return;
        }
        if ev == "pusher:ping" {
            self.ws_send(id, &json!({"event": "pusher:pong", "data": {}}).to_string());
            return;
        }
        if ev == "pusher_internal:subscription_succeeded" {
            if m["channel"] == room {
                let arm = {
                    let mut st = self.st.lock().unwrap();
                    st.failures = 0;
                    st.reconnect_delay = RECONNECT_BASE_MS;
                    !st.status_poll_armed
                };
                self.set_phase("open", "");
                if arm {
                    self.schedule_status_poll();
                }
            }
            return;
        }
        if ev == "pusher:error" {
            let d = match &m["data"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            self.ctx.log(&format!("[kick:{}] pusher error: {d}", self.slug));
            return;
        }

        let data = match &m["data"] {
            Value::String(s) => match serde_json::from_str::<Value>(s) {
                Ok(v) => v,
                Err(_) => return,
            },
            other => other.clone(),
        };
        if m["channel"] == chan {
            // Livestream start/stop events: one authoritative read beats trusting a partial payload.
            let l = ev.to_lowercase();
            if l.contains("stream") || l.contains("live") {
                let s = self.clone();
                self.jobs.spawn(async move { s.refresh_status().await });
            }
            return;
        }
        if m["channel"] != room || !truthy(&data) {
            return;
        }

        match ev {
            "App\\Events\\ChatMessageEvent" => {
                let msg = self.dress_badges(to_kick_chat_msg(&data));
                if js_str_or_empty(Some(&msg["id"])).is_empty() {
                    return;
                }
                note_own_badges(&self.slug, &msg);
                self.push_backlog(msg.clone());
                self.emit(json!({"type": "chat", "message": msg}));
            }
            "App\\Events\\MessageDeletedEvent" => {
                let inner = &data["message"]["id"];
                let pick = if truthy(inner) { inner } else { &data["id"] };
                let mid = js_str_or_empty(Some(pick));
                if mid.is_empty() {
                    return;
                }
                for msg in self.st.lock().unwrap().backlog.iter_mut() {
                    if msg["id"] == mid.as_str() {
                        msg["deleted"] = json!(true);
                    }
                }
                self.emit(json!({"type": "delete", "ids": [mid]}));
            }
            "App\\Events\\UserBannedEvent" => {
                let user = if truthy(&data["user"]) { data["user"].clone() } else { json!({}) };
                let user_id = if user["id"].is_null() { String::new() } else { js_string(&user["id"]) };
                if user_id.is_empty() {
                    return;
                }
                // Kick, like Twitch, retroactively clears a banned user's messages from view.
                for msg in self.st.lock().unwrap().backlog.iter_mut() {
                    if msg["user"]["id"] == user_id.as_str() {
                        msg["deleted"] = json!(true);
                    }
                }
                let expires_at = if truthy(&data["expires_at"]) { data["expires_at"].as_str().and_then(parse_rfc3339_ms).unwrap_or(0) } else { 0 };
                self.emit(json!({
                    "type": "banned",
                    "userId": user_id,
                    "username": js_str_or_empty(user.get("username")),
                    "by": js_str_or_empty(data["banned_by"].get("username")),
                    "reason": "",
                    // No expiry (or an explicit permanent flag) is a permanent ban; a real one is a timeout.
                    "permanent": truthy(&data["permanent"]) || expires_at == 0,
                    "expiresAt": expires_at,
                }));
            }
            "App\\Events\\ChatroomClearEvent" => {
                for msg in self.st.lock().unwrap().backlog.iter_mut() {
                    msg["deleted"] = json!(true);
                }
                self.emit(json!({"type": "clear"}));
            }
            "App\\Events\\PinnedMessageCreatedEvent" => {
                let message = if truthy(&data["message"]) { self.dress_badges(to_kick_chat_msg(&data["message"])) } else { Value::Null };
                self.emit(json!({"type": "pin", "message": message}));
            }
            "App\\Events\\PinnedMessageDeletedEvent" => self.emit(json!({"type": "pin", "message": null})),
            // Kick ships new event types (polls, gifts, rewards) without warning; ignoring unknown ones is deliberate.
            _ => {}
        }
    }
}

impl Source for KickSource {
    fn start(self: Arc<Self>) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st);
            st.last_conn_json = f.to_string();
            f
        };
        self.emit(f);
        // canSend / readOnlyReason follow the login
        let me = self.me.clone();
        let id = auth::EVENTS.on(Arc::new(move |_| {
            if let Some(s) = me.upgrade() {
                s.publish_conn();
            }
        }));
        self.st.lock().unwrap().auth_listener = Some(id);
        let s = self.clone();
        self.jobs.spawn(s.connect());
    }

    fn stop(&self) {
        let ws = {
            let mut st = self.st.lock().unwrap();
            st.stopped = true;
            st.reconnect_timer.clear();
            st.status_timer.clear();
            st.watchdog.clear();
            if let Some(id) = st.auth_listener.take() {
                auth::EVENTS.off(id);
            }
            st.ws.take()
        };
        self.jobs.abort_all();
        if let Some((_, h)) = ws {
            h.terminate();
        }
    }

    fn snapshot(&self) -> Vec<Frame> {
        let st = self.st.lock().unwrap();
        vec![chatlog_frame(&st), conn_frame(&st)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_round_up_to_whole_minutes_within_kicks_limits() {
        assert_eq!(timeout_minutes(1.0), 1);
        assert_eq!(timeout_minutes(60.0), 1);
        assert_eq!(timeout_minutes(61.0), 2);
        assert_eq!(timeout_minutes(600.0), 10);
        assert_eq!(timeout_minutes(1e9), 10_080);
    }

    #[test]
    fn message_ids_are_uuid_like() {
        assert!(valid_message_id("0b4e8a2c-1d3f-4a5b-9c6d-7e8f90a1b2c3"));
        assert!(!valid_message_id(""));
        assert!(!valid_message_id("../moderation/bans"));
        assert!(!valid_message_id("a b"));
        assert!(!valid_message_id("a?x=1"));
    }

    #[test]
    fn ban_bodies_have_the_documented_shape() {
        assert_eq!(ban_body(5, 9, Some(10)), json!({"broadcaster_user_id": 5, "user_id": 9, "duration": 10}));
        assert_eq!(ban_body(5, 9, None), json!({"broadcaster_user_id": 5, "user_id": 9}));
    }

    #[test]
    fn actions_become_kick_calls() {
        let t = plan_action("timeout", "9", "m", Some(300.0), 5).unwrap();
        assert_eq!((t.method, t.scope), ("POST", SCOPE_BAN));
        assert!(t.url.ends_with("/public/v1/moderation/bans"));
        assert_eq!(t.body, Some(json!({"broadcaster_user_id": 5, "user_id": 9, "duration": 5})));
        let b = plan_action("ban", "9", "", None, 5).unwrap();
        assert_eq!((b.method, b.body), ("POST", Some(json!({"broadcaster_user_id": 5, "user_id": 9}))));
        let u = plan_action("unban", "9", "", None, 5).unwrap();
        assert_eq!((u.method, u.scope), ("DELETE", SCOPE_BAN));
        let d = plan_action("delete", "9", "ab-12", None, 5).unwrap();
        assert_eq!((d.method, d.scope, d.body), ("DELETE", SCOPE_DELETE, None));
        assert!(d.url.ends_with("/public/v1/chat/ab-12"));
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(plan_action("timeout", "9", "", None, 5).is_err());
        assert!(plan_action("timeout", "9", "", Some(0.0), 5).is_err());
        assert!(plan_action("ban", "nine", "", None, 5).is_err());
        assert!(plan_action("ban", "", "", None, 5).is_err());
        assert!(plan_action("delete", "9", "x/../y", None, 5).is_err());
        assert!(plan_action("delete", "9", "", None, 5).is_err());
        assert!(plan_action("pin", "9", "m", None, 5).is_err());
    }

    #[test]
    fn mod_status_reads_roles_and_badges() {
        assert!(channel_user_is_mod(&json!({"is_moderator": true})));
        assert!(channel_user_is_mod(&json!({"is_channel_owner": true})));
        assert!(channel_user_is_mod(&json!({"badges": [{"type": "subscriber"}, {"type": "moderator"}]})));
        assert!(!channel_user_is_mod(&json!({"is_moderator": false, "badges": [{"type": "vip"}]})));
        assert!(!channel_user_is_mod(&json!({})));
        assert!(badges_mark_mod(&json!([{"type": "broadcaster"}])));
        assert!(!badges_mark_mod(&Value::Null));
    }
}
