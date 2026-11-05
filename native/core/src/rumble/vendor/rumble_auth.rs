//! Port of src/main/rumble/vendor/rumbleAuth.js (render lib/rumbleAuth.js): Rumble account auth and the chat
//! write path (send, moderate, delete).
//!
//! Rumble has no OAuth for third parties: the only way to act as a user is to hold a Rumble session, and the
//! only way to get one is to log in as them, as Rumble's own web client does: fetch the account's salts, hash
//! the password client-side (the plaintext is never sent, and never stored), and exchange that for a session
//! token (`u_s`). The user does this themselves in Settings; nothing here is ever run without that.
//!
//! The password exists only for the duration of one `login()` call: hashed, used, dropped, never stored or
//! logged. The session token is persisted by mod.rs in ctx.secrets and never leaves main.
//!
//! USE A FRONT-END HOST (wn0, web7), NOT THE APEX; see rumble_hosts.rs. Since 2026-10-27 only wn0 serves
//! service.php; web7 serves just /chat/api/*.
use base64::Engine;
use rand::RngCore;
use serde_json::{json, Value};

use super::rumble_hosts::fetch_from_rumble;
use crate::net::{self, FetchOpts};
use crate::util::{js_str_or_empty, js_string, truthy};

const FETCH_TIMEOUT_MS: u64 = 20_000;
const SESSION_COOKIE: &str = "u_s";

/// What the JS threw: an Error with `.message` and the flags `rumbleRefused` / `rumbleExpired` / `rumbleStatus`.
#[derive(Clone, Debug, Default)]
pub struct RumbleError {
    pub message: String,
    /// a refusal Rumble deliberately returned (answered 403 by the routes, not 502)
    pub refused: bool,
    /// the saved session expired
    pub expired: bool,
    pub status: Option<u16>,
}

impl RumbleError {
    pub fn plain(m: impl Into<String>) -> Self {
        RumbleError { message: m.into(), ..Default::default() }
    }
}

impl std::fmt::Display for RumbleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<net::FetchError> for RumbleError {
    fn from(e: net::FetchError) -> Self {
        RumbleError::plain(e.message)
    }
}

// ---------------------------------------------------------------------------
// Password hashing: salted, stretched MD5. Not ours to choose: the server compares against exactly this.
// ---------------------------------------------------------------------------

fn md5(message: &str) -> String {
    format!("{:x}", md5::compute(message.as_bytes()))
}

/// md5(salt + password), then `iterations` more rounds of md5(previous + password).
fn stretch(password: &str, salt: &str, iterations: u32) -> String {
    let mut current = md5(&format!("{salt}{password}"));
    for _ in 0..iterations {
        current = md5(&format!("{current}{password}"));
    }
    current
}

/// Rumble hands out three salts and expects three comma-joined values back: the first salt's stretched hash
/// re-hashed with the second salt, the third salt's stretched hash, and the second salt echoed verbatim.
pub fn password_hashes(password: &str, salts: &[String]) -> Result<String, RumbleError> {
    if salts.len() < 3 {
        return Err(RumbleError::plain("rumble: expected three salts"));
    }
    let first = md5(&format!("{}{}", stretch(password, &salts[0], 128), salts[1]));
    let second = stretch(password, &salts[2], 128);
    Ok(format!("{first},{second},{}", salts[1]))
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

struct Service {
    json: Value,
    set_cookies: Vec<String>,
}

fn form(pairs: &[(&str, String)]) -> String {
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().map(|(k, v)| (*k, v.as_str()))).finish()
}

fn cookie(session: &str) -> (String, String) {
    ("Cookie".to_string(), format!("{SESSION_COOKIE}={session}"))
}

/// One service.php call. `session` is the raw u_s token, or None for the unauthenticated steps of login.
///
/// Rumble answers 200 with an `errors` array as often as it uses a real status code, so both are folded into
/// one error carrying Rumble's own message: that text is what the settings UI shows the user ("invalid
/// username or password", "too many attempts").
async fn service_php(name: &str, data: &[(&str, String)], params: &[(&str, &str)], session: Option<&str>) -> Result<Service, RumbleError> {
    let mut q = vec![("name", name.to_string())];
    q.extend(params.iter().map(|(k, v)| (*k, v.to_string())));
    let mut opts = FetchOpts::new(FETCH_TIMEOUT_MS).method("POST").header("Content-Type", "application/x-www-form-urlencoded").header("Accept", "application/json").body(form(data));
    opts.manual_redirect = true;
    if let Some(s) = session {
        let (k, v) = cookie(s);
        opts.headers.push((k, v));
    }
    let r = fetch_from_rumble(&format!("/service.php?{}", form(&q)), opts).await?;
    let status = r.status().as_u16();
    let ok = r.status().is_success();
    let set_cookies: Vec<String> = r.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).map(String::from).collect();
    let text = net::text(r).await?;
    if text.contains("Just a moment") {
        // The one failure mode worth naming explicitly, because the fix is a code change (wrong host).
        return Err(RumbleError::plain("Rumble served a bot challenge — this request must use a Rumble front-end host (wn0), not rumble.com"));
    }
    let json: Value = serde_json::from_str(&text).map_err(|_| RumbleError::plain(format!("Rumble returned a non-JSON response to {name} (HTTP {status})")))?;

    // Flagged so moderation can answer a 4xx the user reads, not a gateway failure.
    if let Some(err) = json["errors"].as_array().and_then(|a| a.first()) {
        let m = js_str_or_empty(Some(&err["message"]));
        return Err(RumbleError {
            message: if m.is_empty() { format!("Rumble rejected {name}") } else { m },
            refused: true,
            expired: json["user"]["logged_in"] == Value::Bool(false),
            status: Some(status),
        });
    }
    if !ok {
        return Err(RumbleError { message: format!("Rumble {name} failed (HTTP {status})"), refused: (400..500).contains(&status), expired: false, status: Some(status) });
    }
    Ok(Service { json, set_cookies })
}

fn session_from_set_cookie(lines: &[String]) -> Option<String> {
    let prefix = format!("{SESSION_COOKIE}=");
    for line in lines {
        for part in line.split(';') {
            let part = part.trim_start();
            if let Some(v) = part.strip_prefix(&prefix) {
                let v = v.trim();
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

pub async fn get_salts(username: &str) -> Result<Vec<String>, RumbleError> {
    let s = service_php("user.get_salts", &[("username", username.to_string())], &[("response_type", "session")], None).await?;
    let salts: Vec<String> = s.json["data"]["salts"].as_array().map(|a| a.iter().map(js_string).collect()).unwrap_or_default();
    if salts.len() < 3 {
        return Err(RumbleError::plain("Rumble did not return login salts"));
    }
    Ok(salts)
}

/// A 2FA challenge to continue with.
#[derive(Clone, Debug)]
pub struct TwoFactor {
    pub totp_id: String,
    pub user_key: String,
}

pub enum LoginResult {
    Session(String),
    TwoFactor(TwoFactor),
}

/// Logs in, returning either a session or a 2FA challenge to continue with. `password` is consumed here and
/// nowhere else: it is turned into hashes immediately and never leaves this function.
pub async fn login(username: &str, password: &str) -> Result<LoginResult, RumbleError> {
    let salts = get_salts(username).await?;
    let hashes = password_hashes(password, &salts)?;

    // Rumble decides whether 2FA applies in a dedicated first step, and this call is also what rejects a
    // wrong password: so it runs before user.login even for accounts without 2FA.
    let first = service_php(
        "user.2fa.first_step",
        &[("legacy_password", "1".into()), ("login", username.to_string()), ("password", hashes.clone()), ("redirect_url", "https://rumble.com".into())],
        &[],
        None,
    )
    .await?;
    let info = &first.json["data"];
    if truthy(&info["totp_id"]) {
        return Ok(LoginResult::TwoFactor(TwoFactor { totp_id: js_string(&info["totp_id"]), user_key: js_str_or_empty(Some(&info["user_key"])) }));
    }

    let s = service_php("user.login", &[("username", username.to_string()), ("password_hashes", hashes)], &[], None).await?;
    let session = &s.json["data"]["session"];
    if !truthy(session) {
        return Err(RumbleError::plain("Rumble accepted the login but returned no session"));
    }
    Ok(LoginResult::Session(js_string(session)))
}

/// Second half of a 2FA login. Unlike user.login, this one delivers the session as a Set-Cookie header.
pub async fn complete_two_factor(t: &TwoFactor, code: &str) -> Result<String, RumbleError> {
    let s = service_php(
        "user.2fa.verify_totp",
        &[("code", code.to_string()), ("totp_id", t.totp_id.clone()), ("user_key", t.user_key.clone()), ("redirect_url", "https://rumble.com".into())],
        &[],
        None,
    )
    .await?;
    session_from_set_cookie(&s.set_cookies).ok_or_else(|| RumbleError::plain("Rumble accepted the code but returned no session"))
}

pub struct Who {
    pub id: String,
    pub username: String,
}

/// Cheap read-only "is this session still good, and who is it". None for an expired or revoked session.
pub async fn whoami(session: &str) -> Option<Who> {
    if session.is_empty() {
        return None;
    }
    let s = service_php("user.has_unread_notifications", &[], &[], Some(session)).await.ok()?;
    let user = &s.json["user"];
    if !truthy(&user["logged_in"]) || !truthy(&user["id"]) {
        return None;
    }
    Some(Who { id: js_string(&user["id"]), username: js_str_or_empty(Some(&user["username"])) })
}

pub async fn logout(session: &str) {
    if session.is_empty() {
        return;
    }
    // Best effort. A session we are discarding locally does not need Rumble's agreement.
    let _ = service_php("user.logout", &[], &[], Some(session)).await;
}

// ---------------------------------------------------------------------------
// Chat write path: the chat app's own routes (the ones rumble.com's player calls), they take JSON.
// ---------------------------------------------------------------------------

async fn chat_api(path: &str, method: &'static str, body: Option<Value>, session: &str, referer: Option<&str>) -> Result<Value, RumbleError> {
    let mut opts = FetchOpts::new(FETCH_TIMEOUT_MS).method(method).header("Accept", "application/json");
    opts.manual_redirect = true;
    if body.is_some() {
        opts = opts.header("Content-Type", "application/json");
    }
    let (ck, cv) = cookie(session);
    opts = opts.header(&ck, &cv);
    // Rumble's own client only ever calls these from a page on rumble.com, so every real request carries an
    // Origin and a Referer pointing at the video. Reads are not CSRF-protected; writes are.
    opts = opts.header("Origin", "https://rumble.com").header("Referer", referer.filter(|r| !r.is_empty()).unwrap_or("https://rumble.com/"));
    if let Some(b) = body {
        opts = opts.body(b.to_string());
    }
    let r = fetch_from_rumble(path, opts).await?;
    let status = r.status().as_u16();
    let ok = r.status().is_success();
    let text = net::text(r).await?;
    if text.contains("Just a moment") {
        return Err(RumbleError::plain("Rumble served a bot challenge — this request must use a Rumble front-end host (wn0), not rumble.com"));
    }
    let json: Value = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).map_err(|_| RumbleError::plain(format!("Rumble returned a non-JSON response (HTTP {status})")))?
    };
    // 403 with `logged_in: false` is specifically "your saved Rumble session expired", which the UI must be
    // able to tell apart from a refused action so it can prompt for a re-connect.
    if json["user"]["logged_in"] == Value::Bool(false) {
        return Err(RumbleError { message: "Your Rumble session has expired — reconnect your account in Settings".into(), expired: true, ..Default::default() });
    }
    if let Some(err) = json["errors"].as_array().and_then(|a| a.first()) {
        let m = js_str_or_empty(Some(&err["message"]));
        return Err(RumbleError { message: if m.is_empty() { "Rumble rejected the request".into() } else { m }, refused: true, expired: false, status: Some(status) });
    }
    if !ok {
        return Err(RumbleError { message: format!("Rumble request failed (HTTP {status})"), refused: (400..500).contains(&status), expired: false, status: Some(status) });
    }
    Ok(json)
}

/// Every write to the chat API is wrapped in a `data` envelope. `request_id` is a client-generated id it
/// expects on a send; 32 random bytes, base64, padding stripped, capped at 43 characters.
fn request_id() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    let s = base64::engine::general_purpose::STANDARD.encode(b);
    s.trim_end_matches('=').chars().take(43).collect()
}

fn enc(s: &str) -> String {
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

/// Sends a chat message (the chat API's own message route on the front-end host).
pub async fn send_message(session: &str, chat_id: &str, message: &str, referer: Option<&str>, channel_id: Option<&str>) -> Result<Value, RumbleError> {
    chat_api(
        &format!("/chat/api/chat/{}/message", enc(chat_id)),
        "POST",
        Some(json!({"data": {
            "request_id": request_id(),
            "message": {"text": message},
            // Present and null on purpose: Rumble validates the envelope's shape, not just its contents.
            "rant": null,
            "channel_id": channel_id.and_then(|c| c.parse::<f64>().ok()).map(net::f64_value).unwrap_or(Value::Null),
        }})),
        session,
        referer,
    )
    .await
}

/// Deletes one message.
pub async fn delete_message(session: &str, chat_id: &str, message_id: &str) -> Result<Value, RumbleError> {
    chat_api(&format!("/chat/api/chat/{}/message/{}", enc(chat_id), enc(message_id)), "DELETE", None, session, None).await
}

fn mute(session: &str, video_id: &str, username: &str, duration: Option<u64>) -> impl std::future::Future<Output = Result<Service, RumbleError>> {
    let user = username.strip_prefix('@').unwrap_or(username).to_string();
    let mut data = vec![("user_to_mute", user), ("entity_type", "user".to_string()), ("video", net::f64_value(video_id.parse::<f64>().unwrap_or(f64::NAN)).to_string())];
    // A "video" mute is a timeout on this stream; a "total" mute with no end is the permanent ban.
    match duration {
        Some(d) => {
            data.push(("duration", d.max(1).to_string()));
            data.push(("type", "video".to_string()));
        }
        None => data.push(("type", "total".to_string())),
    }
    let session = session.to_string();
    async move { service_php("moderation.mute", &data, &[], Some(&session)).await }
}

/// Moderation via service.php (the slash commands live on the apex and answer "Access denied" here).
/// Rumble takes the target by NAME, not id.
pub async fn timeout_user(session: &str, video_id: &str, username: &str, seconds: u64) -> Result<(), RumbleError> {
    mute(session, video_id, username, Some(seconds)).await.map(|_| ())
}

pub async fn ban_user(session: &str, video_id: &str, username: &str) -> Result<(), RumbleError> {
    mute(session, video_id, username, None).await.map(|_| ())
}

/// Undoing a mute needs the id of the mute RECORD, which chat messages don't carry: say so.
pub async fn unban_user() -> Result<(), RumbleError> {
    Err(RumbleError { message: "Unbanning has to be done on Rumble — it needs the mute record, which chat messages don't carry".into(), refused: true, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_and_hash_shape() {
        assert_eq!(md5("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(stretch("pw", "s", 0), md5("spw"));
        assert_eq!(stretch("pw", "s", 1), md5(&format!("{}pw", md5("spw"))));
        let salts: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        let h = password_hashes("pw", &salts).unwrap();
        let parts: Vec<&str> = h.split(',').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], md5(&format!("{}b", stretch("pw", "a", 128))));
        assert_eq!(parts[1], stretch("pw", "c", 128));
        assert_eq!(parts[2], "b");
        assert!(password_hashes("pw", &salts[..2]).is_err());
    }

    #[test]
    fn set_cookie_and_request_id() {
        let c = vec!["foo=1; Path=/".to_string(), "u_s=abc123; Path=/; HttpOnly".to_string()];
        assert_eq!(session_from_set_cookie(&c), Some("abc123".into()));
        assert_eq!(session_from_set_cookie(&["u_s=; Path=/".to_string()]), None);
        assert_eq!(session_from_set_cookie(&[]), None);
        let id = request_id();
        assert_eq!(id.len(), 43);
        assert!(!id.contains('='));
        assert_eq!(form(&[("a", "b c".into()), ("d", "é".into())]), "a=b+c&d=%C3%A9");
    }
}
