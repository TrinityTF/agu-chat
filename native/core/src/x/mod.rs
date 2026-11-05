//! Port of src/main/x/index.js: the X (Twitter) platform module (read-only). See docs/ARCHITECTURE.md.
//!
//! Reads x.com's live-chat stream (newline-delimited JSON, backfill first) with a GUEST token only: no
//! account, no bot session, no paid v2 API calls. Emits the frames /ws/x sends: chatlog {status, messages},
//! chat {messages}, status {status}, plus the uniform conn frame. Sending is not possible without X's paid
//! API, so canSend is always false.
//!
//! WHAT IS AND IS NOT POSSIBLE AS A GUEST (measured; see vendor/x_live.rs):
//!  * Given a broadcast id, everything works anonymously: state, title, chat history, live chat.
//!  * Getting the id from a handle is auth-gated at X's end. The public profile only carries a broadcast link
//!    if the streamer tweeted it, so a handle source finds the stream only then; pasting the broadcast link
//!    (x.com/i/broadcasts/<id>) always works.
//!  * Names: events carry only a user id, so chatters show as "X user <last 5 digits of the id>".
//!
//! Differences from the JS: timers are `util::Timer`s, the stream loop is a task owned by the current chat
//! (its AbortHandle is the chat's AbortController), `stop()` aborts the source's tasks (`net::Jobs`).
pub mod ua;
pub mod vendor;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};
use tokio::task::AbortHandle;

use crate::net::Jobs;
use crate::platforms::{Ctx, Emit, PlatformModule, Source};
use crate::protocol::{Frame, Platform};
use crate::util::{js_str_or_empty, js_string, js_trim, now_ms, truthy, Timer};
use ua::set_user_agent;
use vendor::x_chat::{read_live_chat, to_x_chat_msg};
use vendor::x_live::{broadcast_id_in_url, broadcast_states, find_live, normalize_handle, FindLive};

const MAX_BACKLOG: usize = 300;
// x.com's own reconnect delays: a second after a clean end, thirty after an error answer. 503 is X shedding load.
const STREAM_RETRY_MS: u64 = 1000;
const STREAM_ERROR_RETRY_MS: u64 = 30_000;
// A stream that ends this soon without a single event is not a clean end, whatever its status said.
const STREAM_MIN_HEALTHY_MS: i64 = 3000;
const STREAM_FAILS_BEFORE_ERROR: u32 = 3;
// How often a live broadcast is asked whether it is still RUNNING.
const LIVE_CHECK_MS: u64 = 30_000;
const OFFLINE_POLL_MS: u64 = 60_000;
const MAX_RECHECK_MS: u64 = 5 * 60_000;
// Events are held this long so the ~100-message backfill arrives as one chatlog rather than 100 frames.
const BATCH_MS: u64 = 250;

const READ_ONLY_REASON: &str = "X is read-only (sending needs X's paid API)";
const NAMES_NOTICE: &str = "X does not give guests chatter names; they show as X user + the last digits of their id.";

/// "broadcast:<id>" for a broadcast link/id, otherwise the lowercased X username.
pub fn normalize_channel(input: &str) -> Result<String, String> {
    let raw = js_trim(input);
    if raw.is_empty() {
        return Err("Enter an X username or a broadcast link.".into());
    }
    if let Some(id) = raw.strip_prefix("broadcast:").filter(|id| (13..=14).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())) {
        return Ok(format!("broadcast:{id}"));
    }
    // Only a URL counts as a broadcast: a bare 13-character word is far likelier a username.
    if let Some(id) = broadcast_id_in_url(raw) {
        return Ok(format!("broadcast:{id}"));
    }
    let handle = normalize_handle(raw);
    if handle.is_empty() {
        return Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into());
    }
    Ok(handle.to_lowercase())
}

pub struct X;

impl PlatformModule for X {
    fn platform(&self) -> Platform {
        Platform::X
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        normalize_channel(input)
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        set_user_agent(&ctx.user_agent);
        // JS normalizes again here; the relay hands over an already normalized channel.
        let norm = normalize_channel(&channel).unwrap_or(channel);
        XSource::new(norm, emit, ctx)
    }
}

struct Chat {
    id: u64,
    broadcast_id: String,
    cursor: String,
    fails: u32,
    hello: bool,
    task: Option<AbortHandle>,
}

struct St {
    status: Map<String, Value>,
    backlog: Vec<Value>,
    seen: HashSet<String>,
    chat: Option<Chat>,
    next_chat: u64,
    pending: Vec<Value>,
    pending_timer: Timer,
    pending_armed: bool,
    poll_timer: Timer,
    polling: bool,
    soft_misses: u32,
    resolved_once: bool,
    stopped: bool,
    last_conn_json: String,
}

pub struct XSource {
    norm: String,
    pinned: String,
    handle: String,
    emit_fn: Emit,
    ctx: Arc<Ctx>,
    jobs: Jobs,
    st: Mutex<St>,
}

fn conn_frame(st: &St, pinned: bool) -> Value {
    let s = &st.status;
    let error = js_str_or_empty(s.get("error"));
    let state = js_str_or_empty(s.get("state"));
    let live = truthy(&s["live"]);
    let (st_name, detail) = if !st.resolved_once {
        ("connecting", "Looking for a live X broadcast...".to_string())
    } else if !error.is_empty() {
        ("error", error)
    } else if live {
        ("open", "Live".to_string())
    } else {
        (
            "offline",
            if pinned {
                if !state.is_empty() && state != "RUNNING" { format!("This broadcast is not running ({}).", state.to_lowercase()) } else { "This broadcast is not running.".to_string() }
            } else {
                "Not live. X only reveals a broadcast from a handle when the streamer tweets the link: paste the broadcast link to follow it directly.".to_string()
            },
        )
    };
    let mut f = json!({"type": "conn", "state": st_name, "detail": detail, "live": live});
    let title = js_str_or_empty(s.get("title"));
    if !title.is_empty() {
        f["title"] = json!(title);
    }
    f["canSend"] = json!(false);
    f["readOnlyReason"] = json!(READ_ONLY_REASON);
    f
}

fn chatlog_of(st: &St) -> Value {
    json!({"type": "chatlog", "status": Value::Object(st.status.clone()), "messages": st.backlog})
}

impl XSource {
    fn new(norm: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        let pinned = norm.strip_prefix("broadcast:").unwrap_or("").to_string();
        let handle = if pinned.is_empty() { norm.clone() } else { String::new() };
        let mut status = Map::new();
        status.insert("live".into(), json!(false));
        status.insert("broadcastId".into(), Value::Null);
        status.insert("handle".into(), json!(handle));
        status.insert("title".into(), json!(""));
        status.insert("url".into(), json!(""));
        status.insert("state".into(), json!(""));
        status.insert("error".into(), Value::Null);
        status.insert("notice".into(), Value::Null);
        Arc::new(XSource {
            norm,
            pinned,
            handle,
            emit_fn: emit,
            ctx,
            jobs: Jobs::new(),
            st: Mutex::new(St {
                status,
                backlog: Vec::new(),
                seen: HashSet::new(),
                chat: None,
                next_chat: 1,
                pending: Vec::new(),
                pending_timer: Timer::new(),
                pending_armed: false,
                poll_timer: Timer::new(),
                polling: false,
                soft_misses: 0,
                resolved_once: false,
                stopped: false,
                last_conn_json: String::new(),
            }),
        })
    }

    fn emit(&self, f: Frame) {
        (self.emit_fn)(f);
    }

    fn stopped(&self) -> bool {
        self.st.lock().unwrap().stopped
    }

    fn is_current(&self, id: u64) -> bool {
        self.st.lock().unwrap().chat.as_ref().map(|c| c.id) == Some(id)
    }

    fn recheck_delay(&self) -> u64 {
        let sm = self.st.lock().unwrap().soft_misses.min(3);
        (OFFLINE_POLL_MS * 2u64.pow(sm)).min(MAX_RECHECK_MS)
    }

    fn publish_conn(&self) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st, !self.pinned.is_empty());
            let j = f.to_string();
            if j == st.last_conn_json {
                return;
            }
            st.last_conn_json = j;
            f
        };
        self.emit(f);
    }

    fn set_status(&self, next: Vec<(&str, Value)>) {
        let changed = {
            let mut st = self.st.lock().unwrap();
            let mut changed = false;
            for (k, v) in next {
                if st.status.get(k) != Some(&v) {
                    changed = true;
                    st.status.insert(k.to_string(), v);
                }
            }
            changed.then(|| Value::Object(st.status.clone()))
        };
        if let Some(status) = changed {
            self.emit(json!({"type": "status", "status": status}));
        }
        self.publish_conn();
    }

    // ---------- resolution ----------

    /// Which broadcast should this source be reading? A pinned id is the only reliable answer.
    /// Returns (found, state).
    async fn resolve(&self) -> Result<(FindLive, String), String> {
        if !self.pinned.is_empty() {
            let states = broadcast_states(&[self.pinned.clone()]).await?;
            let s = states.into_iter().find(|(id, _)| *id == self.pinned).map(|(_, s)| s);
            return Ok(match s {
                Some(s) if s.state == "RUNNING" => (FindLive { live: true, broadcast_id: self.pinned.clone(), title: s.title }, String::new()),
                other => (FindLive::default(), other.map(|s| s.state).unwrap_or_default()),
            });
        }
        // Free when it works, which is only when the streamer tweets the link.
        let found = find_live(&self.handle).await?;
        Ok(if found.live { (found, String::new()) } else { (FindLive::default(), String::new()) })
    }

    async fn tick_resolve(self: &Arc<Self>) -> u64 {
        let found = match self.resolve().await {
            Ok(f) => f,
            Err(e) => {
                if self.stopped() {
                    return 0;
                }
                {
                    let mut st = self.st.lock().unwrap();
                    st.soft_misses += 1;
                    st.resolved_once = true;
                }
                self.set_status(vec![("live", json!(false)), ("error", json!(if e.is_empty() { "lookup failed".to_string() } else { e })), ("notice", Value::Null)]);
                return self.recheck_delay();
            }
        };
        if self.stopped() {
            return 0;
        }
        self.st.lock().unwrap().resolved_once = true;
        let (found, state) = found;

        if !found.live || found.broadcast_id.is_empty() {
            // NOT a soft miss: "they are not streaming" is the ordinary answer, and backing off on it would delay
            // noticing a stream that starts. Title and id are cleared so yesterday's doesn't read as running.
            self.st.lock().unwrap().soft_misses = 0;
            self.set_status(vec![
                ("live", json!(false)),
                ("broadcastId", Value::Null),
                ("title", json!("")),
                ("state", json!(state)),
                ("url", json!("")),
                ("error", Value::Null),
                ("notice", Value::Null),
            ]);
            return OFFLINE_POLL_MS;
        }

        let chat_id = {
            let mut st = self.st.lock().unwrap();
            st.soft_misses = 0;
            let cur = js_str_or_empty(st.status.get("broadcastId"));
            if !cur.is_empty() && cur != found.broadcast_id {
                st.backlog.clear();
                st.seen.clear();
            }
            let id = st.next_chat;
            st.next_chat += 1;
            st.chat = Some(Chat { id, broadcast_id: found.broadcast_id.clone(), cursor: String::new(), fails: 0, hello: false, task: None });
            id
        };
        self.set_status(vec![
            ("live", json!(true)),
            ("broadcastId", json!(found.broadcast_id)),
            ("title", json!(found.title)),
            ("state", json!("RUNNING")),
            ("url", json!(format!("https://x.com/i/broadcasts/{}", found.broadcast_id))),
            ("error", Value::Null),
            ("notice", json!(NAMES_NOTICE)),
        ]);
        self.start_stream(chat_id, found.broadcast_id);
        LIVE_CHECK_MS
    }

    /// Reading a live broadcast: is it still running? Only an explicit answer ends it.
    async fn tick_live(self: &Arc<Self>, chat_id: u64, broadcast_id: String) -> u64 {
        let mut s = None;
        match broadcast_states(&[broadcast_id.clone()]).await {
            Ok(states) => s = states.into_iter().find(|(id, _)| *id == broadcast_id).map(|(_, s)| s),
            Err(e) => self.ctx.log(&format!("[x:{}] state check failed: {e}", self.norm)),
        }
        if self.stopped() || !self.is_current(chat_id) {
            return 0;
        }
        if let Some(s) = s.filter(|s| !s.state.is_empty() && s.state != "RUNNING") {
            self.drop_chat();
            self.set_status(vec![
                ("live", json!(false)),
                ("broadcastId", Value::Null),
                ("title", json!("")),
                ("state", json!(s.state)),
                ("url", json!("")),
                ("error", Value::Null),
                ("notice", Value::Null),
            ]);
            // A handle source goes straight back to resolving, which picks up a follow-on stream.
            return if self.pinned.is_empty() { 0 } else { OFFLINE_POLL_MS };
        }
        LIVE_CHECK_MS
    }

    async fn tick(self: Arc<Self>) {
        let cur = {
            let mut st = self.st.lock().unwrap();
            if st.stopped || st.polling {
                return;
            }
            st.polling = true;
            st.chat.as_ref().map(|c| (c.id, c.broadcast_id.clone()))
        };
        let next = match cur {
            Some((id, b)) => self.tick_live(id, b).await,
            None => self.tick_resolve().await,
        };
        let stopped = {
            let mut st = self.st.lock().unwrap();
            st.polling = false;
            st.stopped
        };
        if !stopped {
            self.schedule(next);
        }
    }

    /// A sync fn on purpose: building the timer future inside `tick` would make its type recursive.
    fn schedule(self: &Arc<Self>, ms: u64) {
        let s = self.clone();
        self.st.lock().unwrap().poll_timer.set(Duration::from_millis(ms), async move {
            let s2 = s.clone();
            s.jobs.spawn(s2.tick());
        });
    }

    // ---------- the chat stream ----------

    fn start_stream(self: &Arc<Self>, chat_id: u64, broadcast_id: String) {
        let s = self.clone();
        let h = tokio::spawn(async move { s.stream_loop(chat_id, broadcast_id).await });
        let mut st = self.st.lock().unwrap();
        let stopped = st.stopped;
        match st.chat.as_mut().filter(|c| c.id == chat_id) {
            Some(c) if !stopped => c.task = Some(h.abort_handle()),
            _ => h.abort(),
        }
    }

    /// Keep X's live-chat stream open while this chat is current, reconnecting the way x.com does.
    async fn stream_loop(self: Arc<Self>, chat_id: u64, broadcast_id: String) {
        while self.is_current(chat_id) {
            let opened_at = now_ms();
            let cursor = self.st.lock().unwrap().chat.as_ref().map(|c| c.cursor.clone()).unwrap_or_default();
            let events = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let mut delay = STREAM_RETRY_MS;
            let (s2, ev2) = (self.clone(), events.clone());
            let r = read_live_chat(&broadcast_id, &cursor, move |ev| {
                ev2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                s2.on_live_event(chat_id, ev);
            })
            .await;
            let r = r.and_then(|_| {
                if events.load(std::sync::atomic::Ordering::SeqCst) == 0 && now_ms() - opened_at < STREAM_MIN_HEALTHY_MS {
                    Err(vendor::x_chat::XError { message: "live-chat ended at once".into(), status: None })
                } else {
                    Ok(())
                }
            });
            match r {
                Ok(()) => self.stream_recovered(chat_id),
                Err(e) => {
                    if !self.is_current(chat_id) {
                        break;
                    }
                    let fails = {
                        let mut st = self.st.lock().unwrap();
                        match st.chat.as_mut() {
                            Some(c) => {
                                c.fails += 1;
                                c.fails
                            }
                            None => break,
                        }
                    };
                    delay = if e.status == Some(503) { STREAM_RETRY_MS } else { STREAM_ERROR_RETRY_MS.min(STREAM_RETRY_MS * 2u64.pow(fails.min(20))) };
                    self.ctx.log(&format!("[x:{}] live-chat failed ({fails}): {}", self.norm, e.message));
                    if fails >= STREAM_FAILS_BEFORE_ERROR {
                        self.set_status(vec![("error", json!(format!("X chat unavailable: {}", e.message)))]);
                    }
                }
            }
            if !self.is_current(chat_id) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }

    fn stream_recovered(&self, chat_id: u64) {
        let clear = {
            let mut st = self.st.lock().unwrap();
            let Some(c) = st.chat.as_mut().filter(|c| c.id == chat_id) else { return };
            if c.fails == 0 {
                return;
            }
            c.fails = 0;
            js_str_or_empty(st.status.get("error")).starts_with("X chat unavailable")
        };
        if clear {
            self.set_status(vec![("error", Value::Null)]);
        }
    }

    fn on_live_event(self: &Arc<Self>, chat_id: u64, ev: Value) {
        if !ev.is_object() {
            return;
        }
        {
            let mut st = self.st.lock().unwrap();
            let Some(c) = st.chat.as_mut().filter(|c| c.id == chat_id) else { return };
            // The cursor is the last `ts` seen, whatever kind of event carried it -- that is what x.com reconnects from.
            if truthy(&ev["ts"]) {
                let ts = js_string(&ev["ts"]);
                if c.cursor.is_empty() || ts > c.cursor {
                    c.cursor = ts;
                }
            }
        }
        self.stream_recovered(chat_id);
        if ev["chatType"] != 1 || !truthy(&ev["message"]) || !truthy(&ev["userId"]) {
            return;
        }
        let mut st = self.st.lock().unwrap();
        st.pending.push(ev);
        if !st.pending_armed {
            st.pending_armed = true;
            let s = self.clone();
            st.pending_timer.set(Duration::from_millis(BATCH_MS), async move {
                s.st.lock().unwrap().pending_armed = false;
                s.flush(chat_id);
            });
        }
    }

    fn flush(&self, chat_id: u64) {
        let frame = {
            let mut st = self.st.lock().unwrap();
            let batch = std::mem::take(&mut st.pending);
            if batch.is_empty() || st.chat.as_ref().map(|c| c.id) != Some(chat_id) {
                return;
            }
            let mut fresh: Vec<Value> = Vec::new();
            for ev in &batch {
                let Some(msg) = to_x_chat_msg(ev) else { continue };
                if !st.seen.insert(js_string(&msg["id"])) {
                    continue;
                }
                fresh.push(msg);
            }
            fresh.sort_by_key(|m| m["ts"].as_i64().unwrap_or(0));
            st.backlog.extend(fresh.iter().cloned());
            if st.backlog.len() > MAX_BACKLOG {
                let cut = st.backlog.len() - MAX_BACKLOG;
                st.backlog.drain(..cut);
            }
            if st.seen.len() > MAX_BACKLOG * 4 {
                st.seen = st.backlog.iter().map(|m| js_string(&m["id"])).collect();
            }
            let hello = st.chat.as_ref().map(|c| c.hello).unwrap_or(true);
            if !hello {
                // The first batch is the backfill: one chatlog.
                st.chat.as_mut().unwrap().hello = true;
                Some(chatlog_of(&st))
            } else if !fresh.is_empty() {
                Some(json!({"type": "chat", "messages": fresh}))
            } else {
                None
            }
        };
        if let Some(f) = frame {
            self.emit(f);
        }
    }

    fn drop_chat(&self) {
        let task = {
            let mut st = self.st.lock().unwrap();
            let t = st.chat.take().and_then(|c| c.task);
            st.pending.clear();
            st.pending_timer.clear();
            st.pending_armed = false;
            t
        };
        if let Some(t) = task {
            t.abort();
        }
    }
}

impl Source for XSource {
    fn start(self: Arc<Self>) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st, !self.pinned.is_empty());
            st.last_conn_json = f.to_string();
            f
        };
        self.emit(f);
        let s = self.clone();
        self.jobs.spawn(s.tick());
    }

    fn stop(&self) {
        self.st.lock().unwrap().stopped = true;
        self.drop_chat();
        self.st.lock().unwrap().poll_timer.clear();
        self.jobs.abort_all();
    }

    fn snapshot(&self) -> Vec<Frame> {
        let st = self.st.lock().unwrap();
        vec![chatlog_of(&st), conn_frame(&st, !self.pinned.is_empty())]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_channel_forms() {
        let n = |s: &str| normalize_channel(s);
        let id = "1DGleVdEapOJL";
        let id14 = "2qVWZNBJRbVaJN";
        assert_eq!(n("EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("@EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("https://x.com/EsfandTV"), Ok("esfandtv".into()));
        assert_eq!(n("twitter.com/EsfandTV/"), Ok("esfandtv".into()));
        assert_eq!(n(&format!("https://x.com/i/broadcasts/{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("x.com/i/broadcasts/{id14}?s=20")), Ok(format!("broadcast:{id14}")));
        assert_eq!(n(&format!("https://twitter.com/broadcasts/{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("broadcast:{id}")), Ok(format!("broadcast:{id}")));
        assert_eq!(n(&format!("broadcast:{id14}")), Ok(format!("broadcast:{id14}")));
        // a bare 13-character word is a username, not a broadcast
        assert_eq!(n(id), Ok(id.to_lowercase()));
        // a 15-character id is not a broadcast; the JS then reads the URL as the profile "i" (x.com/i), and so does this
        assert_eq!(n(&format!("https://x.com/i/broadcasts/{id}XX")), Ok("i".into()));
        assert_eq!(n("broadcast:short"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        assert_eq!(n(""), Err("Enter an X username or a broadcast link.".into()));
        assert_eq!(n("   "), Err("Enter an X username or a broadcast link.".into()));
        assert_eq!(n("has space"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        assert_eq!(n("this_name_is_way_too_long"), Err("Enter an X username or a broadcast link (x.com/i/broadcasts/...).".into()));
        // stable
        assert_eq!(n("esfandtv"), Ok("esfandtv".into()));
        assert_eq!(n(&format!("broadcast:{id}")), Ok(format!("broadcast:{id}")));
    }
}
