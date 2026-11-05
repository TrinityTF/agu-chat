//! Port of src/main/twitch/auth.js: Twitch account: OAuth device code flow with Agu Chat's own PUBLIC client
//! id (no client secret, no redirect server, so nothing of ours has to be online), token storage, refresh and
//! the hourly validate Twitch requires. A client id is not a secret: every Twitch desktop client ships its
//! own. Settings can override it.
//!
//! Dev safety (not in the JS): with env `AGU_AUTH_READONLY=1` the token is never refreshed or revoked. A run
//! against a COPY of the Electron app's secrets.bin must not rotate the refresh token, or the real install
//! would be logged out.
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, Shared};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::http::{request_json, ReqOpts};
use super::rt::{self, log};
use crate::protocol::{AccountState, AccountUser, DevicePrompt};
use crate::util::{js_trim, now_ms, Listeners, Timer};

const ID: &str = "https://id.twitch.tv/oauth2";
/// The "Agu Chat" app on dev.twitch.tv (client type Public).
pub const DEFAULT_CLIENT_ID: &str = "lq9jr9g9swe9tbdzhp2vcd2htg9hbg";
const HELIX: &str = "https://api.twitch.tv/helix";
const SECRET_KEY: &str = "twitch";
const VALIDATE_EVERY_MS: u64 = 60 * 60_000;
/// Refresh this long before the token runs out, so a request never rides on a dying token.
const REFRESH_MARGIN_MS: i64 = 5 * 60_000;
pub const EXPIRED_MESSAGE: &str = "Twitch login expired, log in again";

/// Everything the chat page asks for, in one grant (the user logs in once).
pub const SCOPES: &[&str] = &[
    "user:read:chat",
    "user:write:chat", // /api/chat/send
    "user:read:emotes", // the emote picker lists the emotes this user can actually send
    "user:read:moderated_channels", // which channels the user moderates: gates the named mod feed and mod buttons
    // Mod actions the chat page offers.
    "moderator:manage:banned_users", // ban / timeout / unban
    "moderator:manage:chat_messages", // delete, clear, pin
    "moderator:manage:warnings",
    "moderator:manage:chat_settings", // the Modes panel
    "moderator:manage:announcements",
    "moderator:manage:shoutouts",
    "moderator:manage:shield_mode",
    "moderator:manage:automod", // AutoMod Allow / Deny
    "moderator:manage:blocked_terms",
    // Read scopes EventSub's channel.moderate v2 and suspicious-user events demand as a set.
    "moderator:read:blocked_terms",
    "moderator:read:chat_settings",
    "moderator:read:unban_requests",
    "moderator:read:moderators",
    "moderator:read:vips",
    "moderator:read:suspicious_users",
    "moderator:read:followers", // "following since" on the user card
    "moderator:read:chatters", // the chatter list behind name suggestions
    "channel:moderate", // legacy chat-moderation scope, requested because the agu.lol chat page did
    "channel:manage:raids", // /raid: broadcaster only, Twitch does not let mods raid
    // Only used on the user's own channel, to label channel-point redeems and Bits Power-ups in chat.
    "channel:read:redemptions",
    "bits:read",
];

/// "change": login / logout / expiry / scope change (not routine refreshes)
pub static EVENTS: Listeners<()> = Listeners::new();

/// The stored login (the JSON stored under `twitch` in secrets.bin).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    #[serde(default)]
    pub expires_at: i64,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub client_id: String,
    pub user: RecordUser,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordUser {
    pub id: String,
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub name: String,
}

struct Flow {
    id: u64,
    client_id: String,
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_at: i64,
    interval: u64,
    timer: Timer,
}

#[derive(Default)]
struct State {
    record: Option<Arc<Record>>,
    pending: Option<Flow>,
    last_error: Option<String>,
    refreshing: Option<Shared<BoxFuture<'static, bool>>>,
    refresh_timer: Timer,
    validate_timer: Timer,
    seq: u64,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn read_only() -> bool {
    std::env::var("AGU_AUTH_READONLY").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

fn changed() {
    EVENTS.emit(&());
    if let Some(ctx) = rt::ctx() {
        ctx.accounts_changed();
    }
}

fn persist() {
    let Some(ctx) = rt::ctx() else { return };
    let rec = st().record.clone();
    let res = match rec {
        Some(r) => ctx.secrets.set(SECRET_KEY, serde_json::to_value(&*r).unwrap_or(Value::Null)),
        None => ctx.secrets.delete(SECRET_KEY),
    };
    if let Err(e) = res {
        log(&format!("could not store the Twitch token {e}"));
    }
}

/// Called once at startup (inside the tokio runtime). Does not block on the network.
pub fn init(ctx: Arc<crate::platforms::Ctx>) {
    rt::set_ctx(ctx.clone());
    let saved = ctx.secrets.get(SECRET_KEY);
    if let Some(v) = saved {
        match serde_json::from_value::<Record>(v) {
            Ok(rec) if !rec.access_token.is_empty() && !rec.user.id.is_empty() => {
                st().record = Some(Arc::new(rec));
                schedule_refresh();
                start_validate_loop();
                tokio::spawn(validate());
            }
            Ok(_) => {}
            Err(e) => log(&format!("could not read the stored Twitch token: {e}")),
        }
    }
}

pub fn state() -> AccountState {
    let s = st();
    let mut out = AccountState { connected: s.record.is_some(), ..Default::default() };
    if let Some(r) = &s.record {
        out.user = Some(AccountUser { id: r.user.id.clone(), login: r.user.login.clone(), name: r.user.name.clone() });
        out.scopes = Some(r.scopes.clone());
    }
    if let Some(p) = &s.pending {
        out.pending = Some(DevicePrompt { user_code: p.user_code.clone(), verification_uri: p.verification_uri.clone(), expires_at: p.expires_at });
    }
    out.error = s.last_error.clone();
    out
}

/// Current token, synchronously: for other modules. May be seconds from refresh.
pub struct UserToken {
    pub token: String,
    pub user: RecordUser,
    pub scopes: Vec<String>,
    pub client_id: String,
}

pub fn get_user_token() -> Option<UserToken> {
    let r = st().record.clone()?;
    Some(UserToken { token: r.access_token.clone(), user: r.user.clone(), scopes: r.scopes.clone(), client_id: r.client_id.clone() })
}

pub fn current() -> Option<Arc<Record>> {
    st().record.clone()
}

/// The record, refreshed first if it is about to run out. None when logged out.
pub async fn fresh_record() -> Option<Arc<Record>> {
    let rec = current();
    if let Some(r) = &rec {
        if r.expires_at - now_ms() < 60_000 {
            refresh_now().await;
        }
    }
    current()
}

pub fn has_scope(scope: &str) -> bool {
    current().map(|r| r.scopes.iter().any(|s| s == scope)).unwrap_or(false)
}

// ---------- login (device code flow) ----------

fn scopes_param() -> String {
    SCOPES.join(" ")
}

fn num(v: &Value) -> f64 {
    crate::util::js_to_number(Some(v))
}

/// Starts the device code flow. The Err is the message to show.
pub async fn login() -> Result<DevicePrompt, String> {
    let ctx = rt::ctx().ok_or("Twitch is not initialised.")?;
    let typed = ctx.settings.get().get("twitchClientId").and_then(Value::as_str).map(|s| js_trim(s).to_string()).unwrap_or_default();
    let client_id = if typed.is_empty() { DEFAULT_CLIENT_ID.to_string() } else { typed };
    cancel();
    st().last_error = None;
    let dev = request_json(&format!("{ID}/device"), ReqOpts { method: "POST", form: Some(vec![("client_id", client_id.clone()), ("scopes", scopes_param())]), ..Default::default() }).await;
    let dev = match dev {
        Ok(d) => d,
        Err(err) => {
            if (400..500).contains(&err.status) {
                return Err(format!(
                    "Twitch did not accept that client ID ({}). If you set your own Twitch app in Settings, check its client ID and that its client type is \"Public\".",
                    err.message
                ));
            }
            return Err(if err.message.is_empty() { "Could not start the Twitch login.".into() } else { err.message });
        }
    };
    let s = |k: &str| dev.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()).map(String::from);
    let (Some(device_code), Some(user_code), Some(verification_uri)) = (s("device_code"), s("user_code"), s("verification_uri")) else {
        return Err("Twitch sent an unexpected reply to the login request.".into());
    };
    let expires_in = Some(num(&dev["expires_in"])).filter(|n| *n != 0.0 && !n.is_nan()).unwrap_or(600.0);
    let interval = Some(num(&dev["interval"])).filter(|n| *n != 0.0 && !n.is_nan()).unwrap_or(5.0).max(1.0) as u64;
    let prompt;
    let flow_id;
    {
        let mut g = st();
        g.seq += 1;
        flow_id = g.seq;
        let flow = Flow {
            id: flow_id,
            client_id,
            device_code,
            user_code: user_code.clone(),
            verification_uri: verification_uri.clone(),
            expires_at: now_ms() + (expires_in * 1000.0) as i64,
            interval,
            timer: Timer::new(),
        };
        prompt = DevicePrompt { user_code, verification_uri, expires_at: flow.expires_at };
        g.pending = Some(flow);
    }
    schedule_poll(flow_id);
    changed();
    Ok(prompt)
}

fn schedule_poll(flow_id: u64) {
    let mut g = st();
    if let Some(f) = g.pending.as_mut().filter(|f| f.id == flow_id) {
        let d = Duration::from_secs(f.interval);
        f.timer.set(d, poll(flow_id));
    }
}

fn fail_flow(flow_id: u64, message: &str) {
    {
        let mut g = st();
        match g.pending.as_mut() {
            Some(f) if f.id == flow_id => f.timer.clear(),
            _ => return,
        }
        g.pending = None;
        g.last_error = Some(message.to_string());
    }
    changed();
}

fn is_pending(flow_id: u64) -> bool {
    st().pending.as_ref().map(|f| f.id == flow_id).unwrap_or(false)
}

async fn poll(flow_id: u64) {
    let (client_id, device_code, expired) = {
        let g = st();
        match g.pending.as_ref().filter(|f| f.id == flow_id) {
            Some(f) => (f.client_id.clone(), f.device_code.clone(), now_ms() > f.expires_at),
            None => return,
        }
    };
    if expired {
        return fail_flow(flow_id, "The Twitch login code expired. Start the login again.");
    }
    let tok = request_json(
        &format!("{ID}/token"),
        ReqOpts {
            method: "POST",
            form: Some(vec![("client_id", client_id), ("scopes", scopes_param()), ("device_code", device_code), ("grant_type", "urn:ietf:params:oauth:grant-type:device_code".into())]),
            ..Default::default()
        },
    )
    .await;
    let tok = match tok {
        Ok(t) => t,
        Err(err) => {
            let msg = err.message.to_lowercase();
            if !is_pending(flow_id) {
                return;
            }
            if err.status == 0 || err.status >= 500 {
                return schedule_poll(flow_id); // transient: keep polling until the code expires
            }
            if msg.contains("authorization_pending") {
                return schedule_poll(flow_id);
            }
            if msg.contains("slow_down") {
                if let Some(f) = st().pending.as_mut() {
                    f.interval += 5;
                }
                return schedule_poll(flow_id);
            }
            if msg.contains("access_denied") || msg.contains("denied") {
                return fail_flow(flow_id, "The Twitch login was denied.");
            }
            if msg.contains("expired") || msg.contains("invalid device code") {
                return fail_flow(flow_id, "The Twitch login code expired. Start the login again.");
            }
            return fail_flow(flow_id, &format!("Twitch login failed: {}", err.message));
        }
    };
    if !is_pending(flow_id) {
        return;
    }
    if let Err(e) = finish_login(flow_id, &tok).await {
        fail_flow(flow_id, &format!("Twitch login failed: {e}"));
    }
}

async fn finish_login(flow_id: u64, tok: &Value) -> Result<(), String> {
    let client_id = st().pending.as_ref().filter(|f| f.id == flow_id).map(|f| f.client_id.clone()).ok_or("cancelled")?;
    let access = tok["access_token"].as_str().unwrap_or("").to_string();
    let v = request_json(&format!("{ID}/validate"), ReqOpts { headers: vec![("Authorization".into(), format!("OAuth {access}"))], ..Default::default() }).await.map_err(|e| e.message)?;
    let user_id = crate::util::js_string(&v["user_id"]);
    let login = v["login"].as_str().unwrap_or("").to_string();
    let mut name = login.clone();
    if let Ok(u) = request_json(
        &format!("{HELIX}/users?id={}", url::form_urlencoded::byte_serialize(user_id.as_bytes()).collect::<String>()),
        ReqOpts { headers: vec![("Authorization".into(), format!("Bearer {access}")), ("Client-Id".into(), client_id.clone())], ..Default::default() },
    )
    .await
    {
        if let Some(d) = u["data"][0]["display_name"].as_str().filter(|d| !d.is_empty()) {
            name = d.to_string();
        }
    } // the login name is a fine display name
    let scopes: Vec<String> = match (&v["scopes"], &tok["scope"]) {
        (Value::Array(a), _) | (_, Value::Array(a)) => a.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    };
    let expires_in = Some(num(&tok["expires_in"])).filter(|n| *n != 0.0 && !n.is_nan()).or(Some(num(&v["expires_in"])).filter(|n| *n != 0.0 && !n.is_nan())).unwrap_or(3600.0);
    {
        let mut g = st();
        match g.pending.as_mut() {
            Some(f) if f.id == flow_id => f.timer.clear(),
            _ => return Ok(()),
        }
        g.pending = None;
        g.last_error = None;
        g.record = Some(Arc::new(Record {
            access_token: access,
            refresh_token: tok["refresh_token"].as_str().unwrap_or("").to_string(),
            expires_at: now_ms() + (expires_in * 1000.0) as i64,
            scopes,
            client_id,
            user: RecordUser { id: user_id, login, name },
        }));
    }
    persist();
    schedule_refresh();
    start_validate_loop();
    changed();
    Ok(())
}

pub fn cancel() {
    {
        let mut g = st();
        match g.pending.as_mut() {
            Some(f) => f.timer.clear(),
            None => return,
        }
        g.pending = None;
    }
    changed();
}

pub async fn logout() {
    cancel();
    let old = {
        let mut g = st();
        let old = g.record.take();
        g.last_error = None;
        g.refresh_timer.clear();
        g.validate_timer.clear();
        old
    };
    persist();
    changed();
    if let Some(old) = old {
        if read_only() {
            return;
        }
        // Best effort: tell Twitch the token is dead so it cannot be used if the file leaks.
        let _ = request_json(&format!("{ID}/revoke"), ReqOpts { method: "POST", form: Some(vec![("client_id", old.client_id.clone()), ("token", old.access_token.clone())]), timeout_ms: 8000, ..Default::default() }).await;
    }
}

// ---------- refresh / validate ----------

/// Resolves true when the token was refreshed, false when it could not be (the login is cleared if Twitch
/// refused it). Callers share one refresh in flight.
pub fn refresh_now() -> Shared<BoxFuture<'static, bool>> {
    let mut g = st();
    if let Some(f) = &g.refreshing {
        return f.clone();
    }
    let handle = tokio::spawn(async {
        let ok = do_refresh().await;
        st().refreshing = None;
        ok
    });
    let fut = async move { handle.await.unwrap_or(false) }.boxed().shared();
    g.refreshing = Some(fut.clone());
    fut
}

async fn do_refresh() -> bool {
    if read_only() {
        return false;
    }
    let Some(rec) = current() else { return false };
    if rec.refresh_token.is_empty() {
        return false;
    }
    // Public client: client_id only, no secret.
    let tok = request_json(
        &format!("{ID}/token"),
        ReqOpts { method: "POST", form: Some(vec![("client_id", rec.client_id.clone()), ("grant_type", "refresh_token".into()), ("refresh_token", rec.refresh_token.clone())]), ..Default::default() },
    )
    .await;
    match tok {
        Ok(tok) => {
            {
                let mut g = st();
                if !g.record.as_ref().map(|r| Arc::ptr_eq(r, &rec)).unwrap_or(false) {
                    return false; // logged out / replaced meanwhile
                }
                let expires_in = Some(num(&tok["expires_in"])).filter(|n| *n != 0.0 && !n.is_nan()).unwrap_or(3600.0);
                let new_scopes: Vec<String> = tok["scope"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect()).unwrap_or_default();
                g.record = Some(Arc::new(Record {
                    access_token: tok["access_token"].as_str().unwrap_or("").to_string(),
                    refresh_token: tok["refresh_token"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| rec.refresh_token.clone()),
                    expires_at: now_ms() + (expires_in * 1000.0) as i64,
                    scopes: if new_scopes.is_empty() { rec.scopes.clone() } else { new_scopes },
                    ..(*rec).clone()
                }));
            }
            persist();
            schedule_refresh();
            true
        }
        Err(err) => {
            if err.status == 400 || err.status == 401 {
                expire();
            } else {
                log(&format!("token refresh failed {}", err.message));
            }
            false
        }
    }
}

fn expire() {
    {
        let mut g = st();
        if g.record.is_none() {
            return;
        }
        g.record = None;
        g.last_error = Some(EXPIRED_MESSAGE.to_string());
        g.refresh_timer.clear();
        g.validate_timer.clear();
    }
    persist();
    changed();
}

fn schedule_refresh() {
    let mut g = st();
    g.refresh_timer.clear();
    let Some(rec) = g.record.clone() else { return };
    let wait = (rec.expires_at - now_ms() - REFRESH_MARGIN_MS).max(10_000).min(i32::MAX as i64);
    g.refresh_timer.set(Duration::from_millis(wait as u64), async {
        refresh_now().await;
    });
}

fn start_validate_loop() {
    st().validate_timer.every(Duration::from_millis(VALIDATE_EVERY_MS), validate);
}

/// GET /oauth2/validate: required hourly by Twitch. A 401 triggers a refresh; a failed refresh logs the user out.
pub async fn validate() {
    let Some(rec) = current() else { return };
    let v = request_json(&format!("{ID}/validate"), ReqOpts { headers: vec![("Authorization".into(), format!("OAuth {}", rec.access_token))], ..Default::default() }).await;
    match v {
        Ok(v) => {
            if !st().record.as_ref().map(|r| Arc::ptr_eq(r, &rec)).unwrap_or(false) {
                return;
            }
            let scopes: Vec<String> = match v["scopes"].as_array() {
                Some(a) => a.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
                None => rec.scopes.clone(),
            };
            let same = scopes.len() == rec.scopes.len() && scopes.iter().all(|s| rec.scopes.contains(s));
            if !same {
                st().record = Some(Arc::new(Record { scopes, ..(*rec).clone() }));
                persist();
                changed();
            }
        }
        Err(err) => {
            if err.status == 401 {
                refresh_now().await;
            } else {
                log(&format!("validate failed {}", err.message));
            }
        }
    }
}
