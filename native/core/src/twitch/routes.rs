//! Port of src/main/twitch/routes.js: the Twitch half of the api router: the paths and bodies of
//! render/server.js's chat-page endpoints, answered with the logged-in user's own token and the source's
//! channel. Handlers get a `RouteReq` ({source, method, query, body, ctx}) and return {status, body}.
//!
//! Errors follow the site: 403 "relogin" when the token lacks the scope, 502 "Twitch error: ..." when Twitch
//! refuses; the error text sits in {error}. Logged out is 401 "Log in to Twitch". Badges, emotes and rules
//! never fail for a logged-out user: they answer empty so chat still renders.
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock, Mutex};

use futures_util::future::BoxFuture;
use serde_json::{json, Map, Value};

use super::access::access as get_access;
use super::auth::{self, Record, RecordUser};
use super::channel::normalize_channel;
use super::helix::{helix, helix_all, helix_get, login_required, user_by_login};
use super::http::{request_json, HttpError, ReqOpts};
use super::source::source_for;
use super::vendor::chatters::{suggest_chatters, Pool};
use crate::q;
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};
use crate::util::{js_str_or_empty, js_string, js_to_number, now_ms, slice_utf16, truthy};

const ANNOUNCE_COLORS: [&str; 5] = ["blue", "green", "orange", "purple", "primary"];
const CHAT_SETTINGS_FIELDS: [&str; 7] = ["emote_mode", "follower_mode", "follower_mode_duration", "slow_mode", "slow_mode_wait_time", "subscriber_mode", "unique_chat_mode"];

const SETTINGS_CACHE_MS: i64 = 30_000;
const BADGES_CACHE_MS: i64 = 60 * 60_000;
const EMOTES_CACHE_MS: i64 = 30 * 60_000;

// Which scope /api/mod/action needs per action.
fn mod_action_scope(action: &str) -> Option<&'static str> {
    Some(match action {
        "ban" | "timeout" | "unban" => "moderator:manage:banned_users",
        "warn" => "moderator:manage:warnings",
        "delete" | "pin" | "unpin" | "pinduration" | "clear" => "moderator:manage:chat_messages",
        _ => return None,
    })
}

type Cache = LazyLock<Mutex<HashMap<String, (Value, i64)>>>;
static SETTINGS_CACHE: Cache = LazyLock::new(|| Mutex::new(HashMap::new())); // channelId -> {data, at}
static BADGES_CACHE: Cache = LazyLock::new(|| Mutex::new(HashMap::new())); // login -> {data, at}
static CHEER_CACHE: Cache = LazyLock::new(|| Mutex::new(HashMap::new())); // login -> {data, at}
static EMOTES_CACHE: Cache = LazyLock::new(|| Mutex::new(HashMap::new())); // `${userId}:${channelId}` -> {data, at}
static GLOBAL_BADGES: Mutex<Option<(Value, i64)>> = Mutex::new(None); // {data, at}

fn cache_get(c: &Cache, key: &str, ttl: i64) -> Option<Value> {
    c.lock().unwrap().get(key).filter(|(_, at)| now_ms() - at < ttl).map(|(d, _)| d.clone())
}

fn cache_put(c: &Cache, key: &str, data: Value) {
    c.lock().unwrap().insert(key.to_string(), (data, now_ms()));
}

// ---------- plumbing ----------

/// What a handler can throw.
enum TwErr {
    Http(HttpError),
    /// `err.userMessage`: a 400 with the text
    User(String),
    /// a bug: the router logs it and answers 500 (nothing builds one yet; JS got it from `throw err`)
    #[allow(dead_code)]
    Bug(String),
}

impl From<HttpError> for TwErr {
    fn from(e: HttpError) -> Self {
        TwErr::Http(e)
    }
}

type R = Result<RouteRes, TwErr>;

fn local(status: u16, message: &str) -> TwErr {
    TwErr::Http(HttpError::local(status, message))
}

fn to_result(err: TwErr) -> Result<RouteRes, RouteError> {
    match err {
        TwErr::Http(e) => {
            if e.local {
                return Ok(RouteRes { status: e.status, body: json!({"error": e.message}) });
            }
            if e.status == 401 {
                return Ok(RouteRes { status: 401, body: json!({"error": "Log in to Twitch"}) }); // refused even after a refresh
            }
            Ok(RouteRes { status: 502, body: json!({"error": format!("Twitch error: {}", e.message)}) })
        }
        TwErr::User(m) => Ok(RouteRes { status: 400, body: json!({"error": m}) }),
        TwErr::Bug(m) => Err(RouteError { status: 500, message: m }),
    }
}

fn route<F, Fut>(f: F) -> Handler
where
    F: Fn(RouteReq) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = R> + Send + 'static,
{
    Arc::new(move |req| {
        let fut = f(req);
        let b: BoxFuture<'static, Result<RouteRes, RouteError>> = Box::pin(async move {
            match fut.await {
                Ok(r) => Ok(r),
                Err(e) => to_result(e),
            }
        });
        b
    })
}

fn ok(body: Value) -> R {
    Ok(RouteRes { status: 200, body })
}

fn ok_default() -> R {
    ok(json!({"ok": true}))
}

fn body_of(b: &Value) -> Map<String, Value> {
    b.as_object().cloned().unwrap_or_default()
}

/// `String(b[k] || "")`
fn bstr(b: &Map<String, Value>, k: &str) -> String {
    js_str_or_empty(b.get(k))
}

fn login_of(req: &RouteReq) -> Result<String, TwErr> {
    normalize_channel(&req.source.channel).map_err(TwErr::User)
}

struct Need {
    login: String,
    me: RecordUser,
    channel_id: String,
    rec: Arc<Record>,
}

/// Logged in (401 otherwise), holding `scope` (403 "relogin" otherwise), and the source's channel id resolved.
async fn need(req: &RouteReq, scope: Option<&str>) -> Result<Need, TwErr> {
    let login = login_of(req)?;
    let Some(rec) = auth::current() else { return Err(login_required().into()) };
    if let Some(scope) = scope {
        if !rec.scopes.iter().any(|s| s == scope) {
            return Err(local(403, "relogin"));
        }
    }
    let channel_id = channel_id_of(&login).await?;
    Ok(Need { login, me: rec.user.clone(), channel_id, rec })
}

async fn channel_id_of(login: &str) -> Result<String, TwErr> {
    if let Some(id) = source_for(login).and_then(|s| s.channel_id()) {
        return Ok(id);
    }
    match user_by_login(login).await? {
        Some(u) => Ok(js_string(&u["id"])),
        None => Err(local(404, "channel not found")),
    }
}

async fn resolve_user(login: &str) -> Result<Value, TwErr> {
    match user_by_login(&crate::util::js_trim(login).to_lowercase()).await? {
        Some(u) => Ok(u),
        None => Err(local(404, "user not found")),
    }
}

fn broadcast_to(login: &str, frame: Value) {
    if let Some(s) = source_for(login) {
        s.emit(frame);
    }
}

fn pick_chat_settings(raw: Option<&Value>) -> Value {
    let mut out = Map::new();
    for k in CHAT_SETTINGS_FIELDS {
        out.insert(k.to_string(), raw.and_then(|d| d.get(k)).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

fn map_emote(e: &Value, template: Option<&Value>) -> Option<Value> {
    let id = e.get("id").filter(|v| truthy(v)).map(js_string)?;
    let images = e.get("images").cloned().unwrap_or_else(|| json!({}));
    let first = |ks: &[&str]| ks.iter().find_map(|k| images.get(*k).filter(|v| truthy(v)).map(js_string));
    let mut url = first(&["url_2x", "url_1x", "url_4x"]).unwrap_or_default();
    if url.is_empty() {
        if let Some(template) = template.filter(|t| truthy(t)) {
            let list = |k: &str| e.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
            let pick = |l: &Vec<Value>, prefer: &str, dflt: &str| {
                if l.iter().any(|x| x == prefer) {
                    prefer.to_string()
                } else {
                    l.first().map(js_string).unwrap_or_else(|| dflt.to_string())
                }
            };
            let format = pick(&list("format"), "animated", "static");
            let theme = pick(&list("theme_mode"), "dark", "dark");
            let scales = list("scale");
            let scale = if scales.is_empty() {
                "2.0".to_string()
            } else if scales.iter().any(|x| x == "2.0") {
                "2.0".to_string()
            } else {
                js_string(scales.last().unwrap())
            };
            url = js_string(template).replacen("{{id}}", &id, 1).replacen("{{format}}", &format, 1).replacen("{{theme_mode}}", &theme, 1).replacen("{{scale}}", &scale, 1);
        }
    }
    Some(json!({"id": id, "name": js_str_or_empty(e.get("name")), "url": url}))
}

// ---------- routes ----------

async fn send(req: RouteReq) -> R {
    let n = need(&req, Some("user:write:chat")).await?;
    let b = body_of(&req.body);
    let text = slice_utf16(&bstr(&b, "text"), 500);
    if text.is_empty() {
        return Err(local(400, "empty message"));
    }
    let reply_to = b.get("replyTo").filter(|v| truthy(v)).map(|v| slice_utf16(&js_string(v), 100));
    let mut body = json!({"broadcaster_id": n.channel_id, "sender_id": n.me.id, "message": text});
    if let Some(r) = reply_to {
        body["reply_parent_message_id"] = json!(r);
    }
    let r = helix("/chat/messages", "POST", Vec::new(), Some(body)).await?;
    let d = &r["data"][0];
    if d.is_object() && d["is_sent"] == json!(false) {
        let reason = if truthy(&d["drop_reason"]) { js_string(&d["drop_reason"]["message"]) } else { "unknown".into() };
        return Err(HttpError::new(502, format!("drop: {reason}")).into());
    }
    ok_default()
}

async fn mod_action(req: RouteReq) -> R {
    let b = body_of(&req.body);
    let action = bstr(&b, "action");
    let Some(scope) = mod_action_scope(&action) else { return Err(local(400, "unknown action")) };
    let n = need(&req, Some(scope)).await?;
    let q0 = || q! {"broadcaster_id" => n.channel_id.clone(), "moderator_id" => n.me.id.clone()};

    // ban/timeout/unban/warn accept userLogin as an alternative to userId; an explicit userId wins.
    let mut user_id = b.get("userId").cloned().unwrap_or(Value::Null);
    if ["ban", "timeout", "unban", "warn"].contains(&action.as_str()) && !truthy(&user_id) {
        let l = crate::util::js_trim(&bstr(&b, "userLogin")).to_lowercase();
        if l.is_empty() {
            return Err(local(400, "missing userId or userLogin"));
        }
        user_id = resolve_user(&l).await?["id"].clone();
    }

    match action.as_str() {
        "ban" | "timeout" => {
            let mut data = json!({"user_id": js_string(&user_id)});
            if action == "timeout" {
                let raw = js_to_number(b.get("duration"));
                let secs = if raw.is_finite() { (raw + 0.5).floor() } else { 1.0 };
                data["duration"] = json!(secs.min(1_209_600.0).max(1.0) as i64); // Twitch's two-week cap
            }
            if b.get("reason").map(truthy).unwrap_or(false) {
                data["reason"] = json!(bstr(&b, "reason"));
            }
            helix("/moderation/bans", "POST", q0(), Some(json!({"data": data}))).await?;
        }
        "unban" => {
            let mut qy = q0();
            qy.push(("user_id".into(), user_id.clone()));
            helix("/moderation/bans", "DELETE", qy, None).await?;
        }
        "warn" => {
            let reason = slice_utf16(&bstr(&b, "reason"), 500);
            let reason = if reason.is_empty() { "Warned by moderator".to_string() } else { reason };
            helix("/moderation/warnings", "POST", q0(), Some(json!({"data": {"user_id": js_string(&user_id), "reason": reason}}))).await?;
        }
        "delete" => {
            let mut qy = q0();
            qy.push(("message_id".into(), b.get("messageId").cloned().unwrap_or(Value::Null)));
            helix("/moderation/chat", "DELETE", qy, None).await?;
        }
        "clear" => {
            helix("/moderation/chat", "DELETE", q0(), None).await?; // no message_id wipes every deletable message
        }
        "pin" | "pinduration" | "unpin" => {
            // Twitch takes 30s to 30min; no duration = until unpinned or the stream ends.
            let d = b.get("durationSeconds").cloned().unwrap_or(Value::Null);
            let dn = js_to_number(Some(&d));
            let duration = if d.is_null() || d == json!("") || !dn.is_finite() { Value::Null } else { json!(((dn + 0.5).floor()).clamp(30.0, 1800.0) as i64) };
            let message_id = b.get("messageId").filter(|v| truthy(v)).cloned().unwrap_or(Value::Null);
            let mut pin_q = q0();
            pin_q.push(("message_id".into(), message_id));
            let src = source_for(&n.login);
            if action == "pin" {
                let mut qy = pin_q.clone();
                qy.push(("duration_seconds".into(), duration));
                helix("/chat/pins", "PUT", qy, None).await?;
                // Show it now, then let Twitch's own answer (who pinned it, when it ends) replace the guess.
                if let Some(s) = &src {
                    s.pin_local(&js_str_or_empty(b.get("messageId")), json!({"id": n.me.id, "login": n.me.login, "name": n.me.name}), &d);
                    tokio::spawn(s.sync_pin());
                }
            } else if action == "pinduration" {
                let mut qy = pin_q.clone();
                qy.push(("duration_seconds".into(), duration));
                helix("/chat/pins", "PATCH", qy, None).await?;
                if let Some(s) = &src {
                    s.sync_pin().await;
                }
            } else {
                helix("/chat/pins", "DELETE", pin_q, None).await?;
                if let Some(s) = &src {
                    s.unpin();
                }
            }
        }
        _ => {}
    }
    ok_default()
}

// Capability probe: which mod UI to offer. The site answered from scopes alone; here the user can open ANY
// channel, so mod capabilities also need to be the broadcaster or a moderator there.
async fn caps(req: RouteReq) -> R {
    let login = login_of(&req)?;
    let rec = auth::current();
    let has = |s: &str| rec.as_ref().map(|r| r.scopes.iter().any(|x| x == s)).unwrap_or(false);
    let mut can_mod = true;
    let mut is_broadcaster = false;
    let mut is_mod: Option<bool> = if rec.is_some() { None } else { Some(false) };
    if rec.is_some() {
        // channel unresolvable: scopes alone decide
        if let Ok(cid) = channel_id_of(&login).await {
            let acc = get_access(&cid).await;
            is_broadcaster = acc.is_broadcaster;
            is_mod = acc.is_mod;
            can_mod = acc.is_mod != Some(false); // unknown (null) keeps the buttons: Twitch refuses what is not allowed
        }
    }
    let m = |s: &str| can_mod && has(s);
    ok(json!({
        "pin": m("moderator:manage:chat_messages"),
        "ban": m("moderator:manage:banned_users"),
        "warn": m("moderator:manage:warnings"),
        "delete": m("moderator:manage:chat_messages"),
        "send": has("user:write:chat"),
        "modes": m("moderator:manage:chat_settings"),
        "announce": m("moderator:manage:announcements"),
        "shoutout": m("moderator:manage:shoutouts"),
        "shield": m("moderator:manage:shield_mode"),
        "terms": m("moderator:manage:blocked_terms"),
        "raid": is_broadcaster && has("channel:manage:raids"),
        "automod": m("moderator:manage:automod"),
        // Not in the site's answer: lets the page say why buttons are missing.
        "loggedIn": rec.is_some(),
        "isBroadcaster": is_broadcaster,
        "isMod": is_mod,
    }))
}

async fn mod_user(req: RouteReq) -> R {
    let n = need(&req, None).await?;
    let Some(id) = req.query_get("id").filter(|s| !s.is_empty()).map(String::from) else { return Err(local(400, "missing id")) };
    let page = helix_get("/users", q! {"id" => id.clone()}).await?;
    let u = page["data"].get(0).filter(|u| !u.is_null());
    let user = match u {
        Some(u) => json!({"id": u["id"], "login": u["login"], "displayName": u["display_name"], "avatar": u["profile_image_url"].as_str().unwrap_or(""), "createdAt": u["created_at"]}),
        None => Value::Null,
    };

    // followedAt / ban only answer for a broadcaster or moderator, and need their scopes. A refusal is
    // reported as followError / banError so the card can tell "not following" apart from "could not check".
    let acc = get_access(&n.channel_id).await;
    let allowed = acc.is_broadcaster || acc.is_mod != Some(false);
    let has = |s: &str| n.rec.scopes.iter().any(|x| x == s);
    let mut followed_at = Value::Null;
    let mut follow_error: Option<String> = None;
    if !allowed {
        follow_error = Some("Only moderators can see follow status".into());
    } else if !has("moderator:read:followers") {
        follow_error = Some("missing moderator:read:followers scope".into());
    } else {
        match helix_get("/channels/followers", q! {"broadcaster_id" => n.channel_id.clone(), "user_id" => id.clone()}).await {
            Ok(f) => followed_at = f["data"][0]["followed_at"].as_str().map(|s| json!(s)).unwrap_or(Value::Null),
            Err(err) => follow_error = Some(err.message),
        }
    }
    // Helix answers /moderation/banned only for the channel's own broadcaster (a moderator's token gets a 401),
    // so for everyone else the ban is left out: the card derives it from the bans and timeouts it saw itself.
    let mut ban = None; // None = undefined (omitted)
    let mut ban_error: Option<String> = None;
    if acc.is_broadcaster && has("moderator:manage:banned_users") {
        match helix_get("/moderation/banned", q! {"broadcaster_id" => n.channel_id.clone(), "user_id" => id.clone()}).await {
            Ok(b) => {
                ban = Some(match b["data"].get(0).filter(|r| !r.is_null()) {
                    Some(row) => json!({"permanent": !truthy(&row["expires_at"]), "expiresAt": if truthy(&row["expires_at"]) { row["expires_at"].clone() } else { Value::Null }, "reason": row["reason"].as_str().unwrap_or("")}),
                    None => Value::Null,
                });
            }
            Err(err) => ban_error = Some(err.message),
        }
    }
    let mut out = json!({"user": user, "followedAt": followed_at});
    if let Some(e) = follow_error {
        out["followError"] = json!(e);
    }
    if let Some(b) = ban {
        out["ban"] = b;
    }
    if let Some(e) = ban_error {
        out["banError"] = json!(e);
    }
    ok(out)
}

async fn automod(req: RouteReq) -> R {
    let b = body_of(&req.body);
    let msg_id = bstr(&b, "msgId");
    if msg_id.is_empty() {
        return Err(local(400, "missing msgId"));
    }
    let action = bstr(&b, "action");
    if action != "allow" && action != "deny" {
        return Err(local(400, "unknown action"));
    }
    let n = need(&req, Some("moderator:manage:automod")).await?;
    // user_id must be the token owner's (the acting moderator); the resolving automod.message.update event reaches every client.
    let r = helix("/moderation/automod/message", "POST", Vec::new(), Some(json!({"user_id": n.me.id, "msg_id": msg_id, "action": if action == "allow" { "ALLOW" } else { "DENY" }}))).await;
    if let Err(err) = r {
        if !err.local {
            if err.status == 404 {
                return Err(local(404, "message no longer held"));
            }
            if err.status == 403 {
                return Err(local(403, "not a moderator"));
            }
        }
        return Err(err.into());
    }
    ok_default()
}

async fn get_settings(req: RouteReq) -> R {
    let n = need(&req, None).await?;
    // ?fresh=1 (the Modes panel opening) skips the cache: another mod may have changed a mode since.
    let fresh = req.query_get("fresh").map(|s| !s.is_empty()).unwrap_or(false);
    if !fresh {
        if let Some(d) = cache_get(&SETTINGS_CACHE, &n.channel_id, SETTINGS_CACHE_MS) {
            return ok(d);
        }
    }
    let page = helix_get("/chat/settings", q! {"broadcaster_id" => n.channel_id.clone()}).await?;
    let data = pick_chat_settings(page["data"].get(0));
    cache_put(&SETTINGS_CACHE, &n.channel_id, data.clone());
    ok(data)
}

async fn post_settings(req: RouteReq) -> R {
    let n = need(&req, Some("moderator:manage:chat_settings")).await?;
    let b = body_of(&req.body);
    let mut patch = Map::new();
    for k in CHAT_SETTINGS_FIELDS {
        if let Some(v) = b.get(k) {
            patch.insert(k.to_string(), v.clone());
        }
    }
    let page = helix("/chat/settings", "PATCH", q! {"broadcaster_id" => n.channel_id.clone(), "moderator_id" => n.me.id.clone()}, Some(Value::Object(patch))).await?;
    let settings = pick_chat_settings(page["data"].get(0));
    cache_put(&SETTINGS_CACHE, &n.channel_id, settings.clone());
    broadcast_to(&n.login, json!({"type": "modes", "settings": settings})); // the Modes panel of every open view of this channel follows
    ok(settings)
}

async fn shield(req: RouteReq) -> R {
    let n = need(&req, Some("moderator:manage:shield_mode")).await?;
    let qy = || q! {"broadcaster_id" => n.channel_id.clone(), "moderator_id" => n.me.id.clone()};
    if req.method == "GET" {
        let page = helix_get("/moderation/shield_mode", qy()).await?;
        return ok(page["data"].get(0).filter(|d| !d.is_null()).cloned().unwrap_or_else(|| json!({"is_active": false})));
    }
    let on = truthy(&body_of(&req.body).get("is_active").cloned().unwrap_or(Value::Null));
    let page = helix("/moderation/shield_mode", "PUT", qy(), Some(json!({"is_active": on}))).await?;
    ok(page["data"].get(0).filter(|d| !d.is_null()).cloned().unwrap_or_else(|| json!({"is_active": on})))
}

async fn announce(req: RouteReq) -> R {
    let n = need(&req, Some("moderator:manage:announcements")).await?;
    let b = body_of(&req.body);
    let text = slice_utf16(&bstr(&b, "text"), 500);
    if text.is_empty() {
        return Err(local(400, "empty message"));
    }
    let c = bstr(&b, "color");
    let color = if ANNOUNCE_COLORS.contains(&c.as_str()) { c } else { "primary".to_string() }; // an odd colour is cosmetic, never a 400
    helix("/chat/announcements", "POST", q! {"broadcaster_id" => n.channel_id.clone(), "moderator_id" => n.me.id.clone()}, Some(json!({"message": text, "color": color}))).await?;
    ok_default()
}

async fn shoutout(req: RouteReq) -> R {
    let n = need(&req, Some("moderator:manage:shoutouts")).await?;
    let login = crate::util::js_trim(&bstr(&body_of(&req.body), "login")).to_lowercase();
    if login.is_empty() {
        return Err(local(400, "missing login"));
    }
    let target = resolve_user(&login).await?;
    helix("/chat/shoutouts", "POST", q! {"from_broadcaster_id" => n.channel_id.clone(), "to_broadcaster_id" => target["id"].clone(), "moderator_id" => n.me.id.clone()}, None).await?;
    ok_default()
}

// Twitch lets only the BROADCASTER raid, so this needs the user to be logged in as the channel's owner.
async fn raid(req: RouteReq) -> R {
    let n = need(&req, Some("channel:manage:raids")).await?;
    if n.me.id != n.channel_id {
        return Err(local(403, &format!("Only {0} can raid from their channel. Log in as {0} to use raids.", n.login)));
    }
    if req.method == "POST" {
        let target = crate::util::js_trim(&bstr(&body_of(&req.body), "login")).to_lowercase();
        if target.is_empty() {
            return Err(local(400, "missing login"));
        }
        let to = resolve_user(&target).await?;
        let page = helix("/raids", "POST", q! {"from_broadcaster_id" => n.channel_id.clone(), "to_broadcaster_id" => to["id"].clone()}, None).await?;
        return ok(page["data"].get(0).filter(|d| !d.is_null()).cloned().unwrap_or_else(|| json!({"ok": true})));
    }
    helix("/raids", "DELETE", q! {"broadcaster_id" => n.channel_id.clone()}, None).await?;
    ok_default()
}

async fn terms(req: RouteReq) -> R {
    let n = need(&req, Some("moderator:manage:blocked_terms")).await?;
    let q0 = || q! {"broadcaster_id" => n.channel_id.clone(), "moderator_id" => n.me.id.clone()};
    if req.method == "GET" {
        let mut qy = q0();
        qy.push(("first".into(), json!(100)));
        let page = helix_get("/moderation/blocked_terms", qy).await?;
        let terms: Vec<Value> = page["data"].as_array().cloned().unwrap_or_default().iter().map(|t| json!({"id": t["id"], "text": t["text"]})).collect();
        return ok(json!({"terms": terms}));
    }
    if req.method == "POST" {
        let text = slice_utf16(crate::util::js_trim(&bstr(&body_of(&req.body), "text")), 500);
        if text.is_empty() {
            return Err(local(400, "empty term"));
        }
        let page = helix("/moderation/blocked_terms", "POST", q0(), Some(json!({"text": text}))).await?;
        let t = page["data"].get(0).filter(|t| !t.is_null());
        return ok(json!({"term": t.map(|t| json!({"id": t["id"], "text": t["text"]})).unwrap_or(Value::Null)}));
    }
    let Some(id) = req.query_get("id").filter(|s| !s.is_empty()).map(String::from) else { return Err(local(400, "missing id")) };
    let mut qy = q0();
    qy.push(("id".into(), json!(id)));
    helix("/moderation/blocked_terms", "DELETE", qy, None).await?;
    ok_default()
}

// The site read these from Twitch's web GraphQL with twitch.tv's own client id; this app never borrows
// another app's client id, and Helix has no endpoint for them, so the warn panel just has no rules to cite.
async fn rules(_req: RouteReq) -> R {
    ok(json!({"rules": []}))
}

// Helix answers badges only with a token. Logged out (or when Helix refuses), the public ivr.fi mirror of the
// same Helix data fills in, so mod/sub/VIP badges show for anonymous readers too.
const IVR: &str = "https://api.ivr.fi/v2/twitch/badges";

async fn helix_badges(login: &str) -> Result<Value, HttpError> {
    let channel_id = channel_id_of(login).await.ok();
    let cached = GLOBAL_BADGES.lock().unwrap().clone().filter(|(_, at)| now_ms() - at <= BADGES_CACHE_MS);
    let global = match cached {
        Some((d, _)) => d,
        None => {
            let d = helix_get("/chat/badges/global", Vec::new()).await?["data"].clone();
            let d = if d.is_array() { d } else { json!([]) };
            *GLOBAL_BADGES.lock().unwrap() = Some((d.clone(), now_ms()));
            d
        }
    };
    let channel = match channel_id {
        Some(cid) => {
            let d = helix_get("/chat/badges", q! {"broadcaster_id" => cid}).await?["data"].clone();
            if d.is_array() {
                d
            } else {
                json!([])
            }
        }
        None => json!([]),
    };
    Ok(json!({"global": global, "channel": channel}))
}

async fn ivr_badges(login: &str) -> Result<Value, HttpError> {
    let enc: String = url::form_urlencoded::byte_serialize(login.as_bytes()).collect();
    let (gurl, curl) = (format!("{IVR}/global"), format!("{IVR}/channel?login={enc}"));
    let (global, channel) = tokio::join!(request_json(&gurl, ReqOpts::default()), request_json(&curl, ReqOpts::default()));
    let global = global?;
    let arr = |v: Value| if v.is_array() { v } else { json!([]) };
    Ok(json!({"global": arr(global), "channel": arr(channel.unwrap_or(Value::Null))}))
}

fn has_global(d: &Option<Value>) -> bool {
    d.as_ref().and_then(|d| d["global"].as_array()).map(|a| !a.is_empty()).unwrap_or(false)
}

async fn badges(req: RouteReq) -> R {
    let login = login_of(&req)?;
    if let Some(d) = cache_get(&BADGES_CACHE, &login, BADGES_CACHE_MS) {
        return ok(d);
    }
    let mut data = None;
    if auth::current().is_some() {
        data = helix_badges(&login).await.ok();
    }
    if !has_global(&data) {
        data = ivr_badges(&login).await.ok();
    }
    if !has_global(&data) {
        return ok(json!({"global": [], "channel": []})); // not cached: the next pane retries
    }
    let data = data.unwrap();
    cache_put(&BADGES_CACHE, &login, data.clone());
    ok(data)
}

// Cheermotes (global + the channel's own prefix) for the chat renderer. Read-only, needs no scope but a token;
// logged out there is no keyless source, so the answer is empty and Bits messages stay plain text.
async fn cheermotes(req: RouteReq) -> R {
    let login = login_of(&req)?;
    if let Some(d) = cache_get(&CHEER_CACHE, &login, BADGES_CACHE_MS) {
        return ok(d);
    }
    if auth::current().is_none() {
        return ok(json!({"data": []}));
    }
    let Ok(cid) = channel_id_of(&login).await else { return ok(json!({"data": []})) };
    let Ok(resp) = helix_get("/bits/cheermotes", q! {"broadcaster_id" => cid}).await else { return ok(json!({"data": []})) };
    let data = json!({"data": if resp["data"].is_array() { resp["data"].clone() } else { json!([]) }});
    if data["data"].as_array().map(|a| !a.is_empty()).unwrap_or(false) {
        cache_put(&CHEER_CACHE, &login, data.clone());
    }
    ok(data)
}

// The emote picker: what THIS user can send here. {channel, global} as the site's picker expects, plus
// `other` for emotes from channels the user is subscribed to.
async fn twitch_emotes(req: RouteReq) -> R {
    let login = login_of(&req)?;
    let Some(rec) = auth::current() else { return ok(json!({"channel": [], "global": []})) };
    let channel_id = channel_id_of(&login).await.ok();
    let key = format!("{}:{}", rec.user.id, channel_id.clone().unwrap_or_default());
    if let Some(d) = cache_get(&EMOTES_CACHE, &key, EMOTES_CACHE_MS) {
        return ok(d);
    }
    let mut data = json!({"channel": [], "global": []});
    if rec.scopes.iter().any(|s| s == "user:read:emotes") {
        let qy = q! {"user_id" => rec.user.id.clone(), "broadcaster_id" => channel_id.clone()};
        match helix_all("/chat/emotes/user", qy, 40).await {
            Ok(all) => {
                let mut seen = std::collections::HashSet::new();
                let (mut channel, mut global, mut other) = (Vec::new(), Vec::new(), Vec::new());
                for e in &all.rows {
                    let Some(m) = map_emote(e, all.template.as_ref()) else { continue };
                    let mid = js_string(&m["id"]);
                    if !seen.insert(mid) {
                        continue;
                    }
                    let owner = js_string(&e["owner_id"]);
                    if channel_id.as_deref().map(|c| !c.is_empty() && owner == c).unwrap_or(false) {
                        channel.push(m);
                    } else if owner == "twitch" || e["emote_type"] == "globals" || e["emote_type"] == "smilies" {
                        global.push(m);
                    } else {
                        other.push(m);
                    }
                }
                data = json!({"channel": channel, "global": global, "other": other});
            }
            Err(err) => {
                if err.local {
                    return Err(err.into());
                }
                return ok(data); // best effort: an empty picker section beats an error
            }
        }
    } else {
        // A token from before user:read:emotes: the public lists still work with any token.
        let g = helix_get("/chat/emotes/global", Vec::new()).await;
        let c = match &channel_id {
            Some(cid) => helix_get("/chat/emotes", q! {"broadcaster_id" => cid.clone()}).await,
            None => Ok(Value::Null),
        };
        let map = |r: Result<Value, HttpError>| -> Vec<Value> {
            match r {
                Ok(v) if truthy(&v) => v["data"].as_array().cloned().unwrap_or_default().iter().filter_map(|e| map_emote(e, v.get("template"))).collect(),
                _ => Vec::new(),
            }
        };
        data = json!({"channel": map(c), "global": map(g)});
    }
    cache_put(&EMOTES_CACHE, &key, data.clone());
    ok(data)
}

// Name suggestions (for the hidden-chatters list), from what this chat said recently.
async fn chatters(req: RouteReq) -> R {
    let login = login_of(&req)?;
    let q = slice_utf16(req.query_get("q").unwrap_or(""), 60);
    let messages: Vec<Value> = source_for(&login).map(|s| s.backlog()).unwrap_or_default().into_iter().filter(|e| e.get("notice").is_none()).collect();
    ok(json!({"results": suggest_chatters(&q, &[Pool { platform: "twitch", messages: &messages }], 8)}))
}

pub fn clear_caches() {
    SETTINGS_CACHE.lock().unwrap().clear();
    BADGES_CACHE.lock().unwrap().clear();
    CHEER_CACHE.lock().unwrap().clear();
    EMOTES_CACHE.lock().unwrap().clear();
    *GLOBAL_BADGES.lock().unwrap() = None;
}

pub fn routes() -> Vec<(String, Handler)> {
    vec![
        ("POST /api/chat/send".into(), route(send)),
        ("POST /api/mod/action".into(), route(mod_action)),
        ("GET /api/mod/caps".into(), route(caps)),
        ("GET /api/mod/user".into(), route(mod_user)),
        ("POST /api/mod/automod".into(), route(automod)),
        ("GET /api/chat/settings".into(), route(get_settings)),
        ("POST /api/chat/settings".into(), route(post_settings)),
        ("GET /api/chat/shield".into(), route(shield)),
        ("PUT /api/chat/shield".into(), route(shield)),
        ("POST /api/chat/announce".into(), route(announce)),
        ("POST /api/chat/shoutout".into(), route(shoutout)),
        ("POST /api/chat/raid".into(), route(raid)),
        ("DELETE /api/chat/raid".into(), route(raid)),
        ("GET /api/chat/terms".into(), route(terms)),
        ("POST /api/chat/terms".into(), route(terms)),
        ("DELETE /api/chat/terms".into(), route(terms)),
        ("GET /api/chat/rules".into(), route(rules)),
        ("GET /api/chat/badges".into(), route(badges)),
        ("GET /api/chat/cheermotes".into(), route(cheermotes)),
        ("GET /api/chat/twitch-emotes".into(), route(twitch_emotes)),
        ("GET /api/chatters".into(), route(chatters)),
    ]
}
