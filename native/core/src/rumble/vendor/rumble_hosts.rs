//! Port of src/main/rumble/vendor/rumbleHosts.js (render lib/rumbleHosts.js): which of Rumble's hosts to ask for
//! a path, and in what order. Rumble decides per HOSTNAME what each one serves and changes it without notice.
//! Measured 2026-11-27: wn0 (which used to serve everything) resets every connection; web7 serves only
//! `/chat/api/*`; the rumble.com apex answers `/service.php` and `/embedJS/` with JSON to a plain request that
//! carries an honest User-Agent (a browser name gets Cloudflare's challenge) and challenges its HTML pages. We
//! never try to get past a challenge: no TLS impersonation, no scripted browser, no browser User-Agent.
//!
//! Three kinds of path, each with its own order and its own health:
//! - "chat" = `/chat/api/*` (stream, send, delete, moderation): web7, then wn0 in case it reopens.
//! - "api"  = `/service.php`, `/embedJS/`: the apex first, then wn0.
//! - "site" = everything else (HTML pages): the apex, then wn0. The apex usually challenges these, so callers
//!   treat a challenge as "not available from an app" rather than an error to fight.
//!
//! DIFFERENCE FROM THE JS: the JS list continues with web1..web27 (dead since 2026-10-27); this app stops at wn0
//! and web7. `fetch_from_rumble` also takes over what the JS passed as `fetchImpl` (both rumbleFetch variants
//! only differ in options, which are in `FetchOpts`) and adds the honest UA + Accept-Language.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::net::{self, FetchError, FetchOpts};
use crate::rumble::ua::get_user_agent;
use crate::util::now_ms;

/// The rumble.com apex. Only "api" and "site" paths use it.
pub const APEX: &str = "apex";

/// The numbered front-end hosts: `<name>.rumble.com`.
pub const RUMBLE_HOSTS: [&str; 2] = ["wn0", "web7"];

/// `https://<host>.rumble.com<path>`, or the apex.
pub fn host_url(host: &str, path: &str) -> String {
    if host == APEX { format!("https://rumble.com{path}") } else { format!("https://{host}.rumble.com{path}") }
}

fn base_hosts(kind: &str) -> &'static [&'static str] {
    match kind {
        "chat" => &["web7", "wn0"],
        _ => &[APEX, "wn0"],
    }
}

// A host that failed is skipped for this long (unless it was the last one that worked -- see hosts_for).
const DOWN_MS: i64 = 10 * 60_000;
// Hosts tried per request.
const MAX_TRIES: usize = 3;

struct Health {
    down: HashMap<String, i64>,      // "kind:host" -> skip until (ms)
    last_good: HashMap<String, String>, // kind -> host
}

static HEALTH: LazyLock<Mutex<Health>> = LazyLock::new(|| Mutex::new(Health { down: HashMap::new(), last_good: HashMap::new() }));

pub fn host_kind(path: &str) -> &'static str {
    if path.starts_with("/chat/api/") {
        "chat"
    } else if path.starts_with("/service.php") || path.starts_with("/embedJS/") {
        "api"
    } else {
        "site"
    }
}

/// Hosts to try for this kind, in order: the last one that worked first -- even if it has since failed once,
/// because one timeout from the host that has been serving everything is far likelier a blip than a lockdown --
/// then every other host that hasn't failed recently.
pub fn hosts_for(kind: &str) -> Vec<String> {
    hosts_for_at(kind, now_ms())
}

pub fn hosts_for_at(kind: &str, now: i64) -> Vec<String> {
    let h = HEALTH.lock().unwrap();
    let base = base_hosts(kind);
    let first = h.last_good.get(kind).cloned().unwrap_or_else(|| base[0].to_string());
    let mut out = vec![first.clone()];
    out.extend(base.iter().filter(|x| **x != first && h.down.get(&format!("{kind}:{x}")).copied().unwrap_or(0) <= now).map(|x| x.to_string()));
    out
}

pub fn mark_host(host: &str, kind: &str, ok: bool) {
    mark_host_at(host, kind, ok, now_ms());
}

pub fn mark_host_at(host: &str, kind: &str, ok: bool, now: i64) {
    let mut h = HEALTH.lock().unwrap();
    if ok {
        h.down.remove(&format!("{kind}:{host}"));
        h.last_good.insert(kind.to_string(), host.to_string());
    } else {
        h.down.insert(format!("{kind}:{host}"), now + DOWN_MS);
    }
}

/// For tests.
pub fn reset_host_health() {
    let mut h = HEALTH.lock().unwrap();
    h.down.clear();
    h.last_good.clear();
}

/// `fetch(host_url(host, path), init)` on the first host that answers. Errs with the last
/// failure when none did.
///
/// A write (anything but GET) that TIMED OUT is not retried on another host: it may have landed, and a chat
/// message sent twice is worse than one that reports an error. A refused connection never reached Rumble, so
/// it is safe to repeat anywhere.
pub async fn fetch_from_rumble(path: &str, mut init: FetchOpts) -> Result<reqwest::Response, FetchError> {
    let kind = host_kind(path);
    let write = !(init.method.is_empty() || init.method.eq_ignore_ascii_case("GET"));
    // rumbleFetch: honest UA + Accept-Language first, the caller's headers after.
    let mut headers = vec![("User-Agent".to_string(), get_user_agent()), ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string())];
    headers.append(&mut init.headers);
    init.headers = headers;
    let mut last: Option<FetchError> = None;
    for host in hosts_for(kind).into_iter().take(MAX_TRIES) {
        match net::fetch(&host_url(&host, path), init.clone()).await {
            Ok(res) => {
                mark_host(&host, kind, true);
                return Ok(res);
            }
            Err(e) => {
                if !e.refused && !e.timed_out {
                    return Err(e); // not a host problem
                }
                mark_host(&host, kind, false);
                let timed_out = !e.refused && e.timed_out;
                last = Some(e);
                if timed_out && write {
                    break;
                }
            }
        }
    }
    Err(last.unwrap_or(FetchError { message: "fetch failed".into(), timed_out: false, refused: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_follow_the_path() {
        assert_eq!(host_kind("/chat/api/chat/1/stream"), "chat");
        assert_eq!(host_kind("/chat/api/chat/1/message"), "chat");
        assert_eq!(host_kind("/service.php?name=user.has_unread_notifications"), "api");
        assert_eq!(host_kind("/embedJS/u3/?request=video&v=v1"), "api");
        assert_eq!(host_kind("/c/Foo"), "site");
        assert_eq!(host_kind("/chat/popup/1"), "site");
    }

    #[test]
    fn urls_and_default_orders() {
        assert_eq!(host_url(APEX, "/service.php?a=b"), "https://rumble.com/service.php?a=b");
        assert_eq!(host_url("web7", "/chat/api/x"), "https://web7.rumble.com/chat/api/x");
        assert!(RUMBLE_HOSTS.iter().all(|h| !h.is_empty() && *h != "www" && *h != "rumble"));
        // health is process-global; these kinds are not touched by the other test
        assert_eq!(base_hosts("chat"), ["web7", "wn0"]);
        assert_eq!(base_hosts("api"), [APEX, "wn0"]);
        assert_eq!(base_hosts("site"), [APEX, "wn0"]);
    }

    #[test]
    fn host_order_and_health() {
        // Health is process-global: this test is the only one touching "testkind".
        let k = "testkind";
        assert_eq!(hosts_for_at(k, 0), vec![APEX, "wn0"]);
        mark_host_at(APEX, k, false, 1000);
        // The default first host is kept first even after one failure; wn0 follows.
        assert_eq!(hosts_for_at(k, 2000), vec![APEX, "wn0"]);
        mark_host_at("wn0", k, true, 1000);
        assert_eq!(hosts_for_at(k, 2000), vec!["wn0"]); // the apex is down for 10 minutes
        assert_eq!(hosts_for_at(k, 1000 + DOWN_MS + 1), vec!["wn0", APEX]);
        mark_host_at(APEX, k, true, 5);
        assert_eq!(hosts_for_at(k, 0), vec![APEX, "wn0"]);
    }
}
