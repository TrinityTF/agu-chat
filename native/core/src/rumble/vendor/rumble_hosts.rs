//! Port of src/main/rumble/vendor/rumbleHosts.js (render lib/rumbleHosts.js): which of Rumble's front-end
//! hosts to ask, and in what order. Every Rumble request goes to a front-end host rather than the rumble.com
//! apex, which answers non-browser clients with Cloudflare's challenge. We never try to get past it.
//!
//! Health is tracked per kind ("chat" = /chat/api/*, "site" = everything else): web7 failing a channel page
//! must not stop it being used for chat.
//!
//! DIFFERENCE FROM THE JS: the JS list continues with web1..web27 (dead since 2026-10-27, tried only after
//! wn0 and web7 fail). The binding rule for this app is "only wn0, then web7 for chat", so the list stops at
//! those two. `fetch_from_rumble` also takes over what the JS passed as `fetchImpl` (both rumbleFetch
//! variants only differ in options, which are in `FetchOpts`) and adds the honest UA + Accept-Language.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::net::{self, FetchError, FetchOpts};
use crate::rumble::ua::get_user_agent;
use crate::util::now_ms;

pub const RUMBLE_HOSTS: [&str; 2] = ["wn0", "web7"];

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
    if path.starts_with("/chat/api/") { "chat" } else { "site" }
}

/// Hosts to try for this kind, in order: the last one that worked first -- even if it has since failed once,
/// because one timeout from the host that has been serving everything is far likelier a blip than a lockdown --
/// then every other host that hasn't failed recently.
pub fn hosts_for(kind: &str) -> Vec<String> {
    hosts_for_at(kind, now_ms())
}

pub fn hosts_for_at(kind: &str, now: i64) -> Vec<String> {
    let h = HEALTH.lock().unwrap();
    let first = h.last_good.get(kind).cloned().unwrap_or_else(|| RUMBLE_HOSTS[0].to_string());
    let mut out = vec![first.clone()];
    out.extend(RUMBLE_HOSTS.iter().filter(|x| **x != first && h.down.get(&format!("{kind}:{x}")).copied().unwrap_or(0) <= now).map(|x| x.to_string()));
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

/// `fetch("https://<host>.rumble.com" + path, init)` on the first host that answers. Errs with the last
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
        match net::fetch(&format!("https://{host}.rumble.com{path}"), init.clone()).await {
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
    fn only_wn0_and_web7_never_the_apex() {
        assert_eq!(RUMBLE_HOSTS, ["wn0", "web7"]);
        assert!(RUMBLE_HOSTS.iter().all(|h| !h.is_empty() && *h != "www" && *h != "rumble"));
    }

    #[test]
    fn host_order_and_health() {
        // Health is process-global: this test is the only one touching "testkind".
        let k = "testkind";
        assert_eq!(hosts_for_at(k, 0), vec!["wn0", "web7"]);
        mark_host_at("wn0", k, false, 1000);
        // The default first host is kept first even after one failure; web7 follows.
        assert_eq!(hosts_for_at(k, 2000), vec!["wn0", "web7"]);
        mark_host_at("web7", k, true, 1000);
        assert_eq!(hosts_for_at(k, 2000), vec!["web7"]); // wn0 is down for 10 minutes
        assert_eq!(hosts_for_at(k, 1000 + DOWN_MS + 1), vec!["web7", "wn0"]);
        mark_host_at("wn0", k, true, 5);
        assert_eq!(hosts_for_at(k, 0), vec!["wn0", "web7"]);
        assert_eq!(host_kind("/chat/api/chat/1/stream"), "chat");
        assert_eq!(host_kind("/service.php"), "site");
    }
}
