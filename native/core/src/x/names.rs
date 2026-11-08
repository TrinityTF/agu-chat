//! Chatter names. The live-chat stream only carries a user id; x.com's own chat client turns ids into names with
//! the `liveAtomsUserQuery` GraphQL call (up to 100 ids), which needs a logged-in session. Names found are
//! cached for the app run (bounded); ids X does not return are remembered as misses for a while so they are not
//! asked again with every message.
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Value};

use super::auth::{self, ApiError};
use crate::util::now_ms;

const QUERY_ID: &str = "n9VGEKZLhDHED8ouN0dxUw";
/// x.com's own limit per call.
pub const MAX_IDS_PER_CALL: usize = 100;
const MAX_KNOWN: usize = 5000;
const MAX_MISSES: usize = 5000;
/// An id X did not return is not asked about again for this long.
pub const MISS_TTL_MS: i64 = 10 * 60_000;

#[derive(Debug, Clone, PartialEq)]
pub struct XUser {
    pub id: String,
    pub username: String,
    pub name: String,
    pub avatar: String,
    pub verified: bool,
}

/// `variables` for the lookup, urlencoded.
pub fn lookup_variables(ids: &[String]) -> String {
    let v = json!({"userIds": ids}).to_string();
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}

/// The two hosts x.com serves the call on.
pub fn lookup_urls(ids: &[String]) -> [String; 2] {
    let q = lookup_variables(ids);
    [format!("https://x.com/i/api/graphql/{QUERY_ID}/liveAtomsUserQuery?variables={q}"), format!("https://api.x.com/graphql/{QUERY_ID}/liveAtomsUserQuery?variables={q}")]
}

/// `data.users[].result` -> users. Entries without a username (suspended or unavailable accounts) are left out.
pub fn parse_lookup(body: &Value) -> Result<Vec<XUser>, String> {
    let Some(users) = body["data"]["users"].as_array() else {
        return Err(auth::error_message(body).unwrap_or_else(|| "X sent an unexpected reply to the name lookup".into()));
    };
    let s = |v: &Value| v.as_str().unwrap_or("").to_string();
    let mut out = Vec::new();
    for u in users {
        let r = &u["result"];
        let id = r["rest_id"].as_str().map(String::from).or_else(|| r["rest_id"].as_u64().map(|n| n.to_string())).unwrap_or_default();
        // x.com moved these fields from `legacy` to `core`; either is read.
        let pick = |a: &Value, b: &Value| if a.as_str().map_or(false, |x| !x.is_empty()) { s(a) } else { s(b) };
        let username = pick(&r["core"]["screen_name"], &r["legacy"]["screen_name"]);
        if id.is_empty() || username.is_empty() {
            continue;
        }
        let name = pick(&r["core"]["name"], &r["legacy"]["name"]);
        let avatar = pick(&r["avatar"]["image_url"], &r["legacy"]["profile_image_url_https"]);
        let verified = r["verification"]["verified"].as_bool().unwrap_or(false);
        out.push(XUser { id, username: username.clone(), name: if name.is_empty() { username } else { name }, avatar, verified });
    }
    Ok(out)
}

/// What the cache knows about one id.
#[derive(Debug, Clone, PartialEq)]
pub enum Known {
    User(XUser),
    /// X was asked recently and had no such user.
    Miss,
    Unknown,
}

#[derive(Default)]
pub struct NameCache {
    known: HashMap<String, XUser>,
    misses: HashMap<String, i64>,
}

impl NameCache {
    pub fn get(&self, id: &str, now: i64) -> Known {
        if let Some(u) = self.known.get(id) {
            return Known::User(u.clone());
        }
        match self.misses.get(id) {
            Some(at) if now - at < MISS_TTL_MS => Known::Miss,
            _ => Known::Unknown,
        }
    }

    /// The ids in `ids` that must be looked up (no name, no recent miss), each once.
    pub fn unknown(&self, ids: impl IntoIterator<Item = String>, now: i64) -> Vec<String> {
        let mut seen = HashSet::new();
        ids.into_iter().filter(|id| seen.insert(id.clone()) && self.get(id, now) == Known::Unknown).collect()
    }

    /// Store an answer: `found` are known, the rest of `asked` are misses.
    pub fn record(&mut self, asked: &[String], found: Vec<XUser>, now: i64) {
        if self.known.len() + found.len() > MAX_KNOWN {
            self.known.clear();
        }
        if self.misses.len() + asked.len() > MAX_MISSES {
            self.misses.retain(|_, at| now - *at < MISS_TTL_MS);
            if self.misses.len() + asked.len() > MAX_MISSES {
                self.misses.clear();
            }
        }
        let got: HashSet<&str> = found.iter().map(|u| u.id.as_str()).collect();
        for id in asked {
            if !got.contains(id.as_str()) {
                self.misses.insert(id.clone(), now);
            }
        }
        for u in found {
            self.misses.remove(&u.id);
            self.known.insert(u.id.clone(), u);
        }
    }
}

static CACHE: LazyLock<Mutex<NameCache>> = LazyLock::new(|| Mutex::new(NameCache::default()));

pub fn cached(id: &str) -> Known {
    CACHE.lock().unwrap().get(id, now_ms())
}

pub fn unknown_ids(ids: impl IntoIterator<Item = String>) -> Vec<String> {
    CACHE.lock().unwrap().unknown(ids, now_ms())
}

/// Look these ids up with an explicit session (no caching).
pub async fn lookup_with(auth_token: &str, ct0: &str, ids: &[String]) -> Result<Vec<XUser>, ApiError> {
    let mut last = ApiError { status: 0, message: "Could not reach X".into() };
    for url in lookup_urls(ids) {
        match auth::call("GET", &url, auth_token, ct0, None, 15_000).await {
            Err(e) => last = e,
            Ok((status, v)) if (200..300).contains(&status) => match parse_lookup(&v) {
                Ok(u) => return Ok(u),
                Err(m) => last = ApiError { status, message: m },
            },
            Ok((status, v)) => {
                last = ApiError { status, message: auth::error_message(&v).unwrap_or_else(|| format!("X answered {status} to the name lookup")) };
                // A session X refuses is refused on both hosts.
                if last.is_auth() {
                    return Err(last);
                }
            }
        }
    }
    Err(last)
}

/// Look up and cache the names of `ids` with the logged-in session (in calls of at most 100). A refused session
/// (401) is reported to the account (`mark_expired`) as well as returned.
pub async fn fetch_names(ids: &[String]) -> Result<(), ApiError> {
    let Some(rec) = auth::session() else { return Err(ApiError { status: 401, message: auth::LOGGED_OUT_REASON.into() }) };
    for chunk in ids.chunks(MAX_IDS_PER_CALL) {
        match lookup_with(&rec.auth_token, &rec.ct0, chunk).await {
            Ok(found) => CACHE.lock().unwrap().record(chunk, found, now_ms()),
            Err(e) => {
                // Only 401 means the session is gone: x.com also answers 403 to requests its anti-bot checks
                // dislike, which must not log the user out.
                if e.status == 401 {
                    auth::mark_expired();
                }
                return Err(e);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(id: &str, name: &str) -> XUser {
        XUser { id: id.into(), username: name.into(), name: name.into(), avatar: String::new(), verified: false }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn lookup_request() {
        let list = ids(&["1", "22"]);
        assert_eq!(lookup_variables(&list), "%7B%22userIds%22%3A%5B%221%22%2C%2222%22%5D%7D");
        let urls = lookup_urls(&list);
        assert!(urls[0].starts_with("https://x.com/i/api/graphql/n9VGEKZLhDHED8ouN0dxUw/liveAtomsUserQuery?variables="));
        assert!(urls[1].starts_with("https://api.x.com/graphql/n9VGEKZLhDHED8ouN0dxUw/liveAtomsUserQuery?variables="));
    }

    #[test]
    fn lookup_answer() {
        let body = json!({"data": {"users": [
            {"result": {"__typename": "User", "rest_id": "1", "core": {"screen_name": "alice", "name": "Alice A"}, "avatar": {"image_url": "https://pbs.twimg.com/a_normal.jpg"}, "verification": {"verified": true}}},
            {"result": {"__typename": "User", "rest_id": "2", "core": {"screen_name": "bob", "name": ""}}},
            {"result": {"__typename": "UserUnavailable"}},
            {"result": {"rest_id": "4", "legacy": {"screen_name": "dan", "name": "Dan", "profile_image_url_https": "https://x/d.jpg"}}},
            {"result": null}
        ]}});
        let v = parse_lookup(&body).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], XUser { id: "1".into(), username: "alice".into(), name: "Alice A".into(), avatar: "https://pbs.twimg.com/a_normal.jpg".into(), verified: true });
        // an empty display name falls back to the username
        assert_eq!(v[1].name, "bob");
        assert!(!v[1].verified);
        assert_eq!(v[2], XUser { id: "4".into(), username: "dan".into(), name: "Dan".into(), avatar: "https://x/d.jpg".into(), verified: false });
        assert_eq!(parse_lookup(&json!({"data": {"users": []}})).unwrap(), vec![]);
        assert_eq!(parse_lookup(&json!({"errors": [{"message": "Bad guest token"}]})), Err("Bad guest token".into()));
        assert!(parse_lookup(&json!({})).is_err());
    }

    #[test]
    fn cache_hold_and_drop() {
        let mut c = NameCache::default();
        assert_eq!(c.unknown(ids(&["1", "2", "1", "3"]), 0), ids(&["1", "2", "3"]));
        // X knew 1 and 2 but not 3
        c.record(&ids(&["1", "2", "3"]), vec![u("1", "alice"), u("2", "bob")], 1000);
        assert_eq!(c.get("1", 1000), Known::User(u("1", "alice")));
        assert_eq!(c.get("3", 1000), Known::Miss);
        assert_eq!(c.get("9", 1000), Known::Unknown);
        // a miss is not asked again until it expires
        assert_eq!(c.unknown(ids(&["1", "3", "9"]), 2000), ids(&["9"]));
        assert_eq!(c.unknown(ids(&["3"]), 1000 + MISS_TTL_MS), ids(&["3"]));
        // a miss that later resolves becomes a name
        c.record(&ids(&["3"]), vec![u("3", "carol")], 5000);
        assert_eq!(c.get("3", 5000), Known::User(u("3", "carol")));
    }

    #[test]
    fn cache_is_bounded() {
        let mut c = NameCache::default();
        let many: Vec<XUser> = (0..MAX_KNOWN).map(|i| u(&i.to_string(), "n")).collect();
        c.record(&[], many, 0);
        c.record(&[], vec![u("extra", "e")], 0);
        assert!(c.known.len() <= MAX_KNOWN);
        assert_eq!(c.get("extra", 0), Known::User(u("extra", "e")));
    }
}
