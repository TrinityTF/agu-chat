//! Port of src/main/twitch/helix.js: Helix with the logged-in user's token. A 401 refreshes the token once
//! and retries.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use serde_json::{json, Value};

use super::auth;
use super::http::{request_json, HttpError, ReqOpts};

const HELIX: &str = "https://api.twitch.tv/helix";

/// Query parameters in JS object form: null, "" and missing are skipped, arrays repeat the key.
pub type Query = Vec<(String, Value)>;

/// `q!{ "broadcaster_id" => id, "first" => 100 }`
#[macro_export]
macro_rules! q {
    ($($k:expr => $v:expr),* $(,)?) => {
        vec![$(($k.to_string(), serde_json::Value::from($v))),*] as $crate::twitch::helix::Query
    };
}

pub fn login_required() -> HttpError {
    HttpError::local(401, "Log in to Twitch") // ours, not Twitch's: routes answer it as-is
}

fn qs(query: &Query) -> String {
    let mut p = url::form_urlencoded::Serializer::new(String::new());
    let mut any = false;
    let scalar = |v: &Value| -> Option<String> {
        match v {
            Value::Null => None,
            Value::String(s) if s.is_empty() => None,
            Value::String(s) => Some(s.clone()),
            other => Some(crate::util::js_string(other)),
        }
    };
    for (k, v) in query {
        if let Value::Array(a) = v {
            for x in a {
                p.append_pair(k, &crate::util::js_string(x));
                any = true;
            }
        } else if let Some(s) = scalar(v) {
            p.append_pair(k, &s);
            any = true;
        }
    }
    if any {
        format!("?{}", p.finish())
    } else {
        String::new()
    }
}

/// `helix("/chat/settings", {query:{broadcaster_id}})` -> parsed JSON, or Null on 204. Err(HttpError) otherwise.
pub async fn helix(path: &str, method: &'static str, query: Query, body: Option<Value>) -> Result<Value, HttpError> {
    let mut attempt = 0;
    loop {
        let Some(rec) = auth::fresh_record().await else { return Err(login_required()) };
        let r = request_json(
            &format!("{HELIX}{path}{}", qs(&query)),
            ReqOpts {
                method,
                headers: vec![("Authorization".into(), format!("Bearer {}", rec.access_token)), ("Client-Id".into(), rec.client_id.clone())],
                json: body.clone(),
                ..Default::default()
            },
        )
        .await;
        match r {
            Err(err) if err.status == 401 && attempt == 0 && auth::refresh_now().await => {
                attempt += 1;
                continue;
            }
            other => return other,
        }
    }
}

pub async fn helix_get(path: &str, query: Query) -> Result<Value, HttpError> {
    helix(path, "GET", query, None).await
}

pub struct AllRows {
    pub rows: Vec<Value>,
    pub template: Option<Value>,
}

/// Follows pagination.cursor and returns every `data` row (capped so a huge list cannot run away).
pub async fn helix_all(path: &str, query: Query, max_pages: usize) -> Result<AllRows, HttpError> {
    let mut rows = Vec::new();
    let mut after: Option<String> = None;
    let mut template = None;
    for _ in 0..max_pages {
        let mut qy = query.clone();
        qy.push(("after".into(), after.clone().map(Value::from).unwrap_or(Value::Null)));
        let last = helix_get(path, qy).await?;
        if let Some(d) = last["data"].as_array() {
            rows.extend(d.iter().cloned());
        }
        template = last.get("template").cloned();
        after = last["pagination"]["cursor"].as_str().filter(|s| !s.is_empty()).map(String::from);
        if after.is_none() {
            break;
        }
    }
    Ok(AllRows { rows, template })
}

// login -> {id, login, name}, cached for an hour (logins rarely change hands).
const USER_TTL_MS: u128 = 60 * 60_000;

struct UserCache {
    map: HashMap<String, (Value, Instant)>,
    order: Vec<String>,
}

static USER_CACHE: LazyLock<Mutex<UserCache>> = LazyLock::new(|| Mutex::new(UserCache { map: HashMap::new(), order: Vec::new() }));

pub async fn user_by_login(login: &str) -> Result<Option<Value>, HttpError> {
    if let Some((user, at)) = USER_CACHE.lock().unwrap().map.get(login) {
        if at.elapsed().as_millis() < USER_TTL_MS {
            return Ok(Some(user.clone()));
        }
    }
    let page = helix_get("/users", q! {"login" => login}).await?;
    let Some(u) = page["data"].get(0).filter(|u| !u.is_null()) else { return Ok(None) };
    let id = crate::util::js_string(&u["id"]);
    let login_v = u["login"].clone();
    let name = u["display_name"].as_str().filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| crate::util::js_string(&login_v));
    let user = json!({"id": id, "login": login_v, "name": name});
    let mut c = USER_CACHE.lock().unwrap();
    if !c.map.contains_key(login) {
        c.order.push(login.to_string());
    }
    c.map.insert(login.to_string(), (user.clone(), Instant::now()));
    if c.map.len() > 500 {
        let first = c.order.remove(0);
        c.map.remove(&first);
    }
    Ok(Some(user))
}

pub fn clear_user_cache() {
    let mut c = USER_CACHE.lock().unwrap();
    c.map.clear();
    c.order.clear();
}
