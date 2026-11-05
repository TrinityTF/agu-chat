//! Port of src/main/x/vendor/xLive.js (render lib/xLive.js): is this X account live, and on which
//! broadcast, with no account at all. Guest token only: no account, no bot session, no paid v2 API.
//!
//! WHAT THIS CAN AND CANNOT DO (all measured against a live broadcast). Given a broadcast id, everything
//! works anonymously (state, title, chat). Getting the id from a handle is AUTH-GATED at X's end: the public
//! profile only carries a broadcast link if the streamer tweeted it (and carries OLD links too), so
//! `find_live` is best effort, and pasting the broadcast link is the path that always works.
//!
//! STATE IS THE ONLY TRUSTWORTHY SIGNAL: a broadcast id on a profile says only that it was linked once.
//! Nothing is live here until broadcasts/show says RUNNING.
//!
//! Differences from the JS: requests get a 20 s header deadline (JS had none), and the honest UA comes from
//! x::ua. Lookaheads (`(?![A-Za-z0-9])`) become "the whole alphanumeric run is 13 or 14 long".
use std::sync::{LazyLock, Mutex};

use regex::Regex;
use serde_json::Value;

use crate::net::{self, FetchOpts};
use crate::util::{js_str_or_empty, js_string, js_to_number, slice_utf16, truthy, now_ms};
use crate::x::ua::get_user_agent;

pub const WEB_BEARER: &str = "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

const TIMEOUT_MS: u64 = 20_000;

// Guest tokens last hours, and activating one per lookup would be both slow and the kind of traffic pattern
// that gets an IP rate limited. Refreshed on expiry rather than on a timer, since only a rejection proves it
// is stale.
static TOKEN: Mutex<(String, i64)> = Mutex::new((String::new(), 0));
const TOKEN_TTL_MS: i64 = 60 * 60 * 1000;

pub async fn guest_token(force: bool) -> Result<String, String> {
    {
        let t = TOKEN.lock().unwrap();
        if !force && !t.0.is_empty() && now_ms() - t.1 < TOKEN_TTL_MS {
            return Ok(t.0.clone());
        }
    }
    let r = net::fetch(
        "https://api.x.com/1.1/guest/activate.json",
        FetchOpts::new(TIMEOUT_MS).method("POST").header("authorization", &format!("Bearer {WEB_BEARER}")).header("user-agent", &get_user_agent()),
    )
    .await
    .map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("could not get an X guest token ({})", r.status().as_u16()));
    }
    let j = net::json(r).await.map_err(|e| e.message)?;
    if !truthy(&j["guest_token"]) {
        return Err("X returned no guest token".into());
    }
    let tok = js_string(&j["guest_token"]);
    *TOKEN.lock().unwrap() = (tok.clone(), now_ms());
    Ok(tok)
}

static PROFILE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:https?://)?(?:www\.)?(?:x|twitter)\.com/").unwrap());
static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_]{1,15}$").unwrap());

/// A handle, however it was typed: bare, @-prefixed, or a full profile URL. "" when it is not one.
pub fn normalize_handle(input: &str) -> String {
    let t = crate::util::js_trim(input);
    let t = PROFILE_PREFIX_RE.replace(t, "");
    let t = t.strip_prefix('@').unwrap_or(&t);
    let clean = t.split(|c| c == '/' || c == '?' || c == '#').next().unwrap_or("");
    if HANDLE_RE.is_match(clean) { clean.to_string() } else { String::new() }
}

static BROADCAST_PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:broadcasts?|i/broadcasts)/([A-Za-z0-9]+)").unwrap());
static BROADCAST_BARE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([A-Za-z0-9]{13,14})$").unwrap());
static PROFILE_BROADCAST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:x|twitter)\.com/i/broadcasts/([A-Za-z0-9]+)").unwrap());

fn id_len_ok(id: &str) -> bool {
    (13..=14).contains(&id.len())
}

/// The first `broadcasts/<id>` in `s` whose id is 13-14 alphanumerics and nothing more
/// (`/(?:broadcasts?|i\/broadcasts)\/([A-Za-z0-9]{13,14})(?![A-Za-z0-9])/`).
pub fn broadcast_id_in_url(s: &str) -> Option<String> {
    BROADCAST_PATH_RE.captures_iter(s).map(|c| c[1].to_string()).find(|id| id_len_ok(id))
}

/// A broadcast id out of anything a person might paste, or "".
/// 13 characters ("1DGleVdEapOJL") or, since 2026-10-27, 14 ("2qVWZNBJRbVaJN"); the trailing guard keeps a
/// 14-character id from being read as its first 13.
pub fn parse_broadcast_id(input: &str) -> String {
    let s = crate::util::js_trim(input);
    broadcast_id_in_url(s).or_else(|| BROADCAST_BARE_RE.captures(s).map(|c| c[1].to_string())).unwrap_or_default()
}

#[derive(Clone, Debug, Default)]
pub struct BroadcastState {
    pub state: String,
    pub title: String,
    pub started_at: i64,
    pub updated_at: i64,
    pub display_name: String,
    /// The Periscope record's `username` is whatever the handle was when the account first streamed and is
    /// not kept in step. Carried for display only; never matched against a handle.
    pub legacy_username: String,
    pub media_key: String,
}

fn auth_headers(o: FetchOpts, token: &str) -> FetchOpts {
    o.header("authorization", &format!("Bearer {WEB_BEARER}")).header("x-guest-token", token).header("user-agent", &get_user_agent())
}

async fn show(id: &str, token: &str) -> Result<reqwest::Response, String> {
    let url = format!("https://api.x.com/1.1/broadcasts/show.json?ids={}", url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>());
    net::fetch(&url, auth_headers(FetchOpts::new(TIMEOUT_MS), token)).await.map_err(|e| e.message)
}

/// Ask X the state of each broadcast id, in the order given.
///
/// One id per request on purpose. The `ids` parameter looks like it takes a comma-separated list and does
/// not: given two, it answers with a single key that is the literal comma-joined string and an empty body,
/// which reads as "both unknown" rather than as an error.
pub async fn broadcast_states(ids: &[String]) -> Result<Vec<(String, BroadcastState)>, String> {
    let mut out = Vec::new();
    for id in ids.iter().take(12) {
        let mut token = guest_token(false).await?;
        let mut r = show(id, &token).await?;
        // A stale guest token shows up as a 401/403 rather than as anything that says "token", so one forced
        // refresh and one retry.
        if matches!(r.status().as_u16(), 401 | 403) {
            token = guest_token(true).await?;
            r = show(id, &token).await?;
        }
        if !r.status().is_success() {
            continue;
        }
        let j = net::json(r).await.unwrap_or(Value::Null);
        if let Some(s) = state_from_json(&j, id) {
            out.push((id.clone(), s));
        }
    }
    Ok(out)
}

pub fn state_from_json(j: &Value, id: &str) -> Option<BroadcastState> {
    let b = &j["broadcasts"][id];
    if !b.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        return None;
    }
    let num = |v: &Value| {
        let n = js_to_number(Some(v));
        if n.is_nan() { 0 } else { n as i64 }
    };
    let state = js_str_or_empty(Some(&b["state"]));
    Some(BroadcastState {
        state: if state.is_empty() { "?".into() } else { state },
        title: slice_utf16(&js_str_or_empty(Some(&b["status"])), 140),
        started_at: num(&b["created_at_ms"]),
        updated_at: num(&b["updated_at_ms"]),
        display_name: js_str_or_empty(Some(&b["user_display_name"])),
        legacy_username: js_str_or_empty(Some(&b["username"])),
        media_key: js_str_or_empty(Some(&b["media_key"])),
    })
}

pub struct ProfileIds {
    pub ids: Vec<String>,
    pub status: u16,
    pub bytes: usize,
    pub handle: String,
}

/// Every broadcast id linked from an account's public profile page.
pub async fn broadcast_ids_on_profile(handle: &str) -> Result<ProfileIds, String> {
    let clean = normalize_handle(handle);
    if clean.is_empty() {
        return Err("that is not an X handle".into());
    }
    let r = net::fetch(
        &format!("https://x.com/{clean}"),
        FetchOpts::new(TIMEOUT_MS).header("user-agent", &get_user_agent()).header("accept", "text/html,application/xhtml+xml").header("accept-language", "en-US,en;q=0.9"),
    )
    .await
    .map_err(|e| e.message)?;
    let status = r.status().as_u16();
    let html = net::text(r).await.map_err(|e| e.message)?;
    Ok(ProfileIds { ids: ids_in_profile(&html), status, bytes: html.len(), handle: clean })
}

/// Only ids reached through a broadcast URL. (An earlier version also swept up every 13-character quoted
/// string in the page, which matched base64 chunks, asset hashes and tweet ids.)
pub fn ids_in_profile(html: &str) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for c in PROFILE_BROADCAST_RE.captures_iter(html) {
        let id = c[1].to_string();
        if id_len_ok(&id) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

#[derive(Clone, Debug, Default)]
pub struct FindLive {
    pub live: bool,
    pub broadcast_id: String,
    pub title: String,
}

/// Is this handle live right now? "not live" is an ordinary answer rather than an error.
pub async fn find_live(handle: &str) -> Result<FindLive, String> {
    let found = broadcast_ids_on_profile(handle).await?;
    if found.ids.is_empty() {
        return Ok(FindLive::default());
    }
    let states = broadcast_states(&found.ids).await?;
    Ok(newest_running(&states))
}

/// Newest first, so that a profile linking several streams settles on the current one rather than whichever
/// the page happened to mention first.
pub fn newest_running(states: &[(String, BroadcastState)]) -> FindLive {
    let mut best: Option<&(String, BroadcastState)> = None;
    for e in states.iter().filter(|(_, s)| s.state == "RUNNING") {
        if best.map(|b| e.1.started_at > b.1.started_at).unwrap_or(true) {
            best = Some(e);
        }
    }
    match best {
        Some((id, s)) => FindLive { live: true, broadcast_id: id.clone(), title: s.title.clone() },
        None => FindLive::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handles() {
        assert_eq!(normalize_handle("EsfandTV"), "EsfandTV");
        assert_eq!(normalize_handle("@EsfandTV"), "EsfandTV");
        assert_eq!(normalize_handle("  @esfandtv  "), "esfandtv");
        assert_eq!(normalize_handle("https://x.com/EsfandTV"), "EsfandTV");
        assert_eq!(normalize_handle("http://www.twitter.com/EsfandTV/status/1?s=2"), "EsfandTV");
        assert_eq!(normalize_handle("x.com/EsfandTV#top"), "EsfandTV");
        assert_eq!(normalize_handle("TWITTER.COM/Foo"), "Foo");
        assert_eq!(normalize_handle("a_b_c_1234567"), "a_b_c_1234567");
        assert_eq!(normalize_handle("sixteen_chars_xx"), "");
        assert_eq!(normalize_handle("has space"), "");
        assert_eq!(normalize_handle("bad-dash"), "");
        assert_eq!(normalize_handle(""), "");
        assert_eq!(normalize_handle("@"), "");
    }

    #[test]
    fn broadcast_ids() {
        assert_eq!(parse_broadcast_id("https://x.com/i/broadcasts/1DGleVdEapOJL"), "1DGleVdEapOJL");
        assert_eq!(parse_broadcast_id("x.com/i/broadcasts/2qVWZNBJRbVaJN?s=1"), "2qVWZNBJRbVaJN");
        assert_eq!(parse_broadcast_id("https://x.com/i/broadcasts/1DGleVdEapOJLXX"), ""); // 15: neither
        assert_eq!(parse_broadcast_id("1DGleVdEapOJL"), "1DGleVdEapOJL");
        assert_eq!(parse_broadcast_id("2qVWZNBJRbVaJN"), "2qVWZNBJRbVaJN");
        assert_eq!(parse_broadcast_id("short"), "");
        assert_eq!(broadcast_id_in_url("twitter.com/broadcast/1DGleVdEapOJL"), Some("1DGleVdEapOJL".into()));
        let html = r#"<a href="https://x.com/i/broadcasts/1DGleVdEapOJL">a</a> twitter.com/i/broadcasts/2qVWZNBJRbVaJN x.com/i/broadcasts/1DGleVdEapOJL x.com/i/broadcasts/toolongtoolongtoolong"#;
        assert_eq!(ids_in_profile(html), vec!["1DGleVdEapOJL", "2qVWZNBJRbVaJN"]);
    }

    #[test]
    fn states_and_newest() {
        let j = json!({"broadcasts": {"abc": {"state": "RUNNING", "status": "Title", "created_at_ms": 5, "user_display_name": "D", "username": "u", "media_key": "m"}, "empty": {}}});
        let s = state_from_json(&j, "abc").unwrap();
        assert_eq!((s.state.as_str(), s.title.as_str(), s.started_at, s.display_name.as_str()), ("RUNNING", "Title", 5, "D"));
        assert!(state_from_json(&j, "empty").is_none());
        assert!(state_from_json(&j, "nope").is_none());
        assert_eq!(state_from_json(&json!({"broadcasts": {"a": {"x": 1}}}), "a").unwrap().state, "?");
        let long = "x".repeat(200);
        assert_eq!(state_from_json(&json!({"broadcasts": {"a": {"status": long}}}), "a").unwrap().title.len(), 140);

        let mk = |st: &str, at: i64| BroadcastState { state: st.into(), started_at: at, title: st.into(), ..Default::default() };
        let l = newest_running(&[("old".into(), mk("RUNNING", 1)), ("ended".into(), mk("ENDED", 9)), ("new".into(), mk("RUNNING", 5))]);
        assert!(l.live);
        assert_eq!(l.broadcast_id, "new");
        assert!(!newest_running(&[("e".into(), mk("ENDED", 1))]).live);
    }
}
