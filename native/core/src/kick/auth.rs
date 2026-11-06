//! Kick account: log in through the Agu Chat broker (api.agu.lol), keep the tokens, refresh them and send chat.
//!
//! WHY A BROKER. Kick's OAuth token exchange needs a client secret, which must never ship in a desktop app. The
//! broker holds it. This side only does PKCE (so nobody else can claim the tokens of a login we started):
//!   1. verifier = random, challenge = base64url(sha256(verifier))
//!   2. listen on 127.0.0.1:<random port>, open the browser at  <broker>/kick/app/start?challenge=..&port=..
//!   3. after consent Kick sends the browser to the broker, which sends it on to http://127.0.0.1:<port>/kick/done
//!      (or /kick/done?error=<message>); we answer with a "close this tab" page and stop listening (5 min limit)
//!   4. POST <broker>/kick/app/claim {verifier} -> tokens + user; later POST <broker>/kick/app/refresh {refresh_token}
//! Sending is Kick's public API with the user's own token: POST https://api.kick.com/public/v1/chat.
//!
//! The tokens live in the encrypted secrets store (key `kick`, like Twitch's `twitch`) and are never logged.
//! Dev safety, as for Twitch: env `AGU_AUTH_READONLY=1` never refreshes (a rotated refresh token would log the
//! real install out).
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::AbortHandle;

use crate::net::{self, str_nullish, FetchOpts};
use crate::platforms::Ctx;
use crate::protocol::{AccountState, AccountUser, DevicePrompt};
use crate::util::{now_ms, Listeners, Timer};

/// The broker. Override with env `AGU_KICK_BROKER` (tests).
pub const DEFAULT_BROKER: &str = "https://api.agu.lol";
const SEND_URL: &str = "https://api.kick.com/public/v1/chat";
const SECRET_KEY: &str = "kick";
/// The browser has this long to come back.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Refresh this long before the token runs out, so a request never rides on a dying token.
pub const REFRESH_MARGIN_MS: i64 = 2 * 60_000;
pub const MAX_CHARS: usize = 500;
pub const EXPIRED_MESSAGE: &str = "Kick login expired, log in again";
pub const LOGGED_OUT_REASON: &str = "Log in to Kick in Settings to chat";

/// "change": login / logout / expiry (not routine refreshes)
pub static EVENTS: Listeners<()> = Listeners::new();

pub fn broker_base() -> String {
    let v = std::env::var("AGU_KICK_BROKER").ok().map(|s| s.trim().trim_end_matches('/').to_string()).filter(|s| !s.is_empty());
    v.unwrap_or_else(|| DEFAULT_BROKER.to_string())
}

fn read_only() -> bool {
    std::env::var("AGU_AUTH_READONLY").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------------------
// Pure parts
// ---------------------------------------------------------------------------------------------------------

/// 32 random bytes -> 43 url-safe chars (RFC 7636 allows 43..=128).
pub fn new_verifier() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

/// `base64url(sha256(verifier))`, no padding (S256).
pub fn challenge_for(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn start_url(base: &str, challenge: &str, port: u16) -> String {
    format!("{base}/kick/app/start?challenge={challenge}&port={port}")
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KickUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in_s: i64,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub tokens: Tokens,
    pub user: KickUser,
}

/// Why a refresh failed: `Rejected` means the broker/Kick refused the token (logged out); `Transient` is
/// anything else (network, 5xx) where the old token may still be fine.
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshError {
    Rejected(String),
    Transient(String),
}

fn error_text(body: &Value) -> Option<String> {
    ["error", "message", "error_description"].iter().find_map(|k| body.get(*k).and_then(Value::as_str)).filter(|s| !s.is_empty()).map(String::from)
}

fn tokens_from(body: &Value, fallback_refresh: &str) -> Option<Tokens> {
    let access = body["access_token"].as_str().filter(|s| !s.is_empty())?.to_string();
    let refresh = body["refresh_token"].as_str().filter(|s| !s.is_empty()).unwrap_or(fallback_refresh).to_string();
    let exp = body["expires_in"].as_f64().filter(|n| *n > 0.0).unwrap_or(3600.0) as i64;
    let scope = match &body["scope"] {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    };
    Some(Tokens { access_token: access, refresh_token: refresh, expires_in_s: exp, scope })
}

/// `POST /kick/app/claim` answer: 200 {access_token, refresh_token, expires_in, scope, user:{id,slug,name}} or 404 {error}.
pub fn parse_claim(status: u16, body: &Value) -> Result<Claim, String> {
    if status != 200 {
        return Err(match error_text(body) {
            Some(e) => format!("Kick login could not be completed: {e}"),
            None => format!("Kick login could not be completed (answer {status}), try again."),
        });
    }
    let tokens = tokens_from(body, "").filter(|t| !t.refresh_token.is_empty()).ok_or("The Kick login service sent an unexpected reply.")?;
    let u = &body["user"];
    let user = KickUser { id: str_nullish(&u["id"]), slug: str_nullish(&u["slug"]), name: str_nullish(&u["name"]) };
    if user.id.is_empty() {
        return Err("The Kick login service sent an unexpected reply.".into());
    }
    Ok(Claim { tokens, user })
}

/// `POST /kick/app/refresh` answer: 200 {access_token, refresh_token, expires_in, scope} or 400/401 {error}.
pub fn parse_refresh(status: u16, body: &Value, old_refresh: &str) -> Result<Tokens, RefreshError> {
    match status {
        200 => tokens_from(body, old_refresh).ok_or_else(|| RefreshError::Transient("The Kick login service sent an unexpected reply.".into())),
        400 | 401 => Err(RefreshError::Rejected(error_text(body).unwrap_or_else(|| EXPIRED_MESSAGE.into()))),
        s => Err(RefreshError::Transient(error_text(body).unwrap_or_else(|| format!("Kick login service answered {s}")))),
    }
}

/// Refresh when the token has less than the margin left.
pub fn needs_refresh(expires_at_ms: i64, now: i64) -> bool {
    expires_at_ms - now < REFRESH_MARGIN_MS
}

/// The body of `POST /public/v1/chat`.
pub fn build_send_body(text: &str, broadcaster_user_id: i64, reply_to: Option<&str>) -> Value {
    let mut b = json!({"content": text, "type": "user", "broadcaster_user_id": broadcaster_user_id});
    if let Some(r) = reply_to.filter(|r| !r.is_empty()) {
        b["reply_to_message_id"] = json!(r);
    }
    b
}

/// What Kick's error answer says, else a generic line.
pub fn send_error_message(status: u16, body: &Value) -> String {
    match error_text(body) {
        Some(m) => m,
        None => format!("Kick rejected the message ({status})"),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Loopback callback
// ---------------------------------------------------------------------------------------------------------

const DONE_PAGE: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Agu Chat</title><style>body{font-family:system-ui,sans-serif;background:#18181b;color:#efeff1;display:flex;align-items:center;justify-content:center;height:100vh;margin:0}div{text-align:center}h1{font-size:20px;margin:0 0 8px}p{color:#adadb8;margin:0}</style></head><body><div><h1>Agu Chat</h1><p>You can close this tab and return to Agu Chat.</p></div></body></html>";

/// `GET <target> HTTP/1.1` -> the target. None for anything that is not a GET request line.
pub fn request_target(head: &str) -> Option<String> {
    let line = head.lines().next()?;
    let mut it = line.split_whitespace();
    if it.next()? != "GET" {
        return None;
    }
    it.next().map(String::from)
}

/// `/kick/done` -> Some(Ok), `/kick/done?error=x` -> Some(Err(x)), any other path -> None.
pub fn parse_done(target: &str) -> Option<Result<(), String>> {
    let u = url::Url::parse(&format!("http://127.0.0.1{target}")).ok()?;
    if u.path() != "/kick/done" {
        return None;
    }
    let err = u.query_pairs().find(|(k, _)| k == "error").map(|(_, v)| v.into_owned());
    Some(match err {
        Some(e) => Err(if e.trim().is_empty() { "Kick login was cancelled".into() } else { e }),
        None => Ok(()),
    })
}

async fn respond(s: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let msg = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}", body.len());
    let _ = s.write_all(msg.as_bytes()).await;
    let _ = s.shutdown().await;
}

/// Waits for the browser to land on /kick/done. Other requests (a favicon, a probe) get a 404 and are ignored.
pub async fn wait_for_callback(listener: &TcpListener, limit: Duration) -> Result<(), String> {
    let run = async {
        loop {
            let Ok((mut s, _)) = listener.accept().await else { continue };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let read = async {
                while buf.len() < 8192 && !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                }
            };
            if tokio::time::timeout(Duration::from_secs(5), read).await.is_err() {
                continue;
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            match request_target(&head).and_then(|t| parse_done(&t)) {
                Some(r) => {
                    respond(&mut s, "200 OK", DONE_PAGE).await;
                    return r;
                }
                None => respond(&mut s, "404 Not Found", "Not found").await,
            }
        }
    };
    match tokio::time::timeout(limit, run).await {
        Ok(r) => r,
        Err(_) => Err("Kick login timed out, try again.".into()),
    }
}

// ---------------------------------------------------------------------------------------------------------
// Broker calls
// ---------------------------------------------------------------------------------------------------------

fn user_agent() -> String {
    let ctx = st().ctx.clone();
    ctx.map(|c| c.user_agent.clone()).unwrap_or_else(|| "AguChat/0 (+https://github.com/TrinityTF/agu-chat)".to_string())
}

async fn json_post(url: &str, headers: &[(&str, String)], body: &Value) -> Result<(u16, Value), net::FetchError> {
    let mut o = FetchOpts::new(15_000).method("POST").header("Content-Type", "application/json").header("Accept", "application/json").header("User-Agent", &user_agent());
    for (k, v) in headers {
        o = o.header(k, v);
    }
    let r = net::fetch(url, o.body(body.to_string())).await?;
    let status = r.status().as_u16();
    let v = net::json(r).await.unwrap_or(Value::Null);
    Ok((status, v))
}

fn unreachable(e: &net::FetchError, what: &str) -> String {
    if e.timed_out {
        format!("{what} did not answer")
    } else {
        format!("Could not reach {what}")
    }
}

pub async fn claim(base: &str, verifier: &str) -> Result<Claim, String> {
    let (s, v) = json_post(&format!("{base}/kick/app/claim"), &[], &json!({"verifier": verifier})).await.map_err(|e| unreachable(&e, "the Kick login service"))?;
    parse_claim(s, &v)
}

pub async fn refresh_tokens(base: &str, refresh_token: &str) -> Result<Tokens, RefreshError> {
    let (s, v) = json_post(&format!("{base}/kick/app/refresh"), &[], &json!({"refresh_token": refresh_token})).await.map_err(|e| RefreshError::Transient(unreachable(&e, "the Kick login service")))?;
    parse_refresh(s, &v, refresh_token)
}

/// A bound loopback listener plus the PKCE pair and the URL to open.
pub struct Prepared {
    pub listener: TcpListener,
    pub verifier: String,
    pub url: String,
}

pub async fn prepare(base: &str) -> Result<Prepared, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.map_err(|_| "Could not start the local login listener".to_string())?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let verifier = new_verifier();
    let url = start_url(base, &challenge_for(&verifier), port);
    Ok(Prepared { listener, verifier, url })
}

/// Wait for the browser, then claim the tokens.
pub async fn complete(p: Prepared, base: &str, limit: Duration) -> Result<Claim, String> {
    wait_for_callback(&p.listener, limit).await.map_err(|e| if e.starts_with("Kick login") { e } else { format!("Kick login failed: {e}") })?;
    let Prepared { listener, verifier, .. } = p;
    drop(listener);
    claim(base, &verifier).await
}

/// The whole login with an injectable "open the browser" step (tests drive the loopback by hand).
pub async fn authorize_with(base: &str, open: impl FnOnce(&str), limit: Duration) -> Result<Claim, String> {
    let p = prepare(base).await?;
    open(&p.url);
    complete(p, base, limit).await
}

// ---------------------------------------------------------------------------------------------------------
// Stored login + state
// ---------------------------------------------------------------------------------------------------------

/// The stored login (the JSON stored under `kick` in secrets.bin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub scope: String,
    pub user: KickUser,
}

struct Flow {
    id: u64,
    url: String,
    expires_at: i64,
    task: Option<AbortHandle>,
}

#[derive(Default)]
struct State {
    ctx: Option<Arc<Ctx>>,
    record: Option<Arc<Record>>,
    pending: Option<Flow>,
    last_error: Option<String>,
    refresh_timer: Timer,
    seq: u64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));
static REFRESH_LOCK: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));
type Opener = Arc<dyn Fn(&str) + Send + Sync>;
static OPENER: Mutex<Option<Opener>> = Mutex::new(None);

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn log(msg: &str) {
    let ctx = st().ctx.clone();
    if let Some(c) = ctx {
        c.log(&format!("[kick] {msg}"));
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
        log(&format!("could not store the Kick token: {e}"));
    }
}

/// Where login URLs go. The backend installs the real opener; without one nothing is opened.
pub fn set_opener(f: Opener) {
    *OPENER.lock().unwrap() = Some(f);
}

fn open_browser(url: &str) {
    let f = OPENER.lock().unwrap().clone();
    if let Some(f) = f {
        f(url);
    }
}

/// Called once at startup (inside the tokio runtime). Does not block on the network.
pub fn init(ctx: Arc<Ctx>) {
    st().ctx = Some(ctx.clone());
    if let Some(v) = ctx.secrets.get(SECRET_KEY) {
        match serde_json::from_value::<Record>(v) {
            Ok(rec) if !rec.access_token.is_empty() && !rec.user.id.is_empty() => {
                st().record = Some(Arc::new(rec));
                schedule_refresh();
            }
            Ok(_) => {}
            Err(_) => log("could not read the stored Kick login"),
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

pub fn state() -> AccountState {
    let g = st();
    let mut out = AccountState { connected: g.record.is_some(), ..Default::default() };
    if let Some(r) = &g.record {
        let login = if r.user.slug.is_empty() { r.user.name.clone() } else { r.user.slug.clone() };
        out.user = Some(AccountUser { id: r.user.id.clone(), login, name: if r.user.name.is_empty() { r.user.slug.clone() } else { r.user.name.clone() } });
        out.scopes = Some(r.scope.split_whitespace().map(String::from).collect());
    }
    if let Some(p) = &g.pending {
        out.pending = Some(DevicePrompt { user_code: String::new(), verification_uri: p.url.clone(), expires_at: p.expires_at });
    }
    out.error = g.last_error.clone();
    out
}

fn record_from(claim: &Claim) -> Record {
    Record {
        access_token: claim.tokens.access_token.clone(),
        refresh_token: claim.tokens.refresh_token.clone(),
        expires_at: now_ms() + claim.tokens.expires_in_s * 1000,
        scope: claim.tokens.scope.clone(),
        user: claim.user.clone(),
    }
}

// ---------- login ----------

/// Starts the browser login and returns at once; the account state follows (pending -> connected | error).
pub async fn login() -> Result<Value, String> {
    if st().ctx.is_none() {
        return Err("Kick is not initialised.".into());
    }
    cancel();
    st().last_error = None;
    let base = broker_base();
    let p = prepare(&base).await?;
    let url = p.url.clone();
    let id = {
        let mut g = st();
        g.seq += 1;
        g.seq
    };
    let task = tokio::spawn(async move {
        let r = complete(p, &base, LOGIN_TIMEOUT).await;
        finish(id, r);
    });
    {
        let mut g = st();
        g.pending = Some(Flow { id, url: url.clone(), expires_at: now_ms() + LOGIN_TIMEOUT.as_millis() as i64, task: Some(task.abort_handle()) });
    }
    open_browser(&url);
    changed();
    Ok(json!({"pending": true}))
}

fn finish(id: u64, r: Result<Claim, String>) {
    {
        let mut g = st();
        if g.pending.as_ref().map(|f| f.id) != Some(id) {
            return; // cancelled or replaced
        }
        g.pending = None;
        match r {
            Ok(c) => {
                g.record = Some(Arc::new(record_from(&c)));
                g.last_error = None;
            }
            Err(e) => g.last_error = Some(e),
        }
    }
    persist();
    schedule_refresh();
    changed();
}

pub fn cancel() {
    {
        let mut g = st();
        match g.pending.take() {
            Some(f) => {
                if let Some(t) = f.task {
                    t.abort();
                }
            }
            None => return,
        }
    }
    changed();
}

pub async fn logout() {
    cancel();
    {
        let mut g = st();
        g.record = None;
        g.last_error = None;
        g.refresh_timer.clear();
    }
    persist();
    changed();
}

// ---------- refresh ----------

fn schedule_refresh() {
    let mut g = st();
    g.refresh_timer.clear();
    let Some(rec) = g.record.clone() else { return };
    if read_only() {
        return;
    }
    let wait = (rec.expires_at - now_ms() - REFRESH_MARGIN_MS).max(5_000).min(i32::MAX as i64);
    let stale = rec.access_token.clone();
    g.refresh_timer.set(Duration::from_millis(wait as u64), async move {
        refresh_if(&stale).await;
    });
}

/// Refresh unless somebody already did (the token differs from `stale`). True when a usable login remains.
/// A refusal logs the user out; a transient failure keeps the login (the old token may still work).
pub async fn refresh_if(stale: &str) -> bool {
    let _guard = REFRESH_LOCK.lock().await;
    let Some(rec) = st().record.clone() else { return false };
    if rec.access_token != stale {
        return true;
    }
    if read_only() || rec.refresh_token.is_empty() {
        return false;
    }
    match refresh_tokens(&broker_base(), &rec.refresh_token).await {
        Ok(t) => {
            {
                let mut g = st();
                // Logged out or re-logged in while the request was in flight: keep that.
                if g.record.as_ref().map(|r| r.access_token != stale).unwrap_or(true) {
                    return g.record.is_some();
                }
                let scope = if t.scope.is_empty() { rec.scope.clone() } else { t.scope };
                g.record = Some(Arc::new(Record { access_token: t.access_token, refresh_token: t.refresh_token, expires_at: now_ms() + t.expires_in_s * 1000, scope, user: rec.user.clone() }));
            }
            persist();
            schedule_refresh();
            true
        }
        Err(RefreshError::Rejected(_)) => {
            {
                let mut g = st();
                g.record = None;
                g.refresh_timer.clear();
                g.last_error = Some(EXPIRED_MESSAGE.into());
            }
            persist();
            changed();
            false
        }
        Err(RefreshError::Transient(e)) => {
            log(&format!("token refresh failed: {e}"));
            false
        }
    }
}

/// The access token, refreshed first if it is about to run out. None when logged out.
pub async fn access_token() -> Option<String> {
    let rec = st().record.clone()?;
    if needs_refresh(rec.expires_at, now_ms()) {
        refresh_if(&rec.access_token).await;
    }
    st().record.as_ref().map(|r| r.access_token.clone())
}

// ---------- sending ----------

#[derive(Debug, Clone, PartialEq)]
pub struct SendError {
    pub status: u16,
    pub message: String,
}

async fn post_chat(url: &str, token: &str, body: &Value) -> Result<(u16, Value), SendError> {
    json_post(url, &[("Authorization", format!("Bearer {token}"))], body).await.map_err(|e| SendError { status: 502, message: unreachable(&e, "Kick") })
}

/// Post one chat message as the logged-in user. A 401 from Kick refreshes the token and retries once.
pub async fn send_chat(text: &str, broadcaster_user_id: i64, reply_to: Option<&str>) -> Result<(), SendError> {
    send_chat_to(SEND_URL, text, broadcaster_user_id, reply_to).await
}

pub async fn send_chat_to(url: &str, text: &str, broadcaster_user_id: i64, reply_to: Option<&str>) -> Result<(), SendError> {
    if text.chars().count() > MAX_CHARS {
        return Err(SendError { status: 400, message: format!("Kick messages can be at most {MAX_CHARS} characters") });
    }
    let Some(token) = access_token().await else { return Err(SendError { status: 401, message: LOGGED_OUT_REASON.into() }) };
    let body = build_send_body(text, broadcaster_user_id, reply_to);
    let (mut status, mut v) = post_chat(url, &token, &body).await?;
    if status == 401 {
        if !refresh_if(&token).await {
            return Err(SendError { status: 401, message: EXPIRED_MESSAGE.into() });
        }
        let t2 = st().record.as_ref().map(|r| r.access_token.clone());
        let Some(t2) = t2 else { return Err(SendError { status: 401, message: EXPIRED_MESSAGE.into() }) };
        (status, v) = post_chat(url, &t2, &body).await?;
    }
    if (200..300).contains(&status) {
        if v["data"]["is_sent"] == json!(false) {
            return Err(SendError { status: 422, message: "Kick did not send the message".into() });
        }
        return Ok(());
    }
    Err(SendError { status, message: send_error_message(status, &v) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_the_rfc_7636_example() {
        assert_eq!(challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifiers_are_valid_and_unique() {
        let (a, b) = (new_verifier(), new_verifier());
        assert_ne!(a, b);
        assert!((43..=128).contains(&a.len()));
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
        assert!(!challenge_for(&a).contains('='));
    }

    #[test]
    fn start_url_carries_challenge_and_port() {
        assert_eq!(start_url("https://api.agu.lol", "abc", 51234), "https://api.agu.lol/kick/app/start?challenge=abc&port=51234");
    }

    #[test]
    fn claim_response_is_parsed() {
        let body = json!({"access_token": "A", "refresh_token": "R", "expires_in": 7200, "scope": "chat:write user:read", "user": {"id": 42, "slug": "some-user", "name": "Some_User"}});
        let c = parse_claim(200, &body).unwrap();
        assert_eq!(c.tokens, Tokens { access_token: "A".into(), refresh_token: "R".into(), expires_in_s: 7200, scope: "chat:write user:read".into() });
        assert_eq!(c.user, KickUser { id: "42".into(), slug: "some-user".into(), name: "Some_User".into() });
    }

    #[test]
    fn claim_failures_carry_the_broker_message() {
        let e = parse_claim(404, &json!({"error": "unknown verifier"})).unwrap_err();
        assert!(e.contains("unknown verifier"));
        assert!(parse_claim(404, &Value::Null).unwrap_err().contains("404"));
        // 200 without a refresh token or user is not a login
        assert!(parse_claim(200, &json!({"access_token": "A", "user": {"id": 1}})).is_err());
        assert!(parse_claim(200, &json!({"access_token": "A", "refresh_token": "R"})).is_err());
    }

    #[test]
    fn refresh_response_is_parsed() {
        let t = parse_refresh(200, &json!({"access_token": "A2", "refresh_token": "R2", "expires_in": 3600, "scope": "chat:write"}), "R").unwrap();
        assert_eq!((t.access_token.as_str(), t.refresh_token.as_str(), t.expires_in_s), ("A2", "R2", 3600));
        // no new refresh token: keep the old one
        assert_eq!(parse_refresh(200, &json!({"access_token": "A3"}), "R").unwrap().refresh_token, "R");
        assert_eq!(parse_refresh(401, &json!({"error": "invalid_grant"}), "R"), Err(RefreshError::Rejected("invalid_grant".into())));
        assert!(matches!(parse_refresh(400, &Value::Null, "R"), Err(RefreshError::Rejected(_))));
        assert!(matches!(parse_refresh(503, &Value::Null, "R"), Err(RefreshError::Transient(_))));
        assert!(matches!(parse_refresh(200, &json!({}), "R"), Err(RefreshError::Transient(_))));
    }

    #[test]
    fn expiry_decides_the_refresh() {
        let now = 1_000_000;
        assert!(needs_refresh(now - 1, now));
        assert!(needs_refresh(now + REFRESH_MARGIN_MS - 1, now));
        assert!(!needs_refresh(now + REFRESH_MARGIN_MS, now));
        assert!(!needs_refresh(now + 3_600_000, now));
    }

    #[test]
    fn send_body_has_the_documented_shape() {
        assert_eq!(build_send_body("hi", 99, None), json!({"content": "hi", "type": "user", "broadcaster_user_id": 99}));
        assert_eq!(build_send_body("hi", 99, Some("abc-123")), json!({"content": "hi", "type": "user", "broadcaster_user_id": 99, "reply_to_message_id": "abc-123"}));
        assert!(build_send_body("hi", 99, Some(""))["reply_to_message_id"].is_null());
    }

    #[test]
    fn kick_errors_surface_their_message() {
        assert_eq!(send_error_message(403, &json!({"message": "You are banned from this chat"})), "You are banned from this chat");
        assert_eq!(send_error_message(500, &Value::Null), "Kick rejected the message (500)");
    }

    #[test]
    fn callback_urls_are_recognised() {
        assert_eq!(request_target("GET /kick/done HTTP/1.1\r\nHost: x\r\n\r\n").as_deref(), Some("/kick/done"));
        assert_eq!(request_target("POST /kick/done HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_done("/kick/done"), Some(Ok(())));
        assert_eq!(parse_done("/kick/done?error=access_denied"), Some(Err("access_denied".into())));
        assert_eq!(parse_done("/kick/done?error=User%20said%20no"), Some(Err("User said no".into())));
        assert_eq!(parse_done("/favicon.ico"), None);
        assert_eq!(parse_done("/kick/donefoo"), None);
    }

    // ---- a mock HTTP server on loopback ----

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Serves `answers` in order (status, body) to successive requests and records "METHOD path auth body".
    async fn mock_server(answers: Vec<(u16, String)>) -> (u16, Arc<Mutex<Vec<String>>>) {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let n = Arc::new(AtomicUsize::new(0));
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                let (head_end, clen) = loop {
                    let k = s.read(&mut chunk).await.unwrap_or(0);
                    if k == 0 {
                        break (0, 0);
                    }
                    buf.extend_from_slice(&chunk[..k]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..p]).to_lowercase();
                        let cl = head.lines().find_map(|l| l.strip_prefix("content-length:")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        break (p + 4, cl);
                    }
                };
                while head_end > 0 && buf.len() < head_end + clen {
                    let k = s.read(&mut chunk).await.unwrap_or(0);
                    if k == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..k]);
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let first = text.lines().next().unwrap_or("").to_string();
                let mut it = first.split_whitespace();
                let (m, p) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
                let auth = text.lines().find(|l| l.to_lowercase().starts_with("authorization:")).map(|l| l.split_once(':').unwrap().1.trim().to_string()).unwrap_or_default();
                let body = text.get(head_end..).unwrap_or("").to_string();
                seen2.lock().unwrap().push(format!("{m} {p} [{auth}] {body}"));
                let i = n.fetch_add(1, Ordering::SeqCst).min(answers.len() - 1);
                let (code, abody) = &answers[i];
                let resp = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{abody}", abody.len());
                let _ = s.write_all(resp.as_bytes()).await;
                let _ = s.shutdown().await;
            }
        });
        (port, seen)
    }

    async fn browser_hits(url: &str, path_and_query: &str) {
        let port: u16 = url.split("port=").nth(1).unwrap().parse().unwrap();
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        c.write_all(format!("GET {path_and_query} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").as_bytes()).await.unwrap();
        let mut out = String::new();
        let _ = c.read_to_string(&mut out).await;
        assert!(out.starts_with("HTTP/1.1 200") || out.starts_with("HTTP/1.1 404"), "{out}");
    }

    async fn wait_for_url(slot: &Arc<Mutex<String>>) -> String {
        loop {
            let u = slot.lock().unwrap().clone();
            if !u.is_empty() {
                return u;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn the_login_flow_claims_with_the_matching_verifier() {
        let claim_json = json!({"access_token": "A", "refresh_token": "R", "expires_in": 7200, "scope": "chat:write", "user": {"id": 7, "slug": "u", "name": "U"}}).to_string();
        let (bport, seen) = mock_server(vec![(200, claim_json)]).await;
        let base = format!("http://127.0.0.1:{bport}");
        let url_slot: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let (u2, b2) = (url_slot.clone(), base.clone());
        let handle = tokio::spawn(async move { authorize_with(&b2, move |u| *u2.lock().unwrap() = u.to_string(), Duration::from_secs(20)).await });
        // the "browser": hit a stray path first (ignored), then the real callback
        let url = wait_for_url(&url_slot).await;
        assert!(url.starts_with(&format!("{base}/kick/app/start?challenge=")));
        browser_hits(&url, "/favicon.ico").await;
        browser_hits(&url, "/kick/done").await;
        let claim = handle.await.unwrap().unwrap();
        assert_eq!(claim.user.slug, "u");
        let req = seen.lock().unwrap()[0].clone();
        assert!(req.starts_with("POST /kick/app/claim"), "{req}");
        let verifier = serde_json::from_str::<Value>(&req[req.find('{').unwrap()..]).unwrap()["verifier"].as_str().unwrap().to_string();
        let challenge = url.split("challenge=").nth(1).unwrap().split('&').next().unwrap();
        assert_eq!(challenge_for(&verifier), challenge);
    }

    #[tokio::test]
    async fn a_denied_login_reports_the_error_and_never_claims() {
        let (bport, seen) = mock_server(vec![(200, "{}".into())]).await;
        let base = format!("http://127.0.0.1:{bport}");
        let url_slot: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let u2 = url_slot.clone();
        let handle = tokio::spawn(async move { authorize_with(&base, move |u| *u2.lock().unwrap() = u.to_string(), Duration::from_secs(20)).await });
        let url = wait_for_url(&url_slot).await;
        browser_hits(&url, "/kick/done?error=access_denied").await;
        let e = handle.await.unwrap().unwrap_err();
        assert!(e.contains("access_denied"), "{e}");
        assert!(seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_login_times_out() {
        let r = authorize_with("http://127.0.0.1:9", |_| {}, Duration::from_millis(150)).await;
        assert!(r.unwrap_err().contains("timed out"));
    }

    #[tokio::test]
    async fn refresh_goes_to_the_broker() {
        let (bport, seen) = mock_server(vec![(200, json!({"access_token": "A2", "refresh_token": "R2", "expires_in": 3600, "scope": "chat:write"}).to_string()), (401, json!({"error": "invalid_grant"}).to_string())]).await;
        let base = format!("http://127.0.0.1:{bport}");
        let t = refresh_tokens(&base, "R1").await.unwrap();
        assert_eq!(t.access_token, "A2");
        assert_eq!(refresh_tokens(&base, "R2").await, Err(RefreshError::Rejected("invalid_grant".into())));
        let s = seen.lock().unwrap().clone();
        assert!(s[0].starts_with("POST /kick/app/refresh") && s[0].contains("\"refresh_token\":\"R1\""), "{}", s[0]);
    }

    #[tokio::test]
    async fn sending_posts_the_body_with_the_bearer_token() {
        // the login state is process-global: this is the only test that sets it
        st().record = Some(Arc::new(Record { access_token: "TOK".into(), refresh_token: "R".into(), expires_at: now_ms() + 3_600_000, scope: String::new(), user: KickUser { id: "1".into(), slug: "me".into(), name: "Me".into() } }));
        let (kport, seen) = mock_server(vec![(200, json!({"data": {"is_sent": true, "message_id": "m"}, "message": "OK"}).to_string()), (403, json!({"message": "You are banned from this chat"}).to_string())]).await;
        let url = format!("http://127.0.0.1:{kport}/public/v1/chat");
        send_chat_to(&url, "hello", 99, Some("rid")).await.unwrap();
        let e = send_chat_to(&url, "again", 99, None).await.unwrap_err();
        assert_eq!((e.status, e.message.as_str()), (403, "You are banned from this chat"));
        let s = seen.lock().unwrap().clone();
        assert!(s[0].starts_with("POST /public/v1/chat [Bearer TOK]"), "{}", s[0]);
        assert!(s[0].contains("\"reply_to_message_id\":\"rid\"") && s[0].contains("\"broadcaster_user_id\":99"));
        let long = "x".repeat(501);
        assert_eq!(send_chat_to(&url, &long, 99, None).await.unwrap_err().status, 400);
        st().record = None;
        assert_eq!(send_chat_to(&url, "hi", 99, None).await.unwrap_err().message, LOGGED_OUT_REASON);
    }
}
