//! The twitch.tv web session: what the user card needs to show Twitch's own viewer card data (badges, follow and
//! subscription, warnings / timeouts / bans, shared mod comments), the way the 7TV browser extension gets it:
//! Twitch's web GraphQL endpoint (gql.twitch.tv) called with the `auth-token` cookie of a twitch.tv sign-in.
//!
//! It is stored apart from the OAuth login (`auth.rs`, Helix) but the user gets both from one "Log in"
//! (`signin.rs`).
//!
//! LOGIN. The app (gpui crate) opens a window with twitch.tv's own login page and hands back the `auth-token`
//! cookie through the login hook (`set_login_hook`), in memory only. `store` verifies it with `currentUser { id
//! login }` before keeping it, in the encrypted secrets store under `twitch_web`. `AGU_AUTH_READONLY=1` never
//! writes or deletes it.
//!
//! CALLS. Plain JSON POSTs with raw query text, `Client-Id` of the twitch.tv web page and `Authorization: OAuth
//! <token>`, and the app's normal User-Agent. Nothing here builds an integrity token or imitates a browser: when
//! Twitch says an operation needs its integrity check, the feature reports that and stops (`Gql::Integrity`).
//! Only a 401 marks the session expired.
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::auth;
use super::rt::{ctx, user_agent};
use crate::relay::{Handler, RouteError, RouteReq, RouteRes};
use crate::util::Listeners;

const SECRET_KEY: &str = "twitch_web";
const GQL_URL: &str = "https://gql.twitch.tv/gql";
/// The public client id twitch.tv's own web page uses.
const WEB_CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
pub const EXPIRED_MESSAGE: &str = "Your twitch.tv session expired, log in to Twitch again";
pub const INTEGRITY_MESSAGE: &str = "Twitch asks for its own browser check before it shows this, so it is not available here.";
pub const COMMENT_MAX_CHARS: usize = 500;

/// "change": sign-in / sign-out / expiry
pub static EVENTS: Listeners<()> = Listeners::new();

fn read_only() -> bool {
    std::env::var("AGU_AUTH_READONLY").map(|v| !v.is_empty() && v != "0").unwrap_or(false)
}

// ---------------------------------------------------------------------------------------------------------
// Queries (the operations 7TV's user card runs, trimmed to what the card shows)
// ---------------------------------------------------------------------------------------------------------

const BADGE_FIELDS: &str = "id setID version title description image1x: imageURL(size: NORMAL) image2x: imageURL(size: DOUBLE)";

fn viewer_card_query() -> String {
    format!(
        "query ViewerCard($channelID: ID!, $channelIDStr: String!, $targetLogin: String!, $channelLogin: String!) {{
  targetUser: user(login: $targetLogin, lookupType: ALL) {{
    id login displayName chatColor createdAt profileImageURL(width: 70)
    displayBadges(channelID: $channelID) {{ {BADGE_FIELDS} }}
    relationship(targetUserID: $channelID) {{
      cumulativeTenure: subscriptionTenure(tenureMethod: CUMULATIVE) {{ months daysRemaining }}
      followedAt
      subscriptionBenefit {{ id tier purchasedWithPrime gift {{ isGift }} }}
    }}
    isModerator(channelID: $channelIDStr)
  }}
  currentUser {{ login id }}
  channelViewer(userLogin: $targetLogin, channelLogin: $channelLogin) {{ id earnedBadges {{ {BADGE_FIELDS} }} }}
  channel(id: $channelID) {{ id moderationSettings {{ canAccessViewerCardModLogs }} }}
}}"
    )
}

const MOD_LOGS_QUERY: &str = "query ViewerCardModLogs($channelID: ID!, $targetID: ID!) {
  banStatus: chatRoomBanStatus(channelID: $channelID, userID: $targetID) {
    createdAt expiresAt isPermanent reason
    moderator { id login displayName }
  }
  viewerCardModLogs(channelID: $channelID, targetID: $targetID) {
    messages: messages(first: 1000) {
      ... on ViewerCardModLogsMessagesError { code }
      ... on ViewerCardModLogsMessagesConnection { pageInfo { hasNextPage } count }
    }
    bans: targetedActions(first: 99, type: BAN) { ...actions }
    timeouts: targetedActions(first: 99, type: TIMEOUT) { ...actions }
    unbans: targetedActions(first: 99, type: UNBAN) { ...actions }
    untimeouts: targetedActions(first: 99, type: UNTIMEOUT) { ...actions }
    warnings: targetedActions(first: 99, type: WARN) { ...actions }
    comments(first: 100) {
      ... on ModLogsCommentConnection {
        edges { cursor node { id timestamp text author { id login displayName } } }
        pageInfo { hasNextPage }
      }
      ... on ModLogsCommentsError { code }
      __typename
    }
  }
}
fragment actions on ModLogsTargetedActionsResult {
  __typename
  ... on ModLogsTargetedActionsError { code }
  ... on ModLogsTargetedActionsConnection {
    count
    pageInfo { hasNextPage }
    edges { cursor node { id localizedLabel { fallbackString } timestamp type } }
  }
}";

const MESSAGES_QUERY: &str = "query ViewerCardModLogsMessagesBySender($channelID: ID!, $senderID: ID!, $cursor: Cursor) {
  logs: viewerCardModLogs(channelID: $channelID, targetID: $senderID) {
    messages(first: 50, after: $cursor) {
      ... on ViewerCardModLogsMessagesError { code }
      ... on ViewerCardModLogsMessagesConnection {
        edges { cursor node { __typename
          ... on ViewerCardModLogsChatMessage { id sentAt content { text } }
          ... on ViewerCardModLogsCaughtMessage { id status category sentAt content { text } }
          ... on ViewerCardModLogsModActionsMessage { timestamp content { fallbackString } }
        } }
        pageInfo { hasNextPage }
      }
    }
  }
}";

const CREATE_COMMENT: &str = "mutation createModComment($input: CreateModeratorCommentInput!) {
  createModeratorComment(input: $input) { comment { id timestamp text author { id login displayName } } }
}";

const DELETE_COMMENT: &str = "mutation deleteModeratorComment($input: DeleteModeratorCommentInput!) {
  deleteModeratorComment(input: $input) { comment { id } }
}";

const VERIFY_QUERY: &str = "query VerifyLogin { currentUser { id login displayName } }";

/// The body of one GQL call (raw query text, no persisted hashes).
pub fn gql_body(operation: &str, query: &str, variables: Value) -> Value {
    json!({"operationName": operation, "query": query, "variables": variables})
}

// ---------------------------------------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum GqlError {
    /// Twitch no longer accepts the session (HTTP 401)
    Unauthorized,
    /// Twitch wants its own integrity check for the operation
    Integrity,
    /// anything else, with Twitch's text
    Other(String),
}

impl GqlError {
    pub fn message(&self) -> String {
        match self {
            GqlError::Unauthorized => EXPIRED_MESSAGE.into(),
            GqlError::Integrity => INTEGRITY_MESSAGE.into(),
            GqlError::Other(m) => m.clone(),
        }
    }
}

/// HTTP status + JSON body of a gql.twitch.tv answer -> its `data`, or why not.
///
/// Twitch answers what it can: a field it refuses (integrity, say) comes back null with an entry in `errors`, the
/// rest still arrives. Such a partial answer is `Ok`, with the refusals kept under `__errors` for `field_error`.
pub fn parse_gql(status: u16, body: &Value) -> Result<Value, GqlError> {
    if status == 401 {
        return Err(GqlError::Unauthorized);
    }
    if (200..300).contains(&status) {
        if let Some(d) = body.get("data").and_then(|d| d.as_object()).filter(|o| o.values().any(|v| !v.is_null())) {
            let mut d = Value::Object(d.clone());
            if let Some(errs) = body["errors"].as_array().filter(|a| !a.is_empty()) {
                d["__errors"] = Value::Array(errs.clone());
            }
            return Ok(d);
        }
    }
    let first_error = |b: &Value| -> Option<String> {
        if let Some(a) = b.as_array() {
            return a.iter().find_map(|e| first_error_of(e));
        }
        first_error_of(b)
    };
    if let Some(m) = first_error(body) {
        if m.to_lowercase().contains("integrity") {
            return Err(GqlError::Integrity);
        }
        return Err(GqlError::Other(m));
    }
    if !(200..300).contains(&status) {
        let m = body["message"].as_str().or_else(|| body["error"].as_str()).map(String::from).unwrap_or_else(|| format!("Twitch answered {status}"));
        if m.to_lowercase().contains("integrity") {
            return Err(GqlError::Integrity);
        }
        return Err(GqlError::Other(m));
    }
    match body.get("data") {
        Some(d) if !d.is_null() => Ok(d.clone()),
        _ => Err(GqlError::Other("Twitch sent no data".into())),
    }
}

/// Why the top-level `field` of a partial answer is missing (None when it is there).
pub fn field_error(data: &Value, field: &str) -> Option<GqlError> {
    if !data[field].is_null() {
        return None;
    }
    let errs = data["__errors"].as_array().cloned().unwrap_or_default();
    let mine = errs.iter().find(|e| e["path"].get(0).and_then(|p| p.as_str()) == Some(field));
    let Some(e) = mine.or(errs.first()) else { return Some(GqlError::Other("Twitch sent nothing for this".into())) };
    let code = e.pointer("/extensions/code").and_then(|c| c.as_str()).unwrap_or("");
    let msg = e["message"].as_str().unwrap_or("").to_string();
    if code == "IntegrityCheckFailed" || msg.to_lowercase().contains("integrity") {
        Some(GqlError::Integrity)
    } else if code == "Unauthenticated" {
        Some(GqlError::Unauthorized)
    } else {
        Some(GqlError::Other(if msg.is_empty() { "Twitch refused this".into() } else { msg }))
    }
}

fn first_error_of(b: &Value) -> Option<String> {
    if let Some(e) = b["errors"].as_array().and_then(|a| a.iter().find_map(|e| e["message"].as_str().filter(|m| !m.is_empty()))) {
        return Some(e.to_string());
    }
    // the shape of a refusal without `errors`: {"error": "...", "message": "failed integrity check"}
    if b["error"].is_string() {
        return b["message"].as_str().or_else(|| b["error"].as_str()).map(String::from);
    }
    None
}

fn s(v: &Value, p: &str) -> String {
    v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn badge_json(b: &Value) -> Value {
    let url = b["image2x"].as_str().filter(|u| !u.is_empty()).or_else(|| b["image1x"].as_str()).unwrap_or("");
    json!({"id": b["id"], "setId": b["setID"], "version": b["version"], "title": b["title"].as_str().unwrap_or(""), "description": b["description"].as_str().unwrap_or(""), "url": url})
}

/// "1000" / "2000" / "3000" -> 1 / 2 / 3
pub fn tier_number(tier: &str) -> u8 {
    match tier.trim().chars().next() {
        Some('2') => 2,
        Some('3') => 3,
        _ => 1,
    }
}

/// The badges for the card: the ones Twitch shows next to the name in this channel first, then the other earned
/// ones, each once.
pub fn merge_badges(displayed: &[Value], earned: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for b in displayed.iter().chain(earned.iter()) {
        let key = (s(b, "/setID"), s(b, "/version"));
        if !out.iter().any(|o| (s(o, "/setId"), s(o, "/version")) == key) {
            out.push(badge_json(b));
        }
    }
    out
}

/// `ViewerCard` data -> the card's account part.
pub fn parse_viewer_card(data: &Value) -> Value {
    let t = &data["targetUser"];
    if t.is_null() {
        return json!({"user": Value::Null});
    }
    let arr = |p: &str| data.pointer(p).and_then(|x| x.as_array()).cloned().unwrap_or_default();
    let displayed = t["displayBadges"].as_array().cloned().unwrap_or_default();
    let earned = arr("/channelViewer/earnedBadges");
    let rel = &t["relationship"];
    let sub = match rel.get("subscriptionBenefit").filter(|b| b.is_object()) {
        Some(b) => {
            let months = rel.pointer("/cumulativeTenure/months").and_then(|m| m.as_u64());
            Some(json!({"tier": tier_number(b["tier"].as_str().unwrap_or("")), "months": months, "gift": b.pointer("/gift/isGift").and_then(|g| g.as_bool()).unwrap_or(false), "prime": b["purchasedWithPrime"].as_bool().unwrap_or(false)}))
        }
        None => None,
    };
    json!({
        "user": {"id": t["id"], "login": t["login"].as_str().unwrap_or(""), "displayName": t["displayName"].as_str().unwrap_or(""), "avatar": t["profileImageURL"].as_str().unwrap_or(""), "createdAt": t["createdAt"].as_str().unwrap_or("")},
        "followedAt": rel["followedAt"],
        "sub": sub,
        "isModerator": t["isModerator"].as_bool().unwrap_or(false),
        "badges": merge_badges(&displayed, &earned),
        "canAccessModLogs": data.pointer("/channel/moderationSettings/canAccessViewerCardModLogs").and_then(|x| x.as_bool()).unwrap_or(false),
        "me": {"id": s(data, "/currentUser/id"), "login": s(data, "/currentUser/login")},
    })
}

/// "1000+" when Twitch has more than it counted, the plain number otherwise.
pub fn count_label(count: u64, has_more: bool) -> String {
    if has_more { format!("{count}+") } else { count.to_string() }
}

fn actions_json(v: &Value) -> Value {
    // an error result (no access, ...) shows as nothing
    let edges = v["edges"].as_array().cloned().unwrap_or_default();
    let items: Vec<Value> = edges
        .iter()
        .map(|e| {
            let n = &e["node"];
            json!({"id": n["id"], "ts": n["timestamp"].as_str().unwrap_or(""), "text": n.pointer("/localizedLabel/fallbackString").and_then(|x| x.as_str()).unwrap_or(""), "type": n["type"].as_str().unwrap_or("")})
        })
        .collect();
    let more = v.pointer("/pageInfo/hasNextPage").and_then(|x| x.as_bool()).unwrap_or(false);
    let count = v["count"].as_u64().unwrap_or(items.len() as u64);
    json!({"count": count, "more": more, "label": count_label(count, more), "items": items, "error": v["code"]})
}

pub fn parse_comment(n: &Value) -> Value {
    json!({"id": n["id"], "ts": n["timestamp"].as_str().unwrap_or(""), "text": n["text"].as_str().unwrap_or(""), "by": s(n, "/author/login"), "byName": s(n, "/author/displayName"), "byId": s(n, "/author/id")})
}

/// `ViewerCardModLogs` data -> ban status + the counts and lists of the tabs.
pub fn parse_mod_logs(data: &Value) -> Value {
    let b = &data["banStatus"];
    let ban = if b.is_object() {
        let permanent = b["isPermanent"].as_bool().unwrap_or(false);
        json!({"permanent": permanent, "expiresAt": b["expiresAt"], "reason": b["reason"].as_str().unwrap_or(""), "by": s(b, "/moderator/displayName"), "since": b["createdAt"]})
    } else {
        Value::Null
    };
    let l = &data["viewerCardModLogs"];
    let m = &l["messages"];
    let m_more = m.pointer("/pageInfo/hasNextPage").and_then(|x| x.as_bool()).unwrap_or(false);
    let m_count = m["count"].as_u64().unwrap_or(0);
    let comments: Vec<Value> = l.pointer("/comments/edges").and_then(|e| e.as_array()).map(|a| a.iter().map(|e| parse_comment(&e["node"])).collect()).unwrap_or_default();
    json!({
        "ban": ban,
        "logs": {
            "messages": {"count": m_count, "more": m_more, "label": count_label(m_count, m_more), "error": m["code"]},
            "warnings": actions_json(&l["warnings"]),
            "timeouts": actions_json(&l["timeouts"]),
            "untimeouts": actions_json(&l["untimeouts"]),
            "bans": actions_json(&l["bans"]),
            "unbans": actions_json(&l["unbans"]),
            "comments": comments,
        }
    })
}

/// `ViewerCardModLogsMessagesBySender` data -> {items, more, cursor}.
pub fn parse_messages(data: &Value) -> Value {
    let m = &data["logs"]["messages"];
    let edges = m["edges"].as_array().cloned().unwrap_or_default();
    let mut cursor = Value::Null;
    let items: Vec<Value> = edges
        .iter()
        .filter_map(|e| {
            cursor = e["cursor"].clone();
            let n = &e["node"];
            match n["__typename"].as_str().unwrap_or("") {
                "ViewerCardModLogsChatMessage" => Some(json!({"kind": "chat", "id": n["id"], "ts": n["sentAt"].as_str().unwrap_or(""), "text": s(n, "/content/text")})),
                "ViewerCardModLogsCaughtMessage" => Some(json!({"kind": "caught", "id": n["id"], "ts": n["sentAt"].as_str().unwrap_or(""), "text": s(n, "/content/text"), "status": n["status"].as_str().unwrap_or("")})),
                "ViewerCardModLogsModActionsMessage" => Some(json!({"kind": "action", "ts": n["timestamp"].as_str().unwrap_or(""), "text": s(n, "/content/fallbackString")})),
                _ => None,
            }
        })
        .collect();
    json!({"items": items, "more": m.pointer("/pageInfo/hasNextPage").and_then(|x| x.as_bool()).unwrap_or(false), "cursor": cursor, "error": m["code"]})
}

// ---------------------------------------------------------------------------------------------------------
// Stored session + state
// ---------------------------------------------------------------------------------------------------------

/// The stored sign-in (JSON under `twitch_web` in secrets.bin). Secret: never printed.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub auth_token: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub login: String,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Record({})", self.login)
    }
}

/// What the sign-in window found. Secret: never printed.
#[derive(Clone, Default, PartialEq)]
pub struct Cookies {
    pub auth_token: String,
}

impl std::fmt::Debug for Cookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cookies(..)")
    }
}

pub type LoginFuture = BoxFuture<'static, Result<Cookies, String>>;

/// The login window the app provides: one window for the twitch.tv sign-in and then Twitch's authorisation page.
#[derive(Clone)]
pub struct LoginHook {
    /// Open the window on twitch.tv's login page. Gives the cookies as soon as the browser profile holds them (the
    /// window stays open); `Err("")` when the window was closed first.
    pub open: Arc<dyn Fn() -> LoginFuture + Send + Sync>,
    /// Show this page in the window. Resolves when the window is gone, closed by the user or by `close`.
    pub show: Arc<dyn Fn(String) -> BoxFuture<'static, ()> + Send + Sync>,
    pub close: Arc<dyn Fn() + Send + Sync>,
    /// Close the window and drop twitch.tv's cookies from the browser profile, so the next login can pick an account.
    pub forget: Arc<dyn Fn() + Send + Sync>,
}

static HOOK: Mutex<Option<LoginHook>> = Mutex::new(None);

pub fn set_login_hook(h: LoginHook) {
    *HOOK.lock().unwrap() = Some(h);
}

pub fn login_hook() -> Option<LoginHook> {
    HOOK.lock().unwrap().clone()
}

#[derive(Default)]
struct State {
    record: Option<Arc<Record>>,
    expired: bool,
}

static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

fn st() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap()
}

fn log(msg: &str) {
    if let Some(c) = ctx() {
        c.log(&format!("[twitch-web] {msg}"));
    }
}

fn changed() {
    EVENTS.emit(&());
}

/// The record as stored and read back (JSON), for the round trip.
pub fn record_to_value(r: &Record) -> Value {
    serde_json::to_value(r).unwrap_or(Value::Null)
}

pub fn record_from_value(v: Value) -> Option<Record> {
    serde_json::from_value::<Record>(v).ok().filter(|r| !r.auth_token.is_empty())
}

fn persist() {
    if read_only() {
        return;
    }
    let Some(ctx) = ctx() else { return };
    let rec = st().record.clone();
    let res = match rec {
        Some(r) => ctx.secrets.set(SECRET_KEY, record_to_value(&r)),
        None => ctx.secrets.delete(SECRET_KEY),
    };
    if let Err(e) = res {
        log(&format!("could not store the twitch.tv sign-in: {e}"));
    }
}

/// Called once at startup, after `rt::set_ctx`.
pub fn init() {
    let Some(ctx) = ctx() else { return };
    if let Some(v) = ctx.secrets.get(SECRET_KEY) {
        match record_from_value(v) {
            Some(rec) => st().record = Some(Arc::new(rec)),
            None => log("could not read the stored twitch.tv sign-in"),
        }
    }
}

pub fn is_signed_in() -> bool {
    st().record.is_some()
}

fn session() -> Option<Arc<Record>> {
    st().record.clone()
}

/// The state Settings and the card read: signed in as whom, whether that is the Twitch account of the app, and
/// whether the combined login is still waiting for the user.
pub fn state_json() -> Value {
    let g = st();
    let app = auth::state();
    let app_login = app.user.as_ref().map(|u| u.login.clone()).unwrap_or_default();
    let mut out = json!({"signedIn": g.record.is_some(), "pending": app.pending.is_some()});
    if let Some(r) = &g.record {
        out["login"] = json!(r.login);
        out["sameUser"] = json!(app_login.is_empty() || app_login.eq_ignore_ascii_case(&r.login));
        out["appLogin"] = json!(app_login);
    }
    out
}

fn mark_expired() {
    {
        let mut g = st();
        if g.record.is_none() {
            return;
        }
        g.record = None;
        g.expired = true;
    }
    persist();
    // the browser profile still holds the dead cookie, which the next login would capture again
    if let Some(h) = login_hook() {
        (h.forget)();
    }
    changed();
}

// ---------- login ----------

/// `currentUser` answer -> (id, login)
pub fn parse_verify(data: &Value) -> Option<(String, String)> {
    let u = &data["currentUser"];
    let (id, login) = (u["id"].as_str()?, u["login"].as_str()?);
    (!id.is_empty() && !login.is_empty()).then(|| (id.to_string(), login.to_string()))
}

/// Verify the cookie of the login window and keep it as the web session.
pub async fn store(c: Cookies) -> Result<(), GqlError> {
    if c.auth_token.is_empty() {
        return Err(GqlError::Other("The twitch.tv sign-in did not finish, try again.".into()));
    }
    let data = gql_with(&c.auth_token, "VerifyLogin", VERIFY_QUERY, json!({})).await?;
    let (user_id, login) = parse_verify(&data).ok_or_else(|| GqlError::Other("Twitch did not say who this sign-in belongs to.".into()))?;
    {
        let mut g = st();
        g.record = Some(Arc::new(Record { auth_token: c.auth_token, user_id, login }));
        g.expired = false;
    }
    persist();
    changed();
    Ok(())
}

/// Sign out of the web session and the browser profile behind it.
pub fn logout() {
    {
        let mut g = st();
        g.record = None;
        g.expired = false;
    }
    persist();
    if let Some(h) = login_hook() {
        (h.forget)();
    }
    changed();
}

// ---------------------------------------------------------------------------------------------------------
// The GQL call
// ---------------------------------------------------------------------------------------------------------

/// The headers of a call: only what the twitch.tv web page's own client id and the OAuth header need.
pub fn gql_headers(auth_token: &str) -> Vec<(&'static str, String)> {
    vec![("Client-Id", WEB_CLIENT_ID.to_string()), ("Authorization", format!("OAuth {auth_token}")), ("Content-Type", "application/json".to_string())]
}

async fn gql_with(auth_token: &str, operation: &str, query: &str, variables: Value) -> Result<Value, GqlError> {
    let mut req = super::http::client().post(GQL_URL).header("User-Agent", user_agent()).timeout(Duration::from_secs(20));
    for (k, v) in gql_headers(auth_token) {
        req = req.header(k, v);
    }
    let res = req.body(gql_body(operation, query, variables).to_string()).send().await.map_err(|_| GqlError::Other("Could not reach Twitch".into()))?;
    let status = res.status().as_u16();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    parse_gql(status, &body)
}

/// A call with the stored session; a 401 forgets it.
async fn gql(operation: &str, query: &str, variables: Value) -> Result<Value, GqlError> {
    let Some(rec) = session() else { return Err(GqlError::Unauthorized) };
    let r = gql_with(&rec.auth_token, operation, query, variables).await;
    if r == Err(GqlError::Unauthorized) {
        mark_expired();
    }
    r
}

// ---------------------------------------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------------------------------------

fn res(status: u16, body: Value) -> Result<RouteRes, RouteError> {
    Ok(RouteRes { status, body })
}

fn err_res(e: &GqlError) -> Result<RouteRes, RouteError> {
    let (status, code) = match e {
        GqlError::Unauthorized => (401, "expired"),
        GqlError::Integrity => (409, "integrity"),
        GqlError::Other(_) => (502, "error"),
    };
    res(status, json!({"error": e.message(), "code": code}))
}

fn route<F, Fut>(f: F) -> Handler
where
    F: Fn(RouteReq) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<RouteRes, RouteError>> + Send + 'static,
{
    Arc::new(move |req| Box::pin(f(req)))
}

pub fn routes() -> Vec<(String, Handler)> {
    vec![
        ("GET /api/twitch/web".into(), route(|_| async { res(200, state_json()) })),
        ("GET /api/twitch/card".into(), route(card)),
        ("GET /api/twitch/card/messages".into(), route(card_messages)),
        ("POST /api/twitch/card/comment".into(), route(comment_add)),
        ("DELETE /api/twitch/card/comment".into(), route(comment_delete)),
    ]
}

/// The channel's user id: the source's own when it knows it, else Twitch's answer for the login.
async fn channel_id(req: &RouteReq) -> Result<String, GqlError> {
    if let Some(id) = req.query_get("channelId").filter(|s| !s.is_empty()) {
        return Ok(id.to_string());
    }
    let login = super::channel::normalize_channel(&req.source.channel).map_err(GqlError::Other)?;
    if let Some(id) = super::source::source_for(&login).and_then(|s| s.channel_id()) {
        return Ok(id);
    }
    let data = gql("ChannelId", "query ChannelId($login: String!) { user(login: $login) { id } }", json!({"login": login})).await?;
    data.pointer("/user/id").and_then(|i| i.as_str()).map(String::from).ok_or_else(|| GqlError::Other("channel not found".into()))
}

/// GET /api/twitch/card?login=&id= : Twitch's viewer card of the person in this channel.
async fn card(req: RouteReq) -> Result<RouteRes, RouteError> {
    if !is_signed_in() {
        return res(401, json!({"error": "Log in to Twitch first", "code": if st().expired { "expired" } else { "signed-out" }}));
    }
    let target_login = req.query_get("login").unwrap_or("").to_lowercase();
    let target_id = req.query_get("id").unwrap_or("").to_string();
    if target_login.is_empty() {
        return res(400, json!({"error": "missing login"}));
    }
    let chan_login = super::channel::normalize_channel(&req.source.channel).unwrap_or_default();
    let cid = match channel_id(&req).await {
        Ok(c) => c,
        Err(e) => return err_res(&e),
    };
    let data = match gql("ViewerCard", &viewer_card_query(), json!({"channelID": cid, "channelIDStr": cid, "targetLogin": target_login, "channelLogin": chan_login})).await {
        Ok(d) => d,
        Err(e) => return err_res(&e),
    };
    if let Some(e) = field_error(&data, "currentUser") {
        if e == GqlError::Unauthorized {
            mark_expired();
        }
        return err_res(&e);
    }
    let mut out = parse_viewer_card(&data);
    out["channelId"] = json!(cid);
    let tid = out.pointer("/user/id").and_then(|i| i.as_str()).map(String::from).filter(|s| !s.is_empty()).unwrap_or(target_id);
    // Twitch's own answer for the sign-in's account: is it the user the app is logged in as?
    let app_login = auth::current().map(|r| r.user.login.clone()).unwrap_or_default();
    let me_login = out.pointer("/me/login").and_then(|l| l.as_str()).unwrap_or("").to_string();
    out["sameUser"] = json!(app_login.is_empty() || app_login.eq_ignore_ascii_case(&me_login));
    if out["canAccessModLogs"] == json!(true) && !tid.is_empty() {
        match gql("ViewerCardModLogs", MOD_LOGS_QUERY, json!({"channelID": cid, "targetID": tid})).await {
            Ok(d) if field_error(&d, "viewerCardModLogs").is_some() => {
                out["logsError"] = match field_error(&d, "viewerCardModLogs") {
                    Some(GqlError::Integrity) => json!({"code": "integrity", "message": INTEGRITY_MESSAGE}),
                    Some(e) => json!({"code": "error", "message": e.message()}),
                    None => Value::Null,
                };
                out["ban"] = parse_mod_logs(&d)["ban"].clone();
            }
            Ok(d) => {
                let logs = parse_mod_logs(&d);
                out["ban"] = logs["ban"].clone();
                out["logs"] = logs["logs"].clone();
            }
            Err(GqlError::Integrity) => out["logsError"] = json!({"code": "integrity", "message": INTEGRITY_MESSAGE}),
            Err(GqlError::Unauthorized) => return err_res(&GqlError::Unauthorized),
            Err(e) => out["logsError"] = json!({"code": "error", "message": e.message()}),
        }
    }
    res(200, out)
}

/// GET /api/twitch/card/messages?id=&cursor= : the next 50 lines of the person Twitch has logged in this channel.
async fn card_messages(req: RouteReq) -> Result<RouteRes, RouteError> {
    let id = req.query_get("id").unwrap_or("").to_string();
    if id.is_empty() {
        return res(400, json!({"error": "missing id"}));
    }
    let cid = match channel_id(&req).await {
        Ok(c) => c,
        Err(e) => return err_res(&e),
    };
    let cursor = req.query_get("cursor").filter(|c| !c.is_empty()).map(|c| json!(c)).unwrap_or(Value::Null);
    match gql("ViewerCardModLogsMessagesBySender", MESSAGES_QUERY, json!({"channelID": cid, "senderID": id, "cursor": cursor})).await {
        Ok(d) => match field_error(&d, "logs") {
            Some(e) => err_res(&e),
            None => res(200, parse_messages(&d)),
        },
        Err(e) => err_res(&e),
    }
}

/// POST /api/twitch/card/comment {targetId, text}
async fn comment_add(req: RouteReq) -> Result<RouteRes, RouteError> {
    let target = req.body["targetId"].as_str().unwrap_or("").to_string();
    let text: String = req.body["text"].as_str().unwrap_or("").trim().chars().take(COMMENT_MAX_CHARS).collect();
    if target.is_empty() || text.is_empty() {
        return res(400, json!({"error": "missing text"}));
    }
    let cid = match channel_id(&req).await {
        Ok(c) => c,
        Err(e) => return err_res(&e),
    };
    match gql("createModComment", CREATE_COMMENT, json!({"input": {"channelID": cid, "targetID": target, "text": text}})).await {
        Ok(d) => match field_error(&d, "createModeratorComment") {
            Some(e) => err_res(&e),
            None => res(200, json!({"comment": parse_comment(&d["createModeratorComment"]["comment"])})),
        },
        Err(e) => err_res(&e),
    }
}

/// DELETE /api/twitch/card/comment {commentId}
async fn comment_delete(req: RouteReq) -> Result<RouteRes, RouteError> {
    let id = req.body["commentId"].as_str().unwrap_or("").to_string();
    if id.is_empty() {
        return res(400, json!({"error": "missing commentId"}));
    }
    let cid = match channel_id(&req).await {
        Ok(c) => c,
        Err(e) => return err_res(&e),
    };
    match gql("deleteModeratorComment", DELETE_COMMENT, json!({"input": {"ID": id, "channelID": cid}})).await {
        Ok(d) => match field_error(&d, "deleteModeratorComment") {
            Some(e) => err_res(&e),
            None => res(200, json!({"ok": true})),
        },
        Err(e) => err_res(&e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_fixture() -> Value {
        json!({
            "targetUser": {
                "id": "42", "login": "cooldude", "displayName": "CoolDude", "createdAt": "2016-04-30T10:00:00Z", "profileImageURL": "https://img/p.png",
                "displayBadges": [{"id": "a", "setID": "subscriber", "version": "12", "title": "12-Month Subscriber", "description": "d", "image1x": "https://b/1", "image2x": "https://b/2"}],
                "relationship": {"followedAt": "2020-01-02T00:00:00Z", "cumulativeTenure": {"months": 14, "daysRemaining": 3}, "subscriptionBenefit": {"id": "s", "tier": "2000", "purchasedWithPrime": false, "gift": {"isGift": true}}},
                "isModerator": true
            },
            "currentUser": {"login": "agu", "id": "1"},
            "channelViewer": {"id": "v", "earnedBadges": [
                {"id": "a", "setID": "subscriber", "version": "12", "title": "12-Month Subscriber", "image1x": "https://b/1"},
                {"id": "b", "setID": "bits", "version": "100", "title": "cheer 100", "image1x": "https://b/3"}]},
            "channel": {"id": "7", "moderationSettings": {"canAccessViewerCardModLogs": true}}
        })
    }

    #[test]
    fn viewer_card_parses() {
        let c = parse_viewer_card(&card_fixture());
        assert_eq!(c["user"]["login"], "cooldude");
        assert_eq!(c["followedAt"], "2020-01-02T00:00:00Z");
        assert_eq!(c["sub"], json!({"tier": 2, "months": 14, "gift": true, "prime": false}));
        assert_eq!(c["isModerator"], true);
        assert_eq!(c["canAccessModLogs"], true);
        let b = c["badges"].as_array().unwrap();
        assert_eq!(b.len(), 2, "the subscriber badge is listed once");
        assert_eq!(b[0]["url"], "https://b/2");
        assert_eq!(b[1]["setId"], "bits");
        assert_eq!(c["me"]["login"], "agu");
    }

    #[test]
    fn viewer_card_without_relationship_or_access() {
        let c = parse_viewer_card(&json!({"targetUser": {"id": "1", "login": "x", "relationship": {"followedAt": null, "subscriptionBenefit": null}}, "channel": {"moderationSettings": {"canAccessViewerCardModLogs": false}}}));
        assert!(c["sub"].is_null());
        assert_eq!(c["canAccessModLogs"], false);
        assert!(c["followedAt"].is_null());
        assert!(parse_viewer_card(&json!({"targetUser": null}))["user"].is_null());
    }

    #[test]
    fn tiers() {
        assert_eq!([tier_number("1000"), tier_number("2000"), tier_number("3000"), tier_number("")], [1, 2, 3, 1]);
    }

    #[test]
    fn counts_show_a_plus_when_there_is_more() {
        assert_eq!(count_label(1000, true), "1000+");
        assert_eq!(count_label(12, false), "12");
        assert_eq!(count_label(0, false), "0");
    }

    fn logs_fixture() -> Value {
        json!({
            "banStatus": {"createdAt": "2026-11-01T00:00:00Z", "expiresAt": "2026-11-02T00:00:00Z", "isPermanent": false, "reason": "spam", "moderator": {"id": "9", "login": "m", "displayName": "Mod"}},
            "viewerCardModLogs": {
                "messages": {"pageInfo": {"hasNextPage": true}, "count": 1000},
                "bans": {"__typename": "ModLogsTargetedActionsConnection", "count": 1, "pageInfo": {"hasNextPage": false}, "edges": [{"cursor": "c", "node": {"id": "x", "localizedLabel": {"fallbackString": "Mod banned CoolDude"}, "timestamp": "2026-10-01T00:00:00Z", "type": "BAN"}}]},
                "timeouts": {"__typename": "ModLogsTargetedActionsConnection", "count": 3, "pageInfo": {"hasNextPage": false}, "edges": []},
                "unbans": {"__typename": "ModLogsTargetedActionsError", "code": "FORBIDDEN"},
                "untimeouts": null,
                "warnings": {"count": 99, "pageInfo": {"hasNextPage": true}, "edges": []},
                "comments": {"__typename": "ModLogsCommentConnection", "edges": [{"cursor": "k", "node": {"id": "cm1", "timestamp": "2026-10-02T00:00:00Z", "text": "watch this one", "author": {"id": "9", "login": "m", "displayName": "Mod"}}}], "pageInfo": {"hasNextPage": false}}
            }
        })
    }

    #[test]
    fn mod_logs_parse_with_counts_and_ban() {
        let l = parse_mod_logs(&logs_fixture());
        assert_eq!(l["ban"]["permanent"], false);
        assert_eq!(l["ban"]["reason"], "spam");
        assert_eq!(l["ban"]["by"], "Mod");
        assert_eq!(l["logs"]["messages"]["label"], "1000+");
        assert_eq!(l["logs"]["bans"]["label"], "1");
        assert_eq!(l["logs"]["bans"]["items"][0]["text"], "Mod banned CoolDude");
        assert_eq!(l["logs"]["timeouts"]["label"], "3");
        assert_eq!(l["logs"]["warnings"]["label"], "99+");
        assert_eq!(l["logs"]["unbans"]["error"], "FORBIDDEN");
        assert_eq!(l["logs"]["untimeouts"]["label"], "0");
        assert_eq!(l["logs"]["comments"][0]["by"], "m");
        assert_eq!(l["logs"]["comments"][0]["text"], "watch this one");
        let none = parse_mod_logs(&json!({"banStatus": null, "viewerCardModLogs": {}}));
        assert!(none["ban"].is_null());
    }

    #[test]
    fn messages_parse_all_node_kinds() {
        let d = json!({"logs": {"messages": {"pageInfo": {"hasNextPage": true}, "edges": [
            {"cursor": "a", "node": {"__typename": "ViewerCardModLogsChatMessage", "id": "1", "sentAt": "2026-10-01T00:00:00Z", "content": {"text": "hello"}}},
            {"cursor": "b", "node": {"__typename": "ViewerCardModLogsCaughtMessage", "id": "2", "status": "DENIED", "category": "x", "sentAt": "2026-10-01T00:01:00Z", "content": {"text": "bad"}}},
            {"cursor": "c", "node": {"__typename": "ViewerCardModLogsModActionsMessage", "timestamp": "2026-10-01T00:02:00Z", "content": {"fallbackString": "Mod timed out X"}}}]}}});
        let m = parse_messages(&d);
        assert_eq!(m["items"].as_array().unwrap().len(), 3);
        assert_eq!(m["items"][0]["text"], "hello");
        assert_eq!(m["items"][1]["status"], "DENIED");
        assert_eq!(m["items"][2]["kind"], "action");
        assert_eq!(m["cursor"], "c");
        assert_eq!(m["more"], true);
    }

    #[test]
    fn gql_answers() {
        assert_eq!(parse_gql(401, &json!({"error": "Unauthorized", "status": 401, "message": "The \"Authorization\" token is invalid."})), Err(GqlError::Unauthorized));
        assert_eq!(parse_gql(200, &json!([{"errors": [{"message": "failed integrity check"}]}])), Err(GqlError::Integrity));
        assert_eq!(parse_gql(200, &json!({"errors": [{"message": "failed integrity check"}]})), Err(GqlError::Integrity));
        assert_eq!(parse_gql(200, &json!({"error": "Bad Request", "message": "failed integrity check"})), Err(GqlError::Integrity));
        assert_eq!(parse_gql(200, &json!({"errors": [{"message": "service error"}]})), Err(GqlError::Other("service error".into())));
        assert_eq!(parse_gql(200, &json!({"data": {"a": 1}})), Ok(json!({"a": 1})));
        assert!(parse_gql(200, &json!({"data": null})).is_err());
        // a partial answer: the refused field is null, the rest is kept
        let part = parse_gql(200, &json!({"data": {"targetUser": {"id": "1", "relationship": null}, "logs": null}, "errors": [
            {"message": "failed integrity check", "path": ["targetUser", "relationship"], "extensions": {"code": "IntegrityCheckFailed"}},
            {"message": "unauthenticated", "path": ["logs"], "extensions": {"code": "Unauthenticated"}}]})).unwrap();
        assert_eq!(part["targetUser"]["id"], "1");
        assert_eq!(field_error(&part, "targetUser"), None);
        assert_eq!(field_error(&part, "logs"), Some(GqlError::Unauthorized));
        let part = parse_gql(200, &json!({"data": {"a": 1, "m": null}, "errors": [{"message": "failed integrity check", "path": ["m"]}]})).unwrap();
        assert_eq!(field_error(&part, "m"), Some(GqlError::Integrity));
    }

    #[test]
    fn verify_answer() {
        assert_eq!(parse_verify(&json!({"currentUser": {"id": "1", "login": "agu"}})), Some(("1".into(), "agu".into())));
        assert_eq!(parse_verify(&json!({"currentUser": null})), None);
    }

    #[test]
    fn body_and_headers_carry_no_integrity() {
        let b = gql_body("ViewerCard", "query ViewerCard { a }", json!({"x": 1}));
        assert_eq!(b["operationName"], "ViewerCard");
        assert_eq!(b["variables"]["x"], 1);
        let h = gql_headers("TOKEN");
        assert!(h.iter().any(|(k, v)| *k == "Authorization" && v == "OAuth TOKEN"));
        assert!(h.iter().any(|(k, v)| *k == "Client-Id" && v == WEB_CLIENT_ID));
        assert!(!h.iter().any(|(k, _)| k.to_lowercase().contains("integrity") || k.to_lowercase() == "user-agent"));
    }

    #[test]
    fn record_round_trip_and_secrets() {
        let r = Record { auth_token: "SECRET".into(), user_id: "1".into(), login: "agu".into() };
        let v = record_to_value(&r);
        assert_eq!(v["authToken"], "SECRET");
        assert_eq!(record_from_value(v), Some(r.clone()));
        assert_eq!(record_from_value(json!({"authToken": ""})), None);
        assert_eq!(record_from_value(json!({"junk": 1})), None);
        assert!(!format!("{r:?}").contains("SECRET"));
        assert!(!format!("{:?}", Cookies { auth_token: "SECRET".into() }).contains("SECRET"));
    }

    #[test]
    fn queries_declare_what_they_use() {
        let q = viewer_card_query();
        assert!(q.contains("$channelLogin") && q.contains("$targetLogin"));
    }
}
