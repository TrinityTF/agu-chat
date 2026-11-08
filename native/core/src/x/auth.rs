//! X account: the user's own x.com session, used to look chatter names up and to send chat. Nothing here touches
//! the Agu Chat website and nothing is paid: these are the same web endpoints x.com's own client calls, with the
//! session cookies of the account that logged in.
//!
//! LOGIN. The app (gpui crate) opens a sign-in window that hosts x.com's own login page and hands back the
//! `auth_token`, `ct0` and `twid` cookies through the login hook (`set_login_hook`); the cookies travel in memory
//! only. They are verified with one cheap call before anything is stored, so a failed login never leaves a record.
//!
//! STORAGE. The session lives in the encrypted secrets store (key `x`, like Kick's `kick`) and is never logged.
//! Dev safety: env `AGU_AUTH_READONLY=1` never writes (or deletes) the stored secret, so a test run cannot log
//! the real install out or overwrite its session.
//!
//! EXPIRY. x.com answers 401/403 to a session it no longer accepts. `mark_expired` forgets the session and the
//! account state says so, which makes every X column say "log in again".
//!
//! Moderation is not built. x.com's web client has `live_chat_remove_chat_message_gql` (delete a message) and
//! `live_chat_mute_user_chat_gql` (mute a chatter) mutations behind the same headers as `sendLiveChatMutation`;
//! they belong in this module next to `send_chat` when moderation for X is wanted.
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::AbortHandle;

use super::ua::browser_user_agent;
use super::vendor::x_live::WEB_BEARER;
use crate::net::{self, FetchOpts};
use crate::platforms::Ctx;
use crate::protocol::{AccountState, AccountUser, DevicePrompt};
use crate::util::{now_ms, Listeners};

const SECRET_KEY: &str = "x";
const VERIFY_URL: &str = "https://x.com/i/api/1.1/account/verify_credentials.json";
const SEND_URL: &str = "https://x.com/i/api/graphql/igfzSylm2VIW-ooc4Eh7nA/sendLiveChatMutation";
const SEND_QUERY_ID: &str = "igfzSylm2VIW-ooc4Eh7nA";
/// The sign-in window has this long.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub const EXPIRED_MESSAGE: &str = "X login expired, log in again";
/// What a send box says on an X column without a login.
pub const LOGGED_OUT_REASON: &str = "Log in to X in Settings to chat";
/// What the column's status says without a login (no chat is shown then).
pub const LOGGED_OUT_DETAIL: &str = "Log in to X in Settings to see X chat";

/// "change": login / logout / expiry
pub static EVENTS: Listeners<()> = Listeners::new();

fn read_only() -> bool {
    std::env::var("AGU_AUTH_READONLY").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------------------

/// The cookies the sign-in window found. Secrets: never printed.
#[derive(Clone, Default, PartialEq)]
pub struct Cookies {
    pub auth_token: String,
    pub ct0: String,
    /// `u%3D<user id>`
    pub twid: String,
}

impl std::fmt::Debug for Cookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cookies(..)")
    }
}

/// The stored login (the JSON stored under `x` in secrets.bin).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub auth_token: String,
    pub ct0: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Record(@{})", self.username)
    }
}

/// Who a session belongs to, from x.com.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub id: String,
    pub username: String,
    pub name: String,
}

/// A failed call: `status` is X's HTTP status, 0 when it never answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    /// x.com no longer accepts the session.
    pub fn is_auth(&self) -> bool {
        self.status == 401 || self.status == 403
    }
}

/// The headers x.com's web client sends to `x.com/i/api` with a session. `post` adds the JSON content type.
pub fn session_headers(auth_token: &str, ct0: &str, post: bool) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("authorization", format!("Bearer {WEB_BEARER}")),
        ("x-csrf-token", ct0.to_string()),
        ("cookie", format!("auth_token={auth_token}; ct0={ct0}")),
        ("x-twitter-auth-type", "OAuth2Session".to_string()),
        ("x-twitter-active-user", "yes".to_string()),
        ("x-twitter-client-language", "en".to_string()),
        ("user-agent", browser_user_agent().to_string()),
        ("accept", "*/*".to_string()),
    ];
    if post {
        h.push(("content-type", "application/json".to_string()));
    }
    h
}

/// The user id inside a `twid` cookie value (`u%3D123`, `u=123`, possibly quoted).
pub fn user_id_from_twid(twid: &str) -> Option<String> {
    let t = twid.trim().trim_matches('"');
    let t = t.strip_prefix("u%3D").or_else(|| t.strip_prefix("u%3d")).or_else(|| t.strip_prefix("u="))?;
    (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit())).then(|| t.to_string())
}

/// The first error message in an X answer (`{"errors":[{"message":..}]}`, also a GraphQL `errors`).
pub fn error_message(body: &Value) -> Option<String> {
    let first = body["errors"].as_array()?.iter().find_map(|e| e["message"].as_str().filter(|m| !m.is_empty()))?;
    Some(first.to_string())
}

/// `account/verify_credentials.json` answer -> who the session is.
pub fn parse_verify(status: u16, body: &Value) -> Result<Identity, ApiError> {
    if !(200..300).contains(&status) {
        let message = match status {
            401 | 403 => "X did not accept that login".to_string(),
            _ => error_message(body).unwrap_or_else(|| format!("X answered {status} while checking the login")),
        };
        return Err(ApiError { status, message });
    }
    let username = body["screen_name"].as_str().unwrap_or("").trim().to_string();
    let id = body["id_str"].as_str().map(String::from).or_else(|| body["id"].as_u64().map(|n| n.to_string())).unwrap_or_default();
    if username.is_empty() {
        return Err(ApiError { status, message: "X did not say who this login belongs to".into() });
    }
    Ok(Identity { id, username, name: body["name"].as_str().unwrap_or("").to_string() })
}

/// The body of `sendLiveChatMutation`.
pub fn build_send_body(broadcast_id: &str, text: &str, session_id: u32, reply_to: Option<&str>) -> Value {
    let mut input = json!({"broadcast_id": broadcast_id, "chat_type": 1, "message": text, "session_id": session_id});
    if let Some(r) = reply_to.filter(|r| !r.is_empty()) {
        input["reply_to"] = json!(r);
    }
    json!({"variables": {"input": input}, "queryId": SEND_QUERY_ID})
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendError {
    pub status: u16,
    pub message: String,
}

/// `sendLiveChatMutation` answer -> the sent message's timestamp, or X's own error text.
pub fn parse_send_response(status: u16, body: &Value) -> Result<String, SendError> {
    if let Some(m) = error_message(body) {
        return Err(SendError { status: if (200..300).contains(&status) { 422 } else { status }, message: m });
    }
    if !(200..300).contains(&status) {
        return Err(SendError { status, message: if status == 401 { EXPIRED_MESSAGE.into() } else { format!("X rejected the message ({status})") } });
    }
    let ts = &body["data"]["live_chat_send_chat_gql"]["timestamp"];
    match ts {
        Value::String(s) if !s.is_empty() => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        _ => Err(SendError { status: 422, message: "X did not confirm the message".into() }),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Login hook: the app provides the sign-in window
// ---------------------------------------------------------------------------------------------------------

/// What opening the sign-in window resolves to: the cookies, or an error. An empty error is "the window was
/// closed", which is a cancel and not worth a message.
pub type LoginFuture = BoxFuture<'static, Result<Cookies, String>>;

#[derive(Clone)]
pub struct LoginHook {
    /// Open the window; the future resolves when it found the cookies or was closed.
    pub open: Arc<dyn Fn() -> LoginFuture + Send + Sync>,
    /// Close the window (a cancel from Settings).
    pub close: Arc<dyn Fn() + Send + Sync>,
}

static HOOK: Mutex<Option<LoginHook>> = Mutex::new(None);

pub fn set_login_hook(h: LoginHook) {
    *HOOK.lock().unwrap() = Some(h);
}

// ---------------------------------------------------------------------------------------------------------
// Stored login + state
// ---------------------------------------------------------------------------------------------------------

struct Flow {
    id: u64,
    expires_at: i64,
    task: Option<AbortHandle>,
}

#[derive(Default)]
struct State {
    ctx: Option<Arc<Ctx>>,
    record: Option<Arc<Record>>,
    pending: Option<Flow>,
    last_error: Option<String>,
    expired: bool,
    seq: u64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn log(msg: &str) {
    let ctx = st().ctx.clone();
    if let Some(c) = ctx {
        c.log(&format!("[x] {msg}"));
    }
}

fn changed() {
    EVENTS.emit(&());
    let ctx = st().ctx.clone();
    if let Some(ctx) = ctx {
        ctx.accounts_changed();
    }
}

fn persist() {
    if read_only() {
        return;
    }
    let (ctx, rec) = {
        let g = st();
        (g.ctx.clone(), g.record.clone())
    };
    let Some(ctx) = ctx else { return };
    let res = match rec {
        Some(r) => ctx.secrets.set(SECRET_KEY, serde_json::to_value(&*r).unwrap_or(Value::Null)),
        None => ctx.secrets.delete(SECRET_KEY),
    };
    if let Err(e) = res {
        log(&format!("could not store the X login: {e}"));
    }
}

/// Called once at startup (inside the tokio runtime). Does not block on the network.
pub fn init(ctx: Arc<Ctx>) {
    st().ctx = Some(ctx.clone());
    if let Some(v) = ctx.secrets.get(SECRET_KEY) {
        match serde_json::from_value::<Record>(v) {
            Ok(rec) if !rec.auth_token.is_empty() && !rec.ct0.is_empty() => st().record = Some(Arc::new(rec)),
            Ok(_) => {}
            Err(_) => log("could not read the stored X login"),
        }
    }
}

/// For a source created before init() (tests).
pub fn ensure_ctx(ctx: &Arc<Ctx>) {
    let mut g = st();
    if g.ctx.is_none() {
        g.ctx = Some(ctx.clone());
    }
}

pub fn is_logged_in() -> bool {
    st().record.is_some()
}

/// The session expired (x.com refused it) and nobody logged in again yet.
pub fn is_expired() -> bool {
    let g = st();
    g.expired && g.record.is_none()
}

/// The column status when there is no login: the plain ask, or the expiry.
pub fn logged_out_detail() -> &'static str {
    if is_expired() { EXPIRED_MESSAGE } else { LOGGED_OUT_DETAIL }
}

pub fn me() -> Option<Identity> {
    st().record.as_ref().map(|r| Identity { id: r.user_id.clone(), username: r.username.clone(), name: r.display_name.clone() })
}

/// The cookies of the logged-in session, for a request.
pub(crate) fn session() -> Option<Arc<Record>> {
    st().record.clone()
}

pub fn state() -> AccountState {
    let g = st();
    let mut out = AccountState { connected: g.record.is_some(), ..Default::default() };
    if let Some(r) = &g.record {
        out.user = Some(AccountUser { id: r.user_id.clone(), login: r.username.clone(), name: format!("@{}", r.username) });
    }
    if let Some(p) = &g.pending {
        out.pending = Some(DevicePrompt { user_code: String::new(), verification_uri: String::new(), expires_at: p.expires_at });
    }
    out.error = g.last_error.clone();
    out
}

/// x.com refused the session: forget it. The account state and every X column say to log in again.
pub fn mark_expired() {
    {
        let mut g = st();
        if g.record.is_none() {
            return;
        }
        g.record = None;
        g.expired = true;
        g.last_error = Some(EXPIRED_MESSAGE.into());
    }
    persist();
    changed();
}

// ---------- login ----------

/// Opens the sign-in window and returns at once; the account state follows (pending -> connected | error).
pub async fn login() -> Result<Value, String> {
    if st().ctx.is_none() {
        return Err("X is not initialised.".into());
    }
    let Some(hook) = HOOK.lock().unwrap().clone() else { return Err("X login is not available in this build.".into()) };
    cancel();
    let id = {
        let mut g = st();
        g.last_error = None;
        g.seq += 1;
        g.seq
    };
    let (open, close) = (hook.open.clone(), hook.close.clone());
    let task = tokio::spawn(async move {
        let r = match tokio::time::timeout(LOGIN_TIMEOUT, open()).await {
            Ok(r) => r,
            Err(_) => {
                close();
                Err("The X login window timed out, try again.".into())
            }
        };
        let r = match r {
            Ok(c) => verify_cookies(c).await,
            Err(e) => Err(e),
        };
        finish(id, r);
    });
    st().pending = Some(Flow { id, expires_at: now_ms() + LOGIN_TIMEOUT.as_millis() as i64, task: Some(task.abort_handle()) });
    changed();
    Ok(json!({"pending": true}))
}

/// Check the cookies with one call and build the record. Nothing is stored here.
async fn verify_cookies(c: Cookies) -> Result<Record, String> {
    if c.auth_token.is_empty() || c.ct0.is_empty() {
        return Err("The X login did not finish, try again.".into());
    }
    let who = match fetch_identity(&c.auth_token, &c.ct0).await {
        Ok(i) => i,
        Err(e) => {
            // verify_credentials is the cheap check; the chatter lookup of the user's own id is the second opinion.
            let by_lookup = match (e.is_auth(), user_id_from_twid(&c.twid)) {
                (false, Some(id)) => super::names::lookup_with(&c.auth_token, &c.ct0, &[id]).await.ok().and_then(|mut v| v.pop()),
                _ => None,
            };
            match by_lookup {
                Some(u) => Identity { id: u.id, username: u.username, name: u.name },
                None => return Err(format!("{} (X login not saved)", e.message)),
            }
        }
    };
    let user_id = if who.id.is_empty() { user_id_from_twid(&c.twid).unwrap_or_default() } else { who.id };
    Ok(Record { auth_token: c.auth_token, ct0: c.ct0, user_id, username: who.username.clone(), display_name: if who.name.is_empty() { who.username } else { who.name } })
}

fn finish(id: u64, r: Result<Record, String>) {
    {
        let mut g = st();
        if g.pending.as_ref().map(|f| f.id) != Some(id) {
            return; // cancelled or replaced
        }
        g.pending = None;
        match r {
            Ok(rec) => {
                g.record = Some(Arc::new(rec));
                g.last_error = None;
                g.expired = false;
            }
            // an empty message is the window being closed: a cancel
            Err(e) if e.is_empty() => {}
            Err(e) => g.last_error = Some(e),
        }
    }
    persist();
    changed();
}

pub fn cancel() {
    let close = {
        let mut g = st();
        match g.pending.take() {
            Some(f) => {
                if let Some(t) = f.task {
                    t.abort();
                }
            }
            None => return,
        }
        HOOK.lock().unwrap().as_ref().map(|h| h.close.clone())
    };
    if let Some(c) = close {
        c();
    }
    changed();
}

pub async fn logout() {
    cancel();
    {
        let mut g = st();
        g.record = None;
        g.last_error = None;
        g.expired = false;
    }
    persist();
    changed();
}

// ---------------------------------------------------------------------------------------------------------
// Calls with the session
// ---------------------------------------------------------------------------------------------------------

/// One request to x.com with the session headers. Returns the status and the JSON body (null if it is not JSON).
pub(crate) async fn call(method: &'static str, url: &str, auth_token: &str, ct0: &str, body: Option<&Value>, timeout_ms: u64) -> Result<(u16, Value), ApiError> {
    let mut o = FetchOpts::new(timeout_ms).method(method);
    for (k, v) in session_headers(auth_token, ct0, body.is_some()) {
        o = o.header(k, &v);
    }
    if let Some(b) = body {
        o = o.body(b.to_string());
    }
    let r = net::fetch(url, o).await.map_err(|e| ApiError { status: 0, message: if e.timed_out { "X did not answer".into() } else { "Could not reach X".into() } })?;
    let status = r.status().as_u16();
    let v = net::json(r).await.unwrap_or(Value::Null);
    Ok((status, v))
}

async fn fetch_identity(auth_token: &str, ct0: &str) -> Result<Identity, ApiError> {
    let (status, v) = call("GET", VERIFY_URL, auth_token, ct0, None, 15_000).await?;
    parse_verify(status, &v)
}

/// One random id per app run, the `session_id` x.com's chat client sends.
fn session_id() -> u32 {
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(rand::random)
}

/// Post one chat message to a broadcast as the logged-in user. X's own error text comes back verbatim.
pub async fn send_chat(broadcast_id: &str, text: &str, reply_to: Option<&str>) -> Result<(), SendError> {
    let Some(rec) = session() else { return Err(SendError { status: 401, message: LOGGED_OUT_REASON.into() }) };
    let body = build_send_body(broadcast_id, text, session_id(), reply_to);
    let (status, v) = call("POST", SEND_URL, &rec.auth_token, &rec.ct0, Some(&body), 15_000).await.map_err(|e| SendError { status: 502, message: e.message })?;
    if status == 401 {
        mark_expired();
    }
    parse_send_response(status, &v).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(h: &'a [(&'static str, String)], k: &str) -> Option<&'a str> {
        h.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn headers_carry_the_session() {
        let h = session_headers("AT", "CT", false);
        assert_eq!(header(&h, "x-csrf-token"), Some("CT"));
        assert_eq!(header(&h, "cookie"), Some("auth_token=AT; ct0=CT"));
        assert_eq!(header(&h, "x-twitter-auth-type"), Some("OAuth2Session"));
        assert_eq!(header(&h, "x-twitter-active-user"), Some("yes"));
        assert_eq!(header(&h, "x-twitter-client-language"), Some("en"));
        assert!(header(&h, "authorization").unwrap().starts_with("Bearer AAAA"));
        assert!(header(&h, "user-agent").unwrap().starts_with("Mozilla/5.0"));
        assert_eq!(header(&h, "content-type"), None);
        let p = session_headers("AT", "CT", true);
        assert_eq!(header(&p, "content-type"), Some("application/json"));
    }

    #[test]
    fn twid_forms() {
        assert_eq!(user_id_from_twid("u%3D12345"), Some("12345".into()));
        assert_eq!(user_id_from_twid("\"u%3D12345\""), Some("12345".into()));
        assert_eq!(user_id_from_twid("u=12345"), Some("12345".into()));
        assert_eq!(user_id_from_twid("u%3D"), None);
        assert_eq!(user_id_from_twid("u%3Dabc"), None);
        assert_eq!(user_id_from_twid(""), None);
    }

    #[test]
    fn verify_answers() {
        let ok = parse_verify(200, &json!({"id_str": "42", "screen_name": "Agu", "name": "Agu!"})).unwrap();
        assert_eq!(ok, Identity { id: "42".into(), username: "Agu".into(), name: "Agu!".into() });
        assert_eq!(parse_verify(200, &json!({"id": 7, "screen_name": "a"})).unwrap().id, "7");
        assert!(parse_verify(200, &json!({"id_str": "42"})).is_err());
        let e = parse_verify(401, &json!({"errors": [{"code": 32, "message": "Could not authenticate you."}]})).unwrap_err();
        assert!(e.is_auth());
        assert_eq!(e.message, "X did not accept that login");
        let e = parse_verify(429, &json!({"errors": [{"message": "Rate limit exceeded"}]})).unwrap_err();
        assert!(!e.is_auth());
        assert_eq!(e.message, "Rate limit exceeded");
        assert_eq!(parse_verify(500, &Value::Null).unwrap_err().message, "X answered 500 while checking the login");
    }

    #[test]
    fn send_body_shape() {
        let b = build_send_body("1DGleVdEapOJL", "hi there", 123456, None);
        assert_eq!(b, json!({"variables": {"input": {"broadcast_id": "1DGleVdEapOJL", "chat_type": 1, "message": "hi there", "session_id": 123456}}, "queryId": "igfzSylm2VIW-ooc4Eh7nA"}));
        let r = build_send_body("b", "m", 1, Some("1794052800123456789"));
        assert_eq!(r["variables"]["input"]["reply_to"], "1794052800123456789");
        assert!(build_send_body("b", "m", 1, Some("")).pointer("/variables/input/reply_to").is_none());
    }

    #[test]
    fn send_answers() {
        let ok = json!({"data": {"live_chat_send_chat_gql": {"timestamp": "1794052800123456789"}}});
        assert_eq!(parse_send_response(200, &ok), Ok("1794052800123456789".into()));
        let num = json!({"data": {"live_chat_send_chat_gql": {"timestamp": 1794052800123456u64}}});
        assert_eq!(parse_send_response(200, &num), Ok("1794052800123456".into()));
        // GraphQL errors arrive with a 200 and X's own text is passed on
        let gql = json!({"errors": [{"message": "You are not allowed to chat here"}], "data": null});
        assert_eq!(parse_send_response(200, &gql), Err(SendError { status: 422, message: "You are not allowed to chat here".into() }));
        assert_eq!(parse_send_response(403, &json!({"errors": [{"code": 226, "message": "automated request"}]})), Err(SendError { status: 403, message: "automated request".into() }));
        assert_eq!(parse_send_response(401, &Value::Null), Err(SendError { status: 401, message: EXPIRED_MESSAGE.into() }));
        assert_eq!(parse_send_response(500, &Value::Null), Err(SendError { status: 500, message: "X rejected the message (500)".into() }));
        assert_eq!(parse_send_response(200, &json!({"data": {}})), Err(SendError { status: 422, message: "X did not confirm the message".into() }));
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let c = Cookies { auth_token: "SECRET".into(), ct0: "SECRET2".into(), twid: "u%3D1".into() };
        assert!(!format!("{c:?}").contains("SECRET"));
        let r = Record { auth_token: "SECRET".into(), ct0: "SECRET2".into(), user_id: "1".into(), username: "agu".into(), display_name: "Agu".into() };
        assert!(!format!("{r:?}").contains("SECRET"));
    }
}
