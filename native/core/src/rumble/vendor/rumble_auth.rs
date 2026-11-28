//! Port of src/main/rumble/vendor/rumbleAuth.js (render lib/rumbleAuth.js): the Rumble session check and the
//! chat write path (send, moderate, delete).
//!
//! Rumble has no OAuth for third parties and refuses password logins from anything but a browser, so the user
//! signs in on Rumble's own page in a web view (`rumble::login`) and only the `u_s` session cookie comes back.
//! mod.rs persists it in ctx.secrets; it never leaves main.
//!
//! HOSTS: `/service.php` goes to the rumble.com apex (honest UA), `/chat/api/*` to web7; see rumble_hosts.rs.
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
// Transport
// ---------------------------------------------------------------------------

struct Service {
    json: Value,
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
    let text = net::text(r).await?;
    if text.contains("Just a moment") {
        // The one failure mode worth naming explicitly, because the fix is a code change (wrong host).
        return Err(RumbleError::plain("Rumble served a bot challenge — Rumble answered with a bot challenge, so this request cannot be made from the app right now"));
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
    Ok(Service { json })
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
        return Err(RumbleError::plain("Rumble served a bot challenge — Rumble answered with a bot challenge, so this request cannot be made from the app right now"));
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

/// How long a mute lasts, as Rumble's mute menu offers it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MuteKind {
    /// "For 5 minutes" and the like: a mute on this stream that ends after `seconds`
    Timed(u64),
    /// "For this livestream": a mute on this stream with no end. INFERRED: the mod menu's own code sits behind a
    /// moderator login, so this is read from the shape of the two documented forms (video + duration = timeout,
    /// total + no duration = ban); a video mute without a duration is the natural third case.
    Stream,
    /// "Forever": the permanent mute (the ban)
    Forever,
}

fn video_number(video_id: &str) -> String {
    net::f64_value(video_id.parse::<f64>().unwrap_or(f64::NAN)).to_string()
}

fn bare(name: &str) -> String {
    name.strip_prefix('@').unwrap_or(name).to_string()
}

/// The form of `moderation.mute`. `entity` is "user", or "channel" for a message posted as a channel.
pub fn mute_form(video_id: &str, username: &str, entity: &str, kind: MuteKind) -> Vec<(&'static str, String)> {
    let mut data = vec![("user_to_mute", bare(username)), ("entity_type", entity.to_string()), ("video", video_number(video_id))];
    match kind {
        MuteKind::Timed(d) => {
            data.push(("duration", d.max(1).to_string()));
            data.push(("type", "video".to_string()));
        }
        MuteKind::Stream => data.push(("type", "video".to_string())),
        MuteKind::Forever => data.push(("type", "total".to_string())),
    }
    data
}

/// The form of `moderation.unmute`. `scope` is "total" (lifts a permanent mute) or "video" (a mute on this stream).
pub fn unmute_form(video_id: &str, username: &str, entity: &str, scope: &str) -> Vec<(&'static str, String)> {
    vec![("type", scope.to_string()), ("muted_user", bare(username)), ("entity_type", entity.to_string()), ("video", video_number(video_id))]
}

/// The form of `chat.message.pin` / `chat.message.unpin` (the unpin one is seen in Rumble's own client; pin takes
/// the same two fields: INFERRED from the pair).
pub fn pin_form(video_id: &str, message_id: &str) -> Vec<(&'static str, String)> {
    vec![("video_id", video_number(video_id)), ("message_id", message_id.to_string())]
}

async fn mute(session: &str, video_id: &str, username: &str, entity: &str, kind: MuteKind) -> Result<(), RumbleError> {
    service_php("moderation.mute", &mute_form(video_id, username, entity, kind), &[], Some(session)).await.map(|_| ())
}

/// Moderation via service.php (the slash commands live on the apex and answer "Access denied" here).
/// Rumble takes the target by NAME, not id.
pub async fn timeout_user(session: &str, video_id: &str, username: &str, seconds: u64) -> Result<(), RumbleError> {
    mute(session, video_id, username, "user", MuteKind::Timed(seconds)).await
}

pub async fn ban_user(session: &str, video_id: &str, username: &str) -> Result<(), RumbleError> {
    mute(session, video_id, username, "user", MuteKind::Forever).await
}

/// "For this livestream": muted on this broadcast only, until it ends.
pub async fn mute_for_stream(session: &str, video_id: &str, username: &str) -> Result<(), RumbleError> {
    mute(session, video_id, username, "user", MuteKind::Stream).await
}

/// Lifts a mute by NAME. A person can be muted for the stream or forever, and a chat message doesn't say which,
/// so with no `scope` the permanent one is tried first, then the stream one.
pub async fn unmute_user(session: &str, video_id: &str, username: &str, scope: Option<&str>) -> Result<(), RumbleError> {
    let scopes: Vec<&str> = match scope {
        Some("video") => vec!["video"],
        Some("total") => vec!["total"],
        _ => vec!["total", "video"],
    };
    let mut first_err = None;
    for s in scopes {
        match service_php("moderation.unmute", &unmute_form(video_id, username, "user", s), &[], Some(session)).await {
            Ok(_) => return Ok(()),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    Err(first_err.unwrap_or_else(|| RumbleError::plain("Rumble rejected the unmute")))
}

pub async fn pin_message(session: &str, video_id: &str, message_id: &str, pin: bool) -> Result<(), RumbleError> {
    let name = if pin { "chat.message.pin" } else { "chat.message.unpin" };
    service_php(name, &pin_form(video_id, message_id), &[], Some(session)).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_id_and_form() {
        let id = request_id();
        assert_eq!(id.len(), 43);
        assert!(!id.contains('='));
        assert_eq!(form(&[("a", "b c".into()), ("d", "é".into())]), "a=b+c&d=%C3%A9");
    }

    #[test]
    fn mute_forms_match_rumbles_menu() {
        let f = |k| mute_form("12345", "@Bob", "user", k);
        assert_eq!(f(MuteKind::Timed(300)), vec![("user_to_mute", "Bob".into()), ("entity_type", "user".into()), ("video", "12345".into()), ("duration", "300".into()), ("type", "video".into())]);
        assert_eq!(f(MuteKind::Stream), vec![("user_to_mute", "Bob".into()), ("entity_type", "user".into()), ("video", "12345".into()), ("type", "video".into())]);
        assert_eq!(f(MuteKind::Forever), vec![("user_to_mute", "Bob".into()), ("entity_type", "user".into()), ("video", "12345".into()), ("type", "total".into())]);
        assert_eq!(mute_form("1", "x", "channel", MuteKind::Forever)[1], ("entity_type", "channel".into()));
    }

    #[test]
    fn unmute_and_pin_forms() {
        assert_eq!(unmute_form("12345", "Bob", "user", "total"), vec![("type", "total".into()), ("muted_user", "Bob".into()), ("entity_type", "user".into()), ("video", "12345".into())]);
        assert_eq!(unmute_form("12345", "Bob", "user", "video")[0], ("type", "video".into()));
        assert_eq!(pin_form("12345", "m9"), vec![("video_id", "12345".into()), ("message_id", "m9".into())]);
        assert_eq!(form(&pin_form("12345", "m 9")), "video_id=12345&message_id=m+9");
    }
}
