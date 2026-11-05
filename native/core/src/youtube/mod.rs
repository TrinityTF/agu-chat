//! Port of src/main/youtube/index.js: the YouTube platform module (read-only). See docs/ARCHITECTURE.md.
//!
//! Emits the frames the agu.lol /ws/youtube relay sends, so the row code works unchanged:
//!   {type:"chatlog", source, status, messages}, {type:"chat", messages:[...]}, {type:"status", status}
//! plus the uniform {type:"conn"} frame.
//!
//! One poll loop per source, paced by YouTube's own timeoutMs; repeated failure backs off exponentially so a
//! deleted channel or a YouTube outage cannot become a fetch loop. Sending is not supported.
//!
//! Differences from the JS: the poll timer is a `util::Timer`, and `stop()` aborts the source's tasks
//! (`net::Jobs`) instead of an `AbortController`.
pub mod vendor;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::net::Jobs;
use crate::platforms::{Ctx, Emit, PlatformModule, Source};
use crate::protocol::{Frame, Platform};
use crate::util::{js_str_or_empty, now_ms, truthy, Timer};
use vendor::youtube::{fetch_metadata, parse_youtube_source, poll_chat, read_watch_page, source_string, to_youtube_chat_msg, youtube_source_url, ChatState, YtSource};

const MAX_BACKLOG: usize = 300;
// How often to re-check an offline channel for a new broadcast: a ~1 MB page fetch, so deliberately slow.
const OFFLINE_POLL_MS: u64 = 60_000;
// Only used when YouTube declines to tell us; its own timeoutMs is always preferred.
const FALLBACK_POLL_MS: u64 = 10_000;
const MAX_BACKOFF_MS: u64 = 5 * 60_000;
// The title/viewer count is decoration; a minute is roughly right for a viewer counter.
const METADATA_TTL_MS: i64 = 60_000;
// How soon to look again after one unconvincing answer. Only the FIRST retry is quick: if YouTube keeps
// giving unconvincing answers it is likely unhappy with us, and rechecking hard is the wrong response.
const RECHECK_MS: u64 = 12_000;
const MAX_RECHECK_MS: u64 = 2 * 60_000;

const READ_ONLY_REASON: &str = "YouTube is read-only in this beta";

pub struct Youtube;

impl PlatformModule for Youtube {
    fn platform(&self) -> Platform {
        Platform::Youtube
    }

    fn normalize_channel(&self, input: &str) -> Result<String, String> {
        parse_youtube_source(input).map(|s| source_string(&s)).ok_or_else(|| "Enter a YouTube @handle, channel id, or a channel / stream link.".to_string())
    }

    fn create_source(&self, channel: String, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        // JS throws "Not a YouTube source" here; the relay has already normalized, so this only guards a stray input.
        let src = parse_youtube_source(&channel).unwrap_or(YtSource { kind: "handle", id: channel.clone() });
        YoutubeSource::new(channel, src, emit, ctx)
    }
}

struct St {
    status: Map<String, Value>,
    phase: String, // connecting | open | offline | unavailable | error
    backlog: Vec<Value>,
    seen: HashSet<String>,
    chat: Option<ChatState>,
    meta_at: i64,
    offline_streak: u32,
    soft_misses: u32,
    failures: u32,
    timer: Timer,
    polling: bool,
    stopped: bool,
    last_conn_json: String,
}

pub struct YoutubeSource {
    channel: String,
    src: YtSource,
    emit_fn: Emit,
    ctx: Arc<Ctx>,
    jobs: Jobs,
    st: Mutex<St>,
}

fn conn_frame(st: &St) -> Value {
    let s = &st.status;
    let notice = js_str_or_empty(s.get("notice"));
    let error = js_str_or_empty(s.get("error"));
    let detail = match st.phase.as_str() {
        "open" => "Live".to_string(),
        "offline" => if notice.is_empty() { "Not live".into() } else { notice },
        "unavailable" => if notice.is_empty() { "Chat is unavailable for this stream.".into() } else { notice },
        "error" => if error.is_empty() { "YouTube unreachable".into() } else { error },
        _ => "Connecting to YouTube...".into(),
    };
    let mut f = json!({"type": "conn", "state": st.phase, "detail": detail, "live": s["live"]});
    let title = js_str_or_empty(s.get("title"));
    if !title.is_empty() {
        f["title"] = json!(title);
    }
    f["canSend"] = json!(false);
    f["readOnlyReason"] = json!(READ_ONLY_REASON);
    f
}

fn chatlog_of(src: &YtSource, st: &St) -> Value {
    json!({"type": "chatlog", "source": {"kind": src.kind, "id": src.id}, "status": Value::Object(st.status.clone()), "messages": st.backlog})
}

impl YoutubeSource {
    fn new(channel: String, src: YtSource, emit: Emit, ctx: Arc<Ctx>) -> Arc<dyn Source> {
        let mut status = Map::new();
        status.insert("live".into(), json!(false));
        status.insert("videoId".into(), Value::Null);
        status.insert("category".into(), json!(""));
        status.insert("channelId".into(), json!(""));
        status.insert("author".into(), json!(""));
        status.insert("title".into(), json!(""));
        status.insert("viewers".into(), Value::Null);
        status.insert("url".into(), json!(youtube_source_url(&src)));
        status.insert("notice".into(), Value::Null);
        status.insert("error".into(), Value::Null);
        Arc::new(YoutubeSource {
            channel,
            src,
            emit_fn: emit,
            ctx,
            jobs: Jobs::new(),
            st: Mutex::new(St {
                status,
                phase: "connecting".into(),
                backlog: Vec::new(),
                seen: HashSet::new(),
                chat: None,
                meta_at: 0,
                offline_streak: 0,
                soft_misses: 0,
                failures: 0,
                timer: Timer::new(),
                polling: false,
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

    fn recheck_delay(&self) -> u64 {
        let sm = self.st.lock().unwrap().soft_misses.min(3);
        (RECHECK_MS * 2u64.pow(sm)).min(MAX_RECHECK_MS)
    }

    // Emit the conn frame only when it changed, so a quiet poll loop does not spam the renderer.
    fn publish_conn(&self) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st);
            let j = f.to_string();
            if j == st.last_conn_json {
                return;
            }
            st.last_conn_json = j;
            f
        };
        self.emit(f);
    }

    fn set_status(&self, next: Vec<(&str, Value)>, next_phase: Option<&str>) {
        let changed = {
            let mut st = self.st.lock().unwrap();
            let mut changed = false;
            for (k, v) in next {
                if st.status.get(k) != Some(&v) {
                    changed = true;
                    st.status.insert(k.to_string(), v);
                }
            }
            if let Some(p) = next_phase {
                st.phase = p.to_string();
            }
            changed.then(|| Value::Object(st.status.clone()))
        };
        if let Some(status) = changed {
            self.emit(json!({"type": "status", "status": status}));
        }
        self.publish_conn();
    }

    fn chatlog_frame(&self) -> Value {
        let st = self.st.lock().unwrap();
        chatlog_of(&self.src, &st)
    }

    /// Normalize actions into rows, de-duplicated (bootstrap backlog and first poll overlap).
    fn ingest(&self, actions: &[Value]) -> Vec<Value> {
        let mut st = self.st.lock().unwrap();
        let mut fresh = Vec::new();
        for a in actions {
            let Some(msg) = to_youtube_chat_msg(a) else { continue };
            let id = js_str_or_empty(Some(&msg["id"]));
            if id.is_empty() || !st.seen.insert(id) {
                continue;
            }
            fresh.push(msg);
        }
        if fresh.is_empty() {
            return fresh;
        }
        st.backlog.extend(fresh.iter().cloned());
        if st.backlog.len() > MAX_BACKLOG {
            let cut = st.backlog.len() - MAX_BACKLOG;
            st.backlog.drain(..cut);
        }
        // Bounded alongside the backlog: without this the id set grows for as long as the source lives.
        if st.seen.len() > MAX_BACKLOG * 4 {
            st.seen = st.backlog.iter().map(|m| js_str_or_empty(Some(&m["id"]))).collect();
        }
        fresh
    }

    async fn refresh_metadata(&self) {
        let chat = {
            let mut st = self.st.lock().unwrap();
            if st.chat.is_none() || now_ms() - st.meta_at < METADATA_TTL_MS {
                return;
            }
            st.meta_at = now_ms();
            st.chat.clone().unwrap()
        };
        // Decoration on a working chat; a metadata hiccup must not back off the poll.
        if let Ok(meta) = fetch_metadata(&chat, &self.ctx.user_agent).await {
            if self.stopped() {
                return;
            }
            let (cur_title, cur_viewers) = {
                let st = self.st.lock().unwrap();
                (st.status["title"].clone(), st.status["viewers"].clone())
            };
            self.set_status(
                vec![
                    ("title", if meta.title.is_empty() { cur_title } else { json!(meta.title) }),
                    ("viewers", match meta.viewers {
                        Some(v) => json!(v),
                        None => cur_viewers,
                    }),
                ],
                None,
            );
        }
    }

    /// Returns the delay before the next tick (ms); 0 when stopped.
    async fn poll_step(&self, first: bool) -> Result<u64, String> {
        let chat = self.st.lock().unwrap().chat.clone().expect("chat");
        let res = poll_chat(&chat, &self.ctx.user_agent).await?;
        if self.stopped() {
            return Ok(0);
        }
        let Some(res) = res.filter(|r| !r.continuation.is_empty()) else {
            // Chat ended or the token expired. Re-resolve promptly rather than declaring the channel offline here:
            // an expired continuation on a still-running stream is common, and the resolve decides.
            {
                let mut st = self.st.lock().unwrap();
                st.chat = None;
                st.seen.clear();
                st.soft_misses += 1;
            }
            return Ok(self.recheck_delay());
        };
        if let Some(c) = self.st.lock().unwrap().chat.as_mut() {
            c.continuation = res.continuation.clone();
        }
        let fresh = self.ingest(&res.actions);
        // The very first poll is the history: one chatlog instead of ~75 chat frames.
        if first {
            let f = self.chatlog_frame();
            self.emit(f);
        } else if !fresh.is_empty() {
            self.emit(json!({"type": "chat", "messages": fresh}));
        }
        self.set_status(vec![("error", Value::Null), ("notice", Value::Null)], Some("open"));
        self.refresh_metadata().await;
        {
            let mut st = self.st.lock().unwrap();
            st.failures = 0;
            st.soft_misses = 0;
        }
        Ok(if res.timeout_ms > 0 { res.timeout_ms } else { FALLBACK_POLL_MS })
    }

    async fn resolve_step(&self) -> Result<u64, String> {
        let res = read_watch_page(&self.src, &self.ctx.user_agent).await?;
        if self.stopped() {
            return Ok(0);
        }

        if !res.exists {
            let msg = if self.src.kind == "video" { "No such YouTube video." } else { "No such YouTube channel." };
            self.set_status(vec![("live", json!(false)), ("videoId", Value::Null), ("error", json!(msg)), ("notice", Value::Null)], Some("error"));
            return Ok(MAX_BACKOFF_MS);
        }
        if res.blocked {
            self.st.lock().unwrap().soft_misses += 1;
            let err = res.error.clone().unwrap_or_else(|| "YouTube served a consent or bot-check page instead of the channel.".into());
            self.set_status(vec![("live", json!(false)), ("error", json!(err)), ("notice", Value::Null)], Some("error"));
            return Ok(self.recheck_delay());
        }
        let (cur_channel_id, cur_author, cur_title, cur_video) = {
            let st = self.st.lock().unwrap();
            (st.status["channelId"].clone(), st.status["author"].clone(), st.status["title"].clone(), st.status["videoId"].clone())
        };
        let or_cur = |new: &str, cur: &Value| if new.is_empty() { cur.clone() } else { json!(new) };
        if !res.live {
            let (streak, was_live) = {
                let mut st = self.st.lock().unwrap();
                st.offline_streak += 1;
                st.soft_misses += 1;
                (st.offline_streak, truthy(&st.status["live"]))
            };
            // One negative answer is not proof of going offline (the served page shape can vary between requests).
            if was_live && streak < 2 {
                return Ok(self.recheck_delay());
            }
            self.set_status(
                vec![
                    ("live", json!(false)),
                    ("videoId", if res.video_id.is_empty() { Value::Null } else { json!(res.video_id) }),
                    ("channelId", or_cur(&res.channel_id, &cur_channel_id)),
                    ("author", or_cur(&res.author, &cur_author)),
                    // Cleared on purpose: a stale title and viewer count would read as though that broadcast were running.
                    ("title", json!("")),
                    ("viewers", Value::Null),
                    ("notice", if self.src.kind == "video" { json!("This video is not a live stream right now.") } else { Value::Null }),
                    ("error", Value::Null),
                ],
                Some("offline"),
            );
            return Ok(OFFLINE_POLL_MS);
        }

        self.st.lock().unwrap().offline_streak = 0;
        let base = vec![
            ("live", json!(true)),
            ("videoId", json!(res.video_id)),
            ("channelId", or_cur(&res.channel_id, &cur_channel_id)),
            ("author", or_cur(&res.author, &cur_author)),
            ("url", json!(format!("https://www.youtube.com/watch?v={}", res.video_id))),
        ];
        if truthy(&cur_video) && cur_video != json!(res.video_id) {
            // A different broadcast: the old chat's rows belong to another video.
            let mut st = self.st.lock().unwrap();
            st.backlog.clear();
            st.seen.clear();
        }
        let title = or_cur(&res.title, &cur_title);
        if res.continuation.is_empty() {
            // Live, but no chat. YouTube's own words are shown, and NOT latched for a full offline poll: the same
            // stream can bootstrap fine seconds later. A genuinely members-only/disabled chat answers the same way.
            self.st.lock().unwrap().soft_misses += 1;
            let mut next = base;
            next.extend([("title", title), ("notice", json!(res.notice.clone().unwrap_or_default())), ("error", Value::Null)]);
            self.set_status(next, Some("unavailable"));
            return Ok(self.recheck_delay());
        }

        {
            let mut st = self.st.lock().unwrap();
            st.chat = Some(ChatState { key: res.key.clone(), client_version: res.client_version.clone(), continuation: res.continuation.clone(), video_id: res.video_id.clone() });
            st.meta_at = 0;
        }
        let mut next = base;
        next.extend([("title", title), ("notice", Value::Null), ("error", Value::Null)]);
        self.set_status(next, None);
        self.poll_step(true).await
    }

    async fn tick(self: Arc<Self>) {
        {
            let mut st = self.st.lock().unwrap();
            if st.stopped || st.polling {
                return;
            }
            st.polling = true;
        }
        let have_chat = self.st.lock().unwrap().chat.is_some();
        let r = if have_chat { self.poll_step(false).await } else { self.resolve_step().await };
        let next = match r {
            Ok(n) => n,
            Err(e) => {
                if self.stopped() {
                    self.st.lock().unwrap().polling = false;
                    return;
                }
                let (failures, msg) = {
                    let mut st = self.st.lock().unwrap();
                    st.failures += 1;
                    st.chat = None;
                    (st.failures, if e.is_empty() { "YouTube unreachable".to_string() } else { e })
                };
                self.set_status(vec![("error", json!(msg))], Some("error"));
                self.ctx.log(&format!("[youtube] {} poll failed ({failures}): {msg}", self.channel));
                // Exponential backoff so a deleted channel, or a YouTube outage, cannot turn into a fetch loop.
                (OFFLINE_POLL_MS * 2u64.pow(failures.min(4))).min(MAX_BACKOFF_MS)
            }
        };
        self.st.lock().unwrap().polling = false;
        if !self.stopped() {
            self.schedule(next);
        }
    }

    fn schedule(self: &Arc<Self>, ms: u64) {
        let s = self.clone();
        self.st.lock().unwrap().timer.set(Duration::from_millis(ms.max(1000)), async move {
            let s2 = s.clone();
            s.jobs.spawn(s2.tick());
        });
    }
}

impl Source for YoutubeSource {
    fn start(self: Arc<Self>) {
        let f = {
            let mut st = self.st.lock().unwrap();
            let f = conn_frame(&st);
            st.last_conn_json = f.to_string();
            f
        };
        self.emit(f);
        let s = self.clone();
        self.jobs.spawn(s.tick());
    }

    fn stop(&self) {
        {
            let mut st = self.st.lock().unwrap();
            st.stopped = true;
            st.timer.clear();
        }
        self.jobs.abort_all();
    }

    fn snapshot(&self) -> Vec<Frame> {
        let st = self.st.lock().unwrap();
        vec![chatlog_of(&self.src, &st), conn_frame(&st)]
    }
}
