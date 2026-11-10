//! YouTube account: the user's own youtube.com session, used to send chat and to moderate. Nothing here touches
//! the Agu Chat website and nothing is paid: these are the web endpoints youtube.com's own client calls, with the
//! session cookies of the account that logged in. Reading chat stays anonymous (mod.rs) and never uses them.
//!
//! LOGIN. The app (gpui crate) opens a sign-in window that hosts Google's own login page and hands back the
//! youtube.com cookies through the login hook (`set_login_hook`); they travel in memory only. They are verified
//! with one cheap call (`account/account_menu`) before anything is stored, so a failed login never leaves a record.
//!
//! STORAGE. The session lives in the encrypted secrets store (key `youtube`, like X's `x`) and is never logged.
//! Dev safety: env `AGU_AUTH_READONLY=1` never writes (or deletes) the stored secret.
//!
//! SIGNING. Every request carries the cookies plus `Authorization: SAPISIDHASH ...` (`sapisid_hash`). Requests go
//! out with the app's honest User-Agent, like the reading code.
//!
//! EXPIRY. YouTube answers 401/403 to a session it no longer accepts. `mark_expired` forgets the session and the
//! account state says so.
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use tokio::task::AbortHandle;

use crate::net::{self, FetchOpts};
use crate::platforms::Ctx;
use crate::protocol::{AccountState, AccountUser, DevicePrompt};
use crate::util::{now_ms, uuid_v4, Listeners};

const SECRET_KEY: &str = "youtube";
const ORIGIN: &str = "https://www.youtube.com";
const ACCOUNT_MENU_URL: &str = "https://www.youtube.com/youtubei/v1/account/account_menu?prettyPrint=false";
const SEND_URL: &str = "https://www.youtube.com/youtubei/v1/live_chat/send_message?prettyPrint=false";
/// Used until a watch page has told us the client version the site runs.
const FALLBACK_CLIENT_VERSION: &str = "2.20260101.00.00";
/// The sign-in window has this long.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
pub const EXPIRED_MESSAGE: &str = "YouTube signed you out - log in again in Settings";
/// What a send box says on a YouTube column without a login.
pub const LOGGED_OUT_REASON: &str = "Log in to YouTube in Settings to chat";

/// The cookies kept from the sign-in window's profile; everything else is dropped.
pub const KEPT: [&str; 15] = [
    "SID", "HSID", "SSID", "APISID", "SAPISID", "__Secure-1PSID", "__Secure-3PSID", "__Secure-1PAPISID", "__Secure-3PAPISID", "__Secure-1PSIDTS", "__Secure-3PSIDTS", "SIDCC", "__Secure-1PSIDCC",
    "__Secure-3PSIDCC", "LOGIN_INFO",
];

/// "change": login / logout / expiry
pub static EVENTS: Listeners<()> = Listeners::new();

fn read_only() -> bool {
    std::env::var("AGU_AUTH_READONLY").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------------------

/// The cookies the sign-in window found (name, value). Secrets: never printed.
#[derive(Clone, Default, PartialEq)]
pub struct Cookies {
    pub pairs: Vec<(String, String)>,
}

impl std::fmt::Debug for Cookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cookies(..)")
    }
}

/// The stored login (the JSON stored under `youtube` in secrets.bin).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub cookies: BTreeMap<String, String>,
    #[serde(default)]
    pub channel_id: String,
    /// without the leading `@`
    #[serde(default)]
    pub handle: String,
    #[serde(default)]
    pub display_name: String,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Record(@{})", self.handle)
    }
}

/// Who a session belongs to, from youtube.com.
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub channel_id: String,
    pub handle: String,
    pub name: String,
}

/// A failed call: `status` is YouTube's HTTP status, 0 when it never answered.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

impl ApiError {
    /// youtube.com no longer accepts the session.
    pub fn is_auth(&self) -> bool {
        self.status == 401 || self.status == 403
    }
}

/// Only the kept cookies, first non-empty value of each name.
pub fn keep_cookies(pairs: &[(String, String)]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (k, v) in pairs {
        if KEPT.contains(&k.as_str()) && !v.is_empty() && !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    out
}

/// SAPISID (or its `__Secure-3PAPISID` twin) is what the request signature is built from.
fn sapisid_of(c: &BTreeMap<String, String>) -> Option<&str> {
    c.get("SAPISID").or_else(|| c.get("__Secure-3PAPISID")).map(String::as_str)
}

/// The sign-in is done when the signature secret and `LOGIN_INFO` are both there.
pub fn login_complete(pairs: &[(String, String)]) -> bool {
    let c = keep_cookies(pairs);
    sapisid_of(&c).is_some() && c.contains_key("LOGIN_INFO")
}

/// `Authorization` value: `SAPISIDHASH {ts}_{sha1hex("{ts} {sapisid} https://www.youtube.com")}`.
pub fn sapisid_hash(ts: u64, sapisid: &str) -> String {
    let digest = Sha1::digest(format!("{ts} {sapisid} {ORIGIN}").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("SAPISIDHASH {ts}_{hex}")
}

fn cookie_header(c: &BTreeMap<String, String>) -> String {
    c.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
}

/// The headers of a signed youtubei request.
pub fn session_headers(c: &BTreeMap<String, String>, ts: u64, user_agent: &str) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("user-agent", user_agent.to_string()),
        ("cookie", cookie_header(c)),
        ("origin", ORIGIN.to_string()),
        ("x-origin", ORIGIN.to_string()),
        ("x-goog-authuser", "0".to_string()),
        ("content-type", "application/json".to_string()),
    ];
    if let Some(s) = sapisid_of(c) {
        h.push(("authorization", sapisid_hash(ts, s)));
    }
    h
}

/// `{"context":{"client":{...}}}` plus the call's own fields.
pub fn with_context(version: &str, extra: Value) -> Value {
    let mut b = json!({"context": {"client": {"clientName": "WEB", "clientVersion": version}}});
    if let (Some(o), Some(e)) = (b.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    b
}

/// The first value stored under `key` anywhere in `v`.
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|x| find_key(x, key))),
        Value::Array(a) => a.iter().find_map(|x| find_key(x, key)),
        _ => None,
    }
}

/// The first `browseId` that looks like a channel id (`UC` + 22).
fn find_channel_id(v: &Value) -> Option<String> {
    match v {
        Value::Object(m) => {
            if let Some(s) = m.get("browseId").and_then(Value::as_str) {
                if s.len() == 24 && s.starts_with("UC") {
                    return Some(s.to_string());
                }
            }
            m.values().find_map(find_channel_id)
        }
        Value::Array(a) => a.iter().find_map(find_channel_id),
        _ => None,
    }
}

/// The text of a `{simpleText}` or `{runs:[{text}]}` node.
pub fn text_of(n: &Value) -> String {
    if let Some(s) = n["simpleText"].as_str() {
        return s.to_string();
    }
    n["runs"].as_array().into_iter().flatten().filter_map(|r| r["text"].as_str()).collect::<String>()
}

/// YouTube's own words for a failed call: `{"error":{"message":..}}`.
pub fn error_message(body: &Value) -> Option<String> {
    body["error"]["message"].as_str().filter(|m| !m.is_empty()).map(String::from)
}

/// `account/account_menu` answer -> who the session is.
pub fn parse_account_menu(status: u16, body: &Value) -> Result<Identity, ApiError> {
    if !(200..300).contains(&status) {
        let message = match status {
            401 | 403 => "YouTube did not accept that login".to_string(),
            _ => error_message(body).unwrap_or_else(|| format!("YouTube answered {status} while checking the login")),
        };
        return Err(ApiError { status, message });
    }
    // a signed-out answer has no account header
    let Some(h) = find_key(body, "activeAccountHeaderRenderer") else {
        return Err(ApiError { status: 401, message: "YouTube did not accept that login".into() });
    };
    let name = text_of(&h["accountName"]).trim().to_string();
    let handle = text_of(&h["channelHandle"]).trim().trim_start_matches('@').to_string();
    if name.is_empty() && handle.is_empty() {
        return Err(ApiError { status, message: "YouTube did not say who this login belongs to".into() });
    }
    Ok(Identity { channel_id: find_channel_id(body).unwrap_or_default(), handle, name })
}

fn varint(mut n: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while n > 127 {
        out.push((n & 127) as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
    out
}

fn field(tag: u64, buf: &[u8]) -> Vec<u8> {
    let mut v = varint(tag);
    v.extend(varint(buf.len() as u64));
    v.extend_from_slice(buf);
    v
}

/// `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` as `%XX`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The `params` of `live_chat/send_message` (the protobuf youtubei.js builds): base64 of the percent-encoded base64.
pub fn send_message_params(video_id: &str, channel_id: &str, number0: u8) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut ids = field(10, channel_id.as_bytes());
    ids.extend(field(18, video_id.as_bytes()));
    let mut raw = field(10, &field(42, &ids));
    raw.extend([16, number0, 24, 4]);
    b64.encode(encode_uri_component(&b64.encode(&raw)).as_bytes())
}

/// The body of `live_chat/send_message`.
pub fn build_send_body(version: &str, params: &str, text: &str, client_message_id: &str) -> Value {
    with_context(version, json!({"params": params, "richMessage": {"textSegments": [{"text": text}]}, "clientMessageId": client_message_id}))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendError {
    pub status: u16,
    pub message: String,
}

/// `send_message` answer -> Ok, or YouTube's own error text.
pub fn parse_send_response(status: u16, body: &Value) -> Result<(), SendError> {
    if (200..300).contains(&status) && error_message(body).is_none() {
        return Ok(());
    }
    let message = match error_message(body) {
        Some(m) => m,
        None if status == 401 || status == 403 => EXPIRED_MESSAGE.to_string(),
        None => format!("YouTube rejected the message ({status})"),
    };
    Err(SendError { status: if (200..300).contains(&status) { 422 } else { status }, message })
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
    client_version: String,
    api_key: String,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn log(msg: &str) {
    let ctx = st().ctx.clone();
    if let Some(c) = ctx {
        c.log(&format!("[youtube] {msg}"));
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
        log(&format!("could not store the YouTube login: {e}"));
    }
}

/// Called once at startup (inside the tokio runtime). Does not block on the network.
pub fn init(ctx: Arc<Ctx>) {
    st().ctx = Some(ctx.clone());
    if let Some(v) = ctx.secrets.get(SECRET_KEY) {
        match serde_json::from_value::<Record>(v) {
            Ok(rec) if login_complete(&rec.cookies.iter().map(|(k, v)| (k.clone(), v.clone())).collect::<Vec<_>>()) => st().record = Some(Arc::new(rec)),
            Ok(_) => {}
            Err(_) => log("could not read the stored YouTube login"),
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

/// The client version and API key the site runs, as the watch page said (the reader passes them on).
pub fn set_client_info(version: &str, key: &str) {
    let mut g = st();
    if !version.is_empty() {
        g.client_version = version.to_string();
    }
    if !key.is_empty() {
        g.api_key = key.to_string();
    }
}

pub(crate) fn client_version() -> String {
    let g = st();
    if g.client_version.is_empty() { FALLBACK_CLIENT_VERSION.to_string() } else { g.client_version.clone() }
}

pub(crate) fn api_key() -> String {
    st().api_key.clone()
}

pub fn is_logged_in() -> bool {
    st().record.is_some()
}

/// The session expired (YouTube refused it) and nobody logged in again yet.
pub fn is_expired() -> bool {
    let g = st();
    g.expired && g.record.is_none()
}

/// The reason a send box gives without a login: the plain ask, or the expiry.
pub fn logged_out_reason() -> &'static str {
    if is_expired() { EXPIRED_MESSAGE } else { LOGGED_OUT_REASON }
}

pub fn me() -> Option<Identity> {
    st().record.as_ref().map(|r| Identity { channel_id: r.channel_id.clone(), handle: r.handle.clone(), name: r.display_name.clone() })
}

pub fn state() -> AccountState {
    let g = st();
    let mut out = AccountState { connected: g.record.is_some(), ..Default::default() };
    if let Some(r) = &g.record {
        out.user = Some(AccountUser { id: r.channel_id.clone(), login: r.handle.clone(), name: if r.display_name.is_empty() { format!("@{}", r.handle) } else { r.display_name.clone() } });
    }
    if let Some(p) = &g.pending {
        out.pending = Some(DevicePrompt { user_code: String::new(), verification_uri: String::new(), expires_at: p.expires_at });
    }
    out.error = g.last_error.clone();
    out
}

/// youtube.com refused the session: forget it. The account state and every YouTube column say to log in again.
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
        return Err("YouTube is not initialised.".into());
    }
    let Some(hook) = HOOK.lock().unwrap().clone() else { return Err("YouTube login is not available in this build.".into()) };
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
                Err("The YouTube login window timed out, try again.".into())
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
    if !login_complete(&c.pairs) {
        return Err("The YouTube login did not finish, try again.".into());
    }
    let cookies = keep_cookies(&c.pairs);
    let who = fetch_identity(&cookies).await.map_err(|e| format!("{} (YouTube login not saved)", e.message))?;
    Ok(Record { cookies, channel_id: who.channel_id, display_name: if who.name.is_empty() { who.handle.clone() } else { who.name }, handle: who.handle })
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

fn user_agent() -> String {
    st().ctx.as_ref().map(|c| c.user_agent.clone()).filter(|u| !u.is_empty()).unwrap_or_else(|| "AguChat".to_string())
}

/// One signed POST to youtube.com. Returns the status and the JSON body (null if it is not JSON).
async fn post_with(cookies: &BTreeMap<String, String>, url: &str, body: &Value, timeout_ms: u64) -> Result<(u16, Value), ApiError> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let mut o = FetchOpts::new(timeout_ms).method("POST");
    for (k, v) in session_headers(cookies, ts, &user_agent()) {
        o = o.header(k, &v);
    }
    o = o.body(body.to_string());
    let r = net::fetch(url, o).await.map_err(|e| ApiError { status: 0, message: if e.timed_out { "YouTube did not answer".into() } else { "Could not reach YouTube".into() } })?;
    let status = r.status().as_u16();
    let v = net::json(r).await.unwrap_or(Value::Null);
    Ok((status, v))
}

/// A signed POST with the logged-in session. A refused session (401/403) is forgotten (`mark_expired`) when
/// `expire_on_403` is set; 401 always is.
pub(crate) async fn call(url: &str, body: &Value, expire_on_403: bool) -> Result<(u16, Value), ApiError> {
    let Some(rec) = st().record.clone() else { return Err(ApiError { status: 401, message: logged_out_reason().into() }) };
    let (status, v) = post_with(&rec.cookies, url, body, 15_000).await?;
    if status == 401 || (status == 403 && expire_on_403) {
        mark_expired();
    }
    Ok((status, v))
}

async fn fetch_identity(cookies: &BTreeMap<String, String>) -> Result<Identity, ApiError> {
    let body = with_context(&client_version(), json!({}));
    let (status, v) = post_with(cookies, ACCOUNT_MENU_URL, &body, 15_000).await?;
    parse_account_menu(status, &v)
}

/// Post one chat message to a live broadcast as the logged-in user. YouTube's own error text comes back verbatim.
/// `video_id` and `channel_id` (UC...) are the broadcast's.
pub async fn send_chat(video_id: &str, channel_id: &str, text: &str) -> Result<(), SendError> {
    if !is_logged_in() {
        return Err(SendError { status: 401, message: logged_out_reason().into() });
    }
    let version = client_version();
    let mut last = SendError { status: 502, message: "YouTube did not answer".into() };
    // the number after the ids is not documented; 1 first, 2 when YouTube calls the request malformed
    for number0 in [1u8, 2] {
        let body = build_send_body(&version, &send_message_params(video_id, channel_id, number0), text, &uuid_v4());
        let (status, v) = call(SEND_URL, &body, true).await.map_err(|e| SendError { status: if e.status == 0 { 502 } else { e.status }, message: e.message })?;
        if status == 401 || status == 403 {
            return Err(SendError { status: 401, message: EXPIRED_MESSAGE.into() });
        }
        match parse_send_response(status, &v) {
            Ok(()) => return Ok(()),
            Err(e) if status == 400 => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn sapisid_hash_vector() {
        // sha1("1700000000 abc123 https://www.youtube.com")
        assert_eq!(sapisid_hash(1_700_000_000, "abc123"), "SAPISIDHASH 1700000000_9e5071f149fc514366f78b22d1a169786d40ed32");
    }

    #[test]
    fn headers_carry_the_session() {
        let c = keep_cookies(&pairs(&[("SAPISID", "S"), ("LOGIN_INFO", "L"), ("NOPE", "x"), ("SID", "")]));
        assert_eq!(c.len(), 2);
        let h = session_headers(&c, 1_700_000_000, "AguChat/1");
        let get = |k: &str| h.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str());
        assert_eq!(get("cookie"), Some("LOGIN_INFO=L; SAPISID=S"));
        assert!(get("authorization").unwrap().starts_with("SAPISIDHASH 1700000000_"));
        assert_eq!(get("origin"), Some("https://www.youtube.com"));
        assert_eq!(get("x-origin"), Some("https://www.youtube.com"));
        assert_eq!(get("x-goog-authuser"), Some("0"));
        assert_eq!(get("content-type"), Some("application/json"));
        assert_eq!(get("user-agent"), Some("AguChat/1"));
    }

    #[test]
    fn login_needs_sapisid_and_login_info() {
        assert!(login_complete(&pairs(&[("SAPISID", "a"), ("LOGIN_INFO", "b")])));
        assert!(login_complete(&pairs(&[("__Secure-3PAPISID", "a"), ("LOGIN_INFO", "b")])));
        assert!(!login_complete(&pairs(&[("SAPISID", "a")])));
        assert!(!login_complete(&pairs(&[("LOGIN_INFO", "b")])));
        assert!(!login_complete(&pairs(&[("SAPISID", ""), ("LOGIN_INFO", "b")])));
    }

    #[test]
    fn account_menu_parse() {
        let ok = json!({"actions": [{"openPopupAction": {"popup": {"multiPageMenuRenderer": {
            "header": {"activeAccountHeaderRenderer": {"accountName": {"simpleText": "Agu"}, "channelHandle": {"simpleText": "@agu_tf"}}},
            "sections": [{"multiPageMenuSectionRenderer": {"items": [{"compactLinkRenderer": {"navigationEndpoint": {"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv"}}}}]}}]}}}}]});
        assert_eq!(parse_account_menu(200, &ok).unwrap(), Identity { channel_id: "UCabcdefghijklmnopqrstuv".into(), handle: "agu_tf".into(), name: "Agu".into() });
        // signed-out answer: no header
        assert!(parse_account_menu(200, &json!({"actions": [{"openPopupAction": {}}]})).unwrap_err().is_auth());
        assert!(parse_account_menu(401, &Value::Null).unwrap_err().is_auth());
        assert!(parse_account_menu(403, &Value::Null).unwrap_err().is_auth());
        let e = parse_account_menu(500, &json!({"error": {"message": "Backend Error"}})).unwrap_err();
        assert_eq!((e.status, e.message.as_str()), (500, "Backend Error"));
    }

    #[test]
    fn send_params_bytes() {
        // checked against the youtubei.js encoder
        assert_eq!(send_message_params("dQw4w9WgXcQ", "UCabcdefghijklmnopqrstuv", 1), "Q2lrcUp3b1lWVU5oWW1Oa1pXWm5hR2xxYTJ4dGJtOXdjWEp6ZEhWMkVndGtVWGMwZHpsWFoxaGpVUkFCR0FRJTNE");
        assert_eq!(send_message_params("dQw4w9WgXcQ", "UCabcdefghijklmnopqrstuv", 2), "Q2lrcUp3b1lWVU5oWW1Oa1pXWm5hR2xxYTJ4dGJtOXdjWEp6ZEhWMkVndGtVWGMwZHpsWFoxaGpVUkFDR0FRJTNE");
        assert_eq!(encode_uri_component("a+b/c=d é"), "a%2Bb%2Fc%3Dd%20%C3%A9");
        assert_eq!(varint(300), vec![0xac, 0x02]);
    }

    #[test]
    fn send_body_and_answers() {
        let b = build_send_body("2.1", "P", "hi", "id1");
        assert_eq!(b, json!({"context": {"client": {"clientName": "WEB", "clientVersion": "2.1"}}, "params": "P", "richMessage": {"textSegments": [{"text": "hi"}]}, "clientMessageId": "id1"}));
        assert_eq!(parse_send_response(200, &json!({"actions": []})), Ok(()));
        assert_eq!(parse_send_response(400, &json!({"error": {"message": "Request contains an invalid argument."}})), Err(SendError { status: 400, message: "Request contains an invalid argument.".into() }));
        assert_eq!(parse_send_response(200, &json!({"error": {"message": "nope"}})), Err(SendError { status: 422, message: "nope".into() }));
        assert_eq!(parse_send_response(500, &Value::Null), Err(SendError { status: 500, message: "YouTube rejected the message (500)".into() }));
    }

    #[test]
    fn secrets_stay_out_of_debug() {
        let c = Cookies { pairs: pairs(&[("SAPISID", "SECRET")]) };
        assert!(!format!("{c:?}").contains("SECRET"));
        let r = Record { cookies: keep_cookies(&c.pairs), channel_id: "UC".into(), handle: "agu".into(), display_name: "Agu".into() };
        assert!(!format!("{r:?}").contains("SECRET"));
    }
}
