//! Port of src/main/twitch/access.js: is the logged-in user the broadcaster or a moderator of a channel?
//! Twitch has no "am I a mod here" call, so this lists the channels the user moderates
//! (GET /moderation/channels, needs user:read:moderated_channels) and caches it.
use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use super::auth::{self, Record, RecordUser};
use super::helix::helix_all;
use super::rt::log;
use crate::q;
use crate::util::now_ms;

const TTL_MS: i64 = 10 * 60_000;

struct Cache {
    user_id: String,
    at: i64,
    ids: HashSet<String>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);
/// Single flight: callers queue behind one load and re-check the cache after it.
static LOADING: LazyLock<tokio::sync::Mutex<()>> = LazyLock::new(|| tokio::sync::Mutex::new(()));

pub fn invalidate() {
    *CACHE.lock().unwrap() = None;
}

async fn load(rec: &Record) {
    match helix_all("/moderation/channels", q! {"user_id" => rec.user.id.clone(), "first" => 100}, 20).await {
        Ok(all) => {
            let ids = all.rows.iter().map(|r| crate::util::js_string(&r["broadcaster_id"])).collect();
            *CACHE.lock().unwrap() = Some(Cache { user_id: rec.user.id.clone(), at: now_ms(), ids });
        }
        Err(err) => {
            log(&format!("could not list moderated channels {}", err.message));
            // A stale list beats no list; only give up when there is nothing at all.
            if let Some(c) = CACHE.lock().unwrap().as_mut().filter(|c| c.user_id == rec.user.id) {
                c.at = now_ms() - TTL_MS + 60_000; // retry in a minute
            }
        }
    }
}

fn fresh(user_id: &str) -> bool {
    CACHE.lock().unwrap().as_ref().map(|c| c.user_id == user_id && now_ms() - c.at <= TTL_MS).unwrap_or(false)
}

pub struct Access {
    pub logged_in: bool,
    pub me: Option<RecordUser>,
    pub is_broadcaster: bool,
    /// true / false, or None when it cannot be known (the token predates the scope, or Twitch could not be
    /// reached and nothing is cached).
    pub is_mod: Option<bool>,
}

pub async fn access(channel_id: &str) -> Access {
    let Some(rec) = auth::current() else { return Access { logged_in: false, me: None, is_broadcaster: false, is_mod: Some(false) } };
    let mut out = Access { logged_in: true, me: Some(rec.user.clone()), is_broadcaster: channel_id == rec.user.id, is_mod: None };
    if out.is_broadcaster {
        out.is_mod = Some(true);
        return out;
    }
    if !rec.scopes.iter().any(|s| s == "user:read:moderated_channels") {
        return out;
    }
    if !fresh(&rec.user.id) {
        let _guard = LOADING.lock().await;
        if !fresh(&rec.user.id) {
            load(&rec).await;
        }
    }
    if let Some(c) = CACHE.lock().unwrap().as_ref().filter(|c| c.user_id == rec.user.id) {
        out.is_mod = Some(c.ids.contains(channel_id));
    }
    out
}
