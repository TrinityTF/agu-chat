//! Port of src/main/twitch/source.js: one Twitch channel's chat: the state ircRelay.js's ChannelRelay kept
//! for a channel (backlog, named mod feed, AutoMod queue, suspicious users, pin, room modes) and the frames
//! its /ws/chat socket sent, emitted straight to the renderer instead of over a socket.
//! IRC comes from the shared hub (irc_hub.rs), EventSub from the shared session (eventsub.rs).
//!
//! Locking: `st` is only ever held for short, non-blocking sections and never across an `emit`, an await or
//! a call into the hub / EventSub, so frame callbacks can call back into the source freely.
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, Shared};
use serde_json::{json, Map, Value};

use super::access::access as get_access;
use super::auth;
use super::backlog::{fetch_recent, shape_recent};
use super::helix::helix_get;
use super::irc_hub::HubListener;
use super::shared::{esub, hub};
use super::vendor::irc::{build_chat_msg, build_notice, IrcLine};
use super::vendor::modfeed_text::{build_held_msg, build_moderate_entry, followers_mode_text, map_automod_status, slow_mode_text, text_from_fragments, toggle_mode_text};
use crate::platforms::{Emit, Source};
use crate::protocol::Frame;
use crate::q;
use crate::util::{iso_ms, js_int_or, js_str_or_empty, js_to_number, now_ms, parse_rfc3339_ms, uuid_v4, Timer};

const MAX_BACKLOG: usize = 200;
const MAX_AUTOMOD_QUEUE: usize = 50;
const MAX_SUSPICIOUS: usize = 300;
const MAX_MOD_FEED: usize = 50;
const LIVE_POLL_MS: u64 = 60_000;
// Twitch has no IRC line or EventSub event for a pin, so the pin banner polls GET /chat/pins while the source
// is open and the user can read it (mods and the broadcaster only).
const PIN_POLL_MS: u64 = 15_000;
const REWARDS_TTL_MS: i64 = 5 * 60_000;
const BACKLOG_REFETCH_MIN_MS: i64 = 30_000;

// NOTICEs that mean this channel cannot be read at all.
const FATAL_NOTICES: [&str; 3] = ["msg_channel_suspended", "msg_channel_blocked", "tos_ban"];

/// login -> live source, so routes can reach the source a request is about (to broadcast a mode change, set a pin).
static SOURCES: LazyLock<Mutex<HashMap<String, Arc<TwitchSource>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn source_for(login: &str) -> Option<Arc<TwitchSource>> {
    SOURCES.lock().unwrap().get(login).cloned()
}

#[derive(Clone, Default, PartialEq)]
struct RoomState {
    followers: Option<String>,
    slow: Option<String>,
    emote_only: Option<String>,
    subs_only: Option<String>,
    r9k: Option<String>,
}

struct St {
    channel_id: Option<String>, // from the first ROOMSTATE (room-id): anonymous IRC needs no Helix to know it
    stopped: bool,
    backlog: Vec<Value>,
    mod_feed: Vec<Value>,
    automod_queue: Vec<Value>,
    suspicious: Map<String, Value>, // insertion ordered
    pinned: Option<Value>,
    esub_active: bool,
    esub_reason: Option<String>,
    named_feed: bool, // channel.moderate is live: the anonymous IRC-derived feed would double every entry
    room_state: Option<RoomState>,
    rewards: HashMap<String, Value>, // rewardId -> {title, cost, bits?} (only learnable on the user's own channel)
    rewards_at: i64,
    // connection
    state: String,
    detail: String,
    live: Option<bool>,
    title: Option<String>,
    // timers / bookkeeping
    live_timer: Timer,
    pin_timer: Timer,
    pin_syncing: Option<Shared<BoxFuture<'static, ()>>>,
    backlog_at: i64,
    can_read_pin: bool,
    auth_user: Option<String>,
    auth_listener: Option<u64>,
}

pub struct TwitchSource {
    me: Weak<TwitchSource>,
    login: String,
    emit_fn: Emit,
    st: Mutex<St>,
}

fn user_json(id: &str, login: &str, name: &str, color: &str) -> Value {
    json!({"id": id, "login": login, "name": name, "color": color})
}

fn or_str(a: String, b: String) -> String {
    if a.is_empty() {
        b
    } else {
        a
    }
}

impl TwitchSource {
    pub fn new(channel: String, emit: Emit) -> Arc<Self> {
        Arc::new_cyclic(|me| TwitchSource {
            me: me.clone(),
            login: channel,
            emit_fn: emit,
            st: Mutex::new(St {
                channel_id: None,
                stopped: false,
                backlog: Vec::new(),
                mod_feed: Vec::new(),
                automod_queue: Vec::new(),
                suspicious: Map::new(),
                pinned: None,
                esub_active: false,
                esub_reason: None,
                named_feed: false,
                room_state: None,
                rewards: HashMap::new(),
                rewards_at: 0,
                state: "connecting".into(),
                detail: "Connecting…".into(),
                live: None,
                title: None,
                live_timer: Timer::new(),
                pin_timer: Timer::new(),
                pin_syncing: None,
                backlog_at: 0,
                can_read_pin: false,
                auth_user: auth::current().map(|r| r.user.id.clone()),
                auth_listener: None,
            }),
        })
    }

    fn arc(&self) -> Arc<TwitchSource> {
        self.me.upgrade().expect("source alive")
    }

    pub fn login(&self) -> &str {
        &self.login
    }

    pub fn channel_id(&self) -> Option<String> {
        self.st.lock().unwrap().channel_id.clone()
    }

    pub fn backlog(&self) -> Vec<Value> {
        self.st.lock().unwrap().backlog.clone()
    }

    pub fn esub_state(&self) -> (bool, Option<String>) {
        let s = self.st.lock().unwrap();
        (s.esub_active, s.esub_reason.clone())
    }

    /// Frames go out through the manager's callback, outside every lock.
    pub fn emit(&self, frame: Frame) {
        (self.emit_fn)(frame);
    }

    // ---------- frames ----------

    fn chatlog(&self) -> Value {
        let s = self.st.lock().unwrap();
        Self::chatlog_locked(&s)
    }

    fn chatlog_locked(s: &St) -> Value {
        json!({
            "type": "chatlog",
            "channelId": s.channel_id, // 7TV/BTTV/FFZ are keyed by it; known from the join ROOMSTATE
            "messages": s.backlog,
            "pinned": s.pinned,
            "modfeed": s.mod_feed,
            "automod": s.automod_queue,
            "suspicious": s.suspicious.values().cloned().collect::<Vec<_>>(),
            "esub": {"active": s.esub_active, "reason": s.esub_reason},
        })
    }

    fn sendability() -> Value {
        match auth::current() {
            None => {
                let err = auth::state().error;
                json!({"canSend": false, "readOnlyReason": err.unwrap_or_else(|| "Log in to Twitch in Settings to send messages.".into())})
            }
            Some(rec) if !rec.scopes.iter().any(|s| s == "user:write:chat") => json!({"canSend": false, "readOnlyReason": "Log in to Twitch again to allow sending messages."}),
            Some(_) => json!({"canSend": true}),
        }
    }

    fn conn_frame(&self) -> Value {
        let s = self.st.lock().unwrap();
        let mut f = json!({"type": "conn", "state": s.state, "detail": s.detail});
        if let (Some(o), Some(extra)) = (f.as_object_mut(), Self::sendability().as_object()) {
            for (k, v) in extra {
                o.insert(k.clone(), v.clone());
            }
        }
        if let Some(id) = &s.channel_id {
            f["channelId"] = json!(id);
        }
        if let Some(l) = s.live {
            f["live"] = json!(l);
        }
        if let Some(t) = s.title.as_ref().filter(|t| !t.is_empty()) {
            f["title"] = json!(t);
        }
        f
    }

    fn emit_conn(&self) {
        self.emit(self.conn_frame());
    }

    fn set_conn(&self, state: &str, detail: &str) {
        {
            let mut s = self.st.lock().unwrap();
            if s.state == state && s.detail == detail {
                return;
            }
            s.state = state.to_string();
            s.detail = detail.to_string();
            if s.stopped {
                return;
            }
        }
        self.emit_conn();
    }

    fn is_stopped(&self) -> bool {
        self.st.lock().unwrap().stopped
    }

    // ---------- IRC ----------

    fn push_backlog(&self, entry: Value) {
        let mut s = self.st.lock().unwrap();
        Self::push_backlog_locked(&mut s, entry);
    }

    fn push_backlog_locked(s: &mut St, entry: Value) {
        s.backlog.push(entry);
        if s.backlog.len() > MAX_BACKLOG {
            let cut = s.backlog.len() - MAX_BACKLOG;
            s.backlog.drain(..cut);
        }
    }

    /// Merge history into the live backlog (live entries win on a duplicate id) and tell every subscriber.
    async fn load_backlog(&self) {
        self.st.lock().unwrap().backlog_at = now_ms();
        let lines = fetch_recent(&self.login, 200, 3000).await;
        if self.is_stopped() || lines.is_empty() {
            return;
        }
        let (entries, room_id) = shape_recent(&lines);
        let frame = {
            let mut s = self.st.lock().unwrap();
            if let (Some(r), None) = (&room_id, &s.channel_id) {
                s.channel_id = Some(r.clone());
            }
            let have: std::collections::HashSet<String> = s.backlog.iter().filter_map(|e| e["id"].as_str().map(String::from)).collect();
            let mut merged: Vec<Value> = entries.into_iter().filter(|e| !e["id"].as_str().map(|i| have.contains(i)).unwrap_or(false)).collect();
            merged.extend(s.backlog.drain(..));
            merged.sort_by_key(|e| e["ts"].as_i64().unwrap_or(0)); // stable: same-millisecond lines keep their order
            let cut = merged.len().saturating_sub(MAX_BACKLOG);
            s.backlog = merged.split_off(cut);
            Self::chatlog_locked(&s)
        };
        self.emit(frame); // a chatlog replaces the view, so this carries everything, live lines included
    }

    // ---------- anonymous mod feed ----------

    fn push_mod_feed(&self, kind: &str, text: &str) {
        self.push_mod_feed_rec(kind, text, None);
    }

    /// `rec` is the structured form of a one-user action (who, by whom, how long, why) for the card's history.
    fn push_mod_feed_rec(&self, kind: &str, text: &str, rec: Option<Value>) {
        let mut feed = json!({"ts": now_ms(), "kind": kind, "text": text});
        if let Some(r) = rec {
            feed["mod"] = r;
        }
        {
            let mut s = self.st.lock().unwrap();
            s.mod_feed.push(feed.clone());
            if s.mod_feed.len() > MAX_MOD_FEED {
                let cut = s.mod_feed.len() - MAX_MOD_FEED;
                s.mod_feed.drain(..cut);
            }
        }
        self.emit(json!({"type": "modfeed", "feed": feed}));
    }

    // ROOMSTATE carries the FULL tag snapshot on join, then only the tags that changed. Diff against the last
    // seen value so only genuine changes make a feed entry; the first one is recorded as a baseline and never
    // diffed.
    fn handle_room_state(&self, tags: &HashMap<String, String>) {
        let (prev, named) = {
            let mut s = self.st.lock().unwrap();
            let prev = s.room_state.clone();
            let pick = |k: &str, p: Option<&Option<String>>| -> Option<String> {
                match tags.get(k) {
                    Some(v) => Some(v.clone()),
                    None => p.and_then(|x| x.clone()),
                }
            };
            let next = RoomState {
                followers: pick("followers-only", prev.as_ref().map(|p| &p.followers)),
                slow: pick("slow", prev.as_ref().map(|p| &p.slow)),
                emote_only: pick("emote-only", prev.as_ref().map(|p| &p.emote_only)),
                subs_only: pick("subs-only", prev.as_ref().map(|p| &p.subs_only)),
                r9k: pick("r9k", prev.as_ref().map(|p| &p.r9k)),
            };
            s.room_state = Some(next.clone());
            let named = s.named_feed;
            (prev.map(|p| (p, next)), named)
        };
        let Some((prev, next)) = prev else { return };
        if named {
            return; // the named feed covers every mode change once channel.moderate is live
        }
        if next.followers.is_some() && next.followers != prev.followers {
            self.push_mod_feed("mode", &followers_mode_text(next.followers.as_deref().unwrap()));
        }
        if next.slow.is_some() && next.slow != prev.slow {
            self.push_mod_feed("mode", &slow_mode_text(next.slow.as_deref().unwrap()));
        }
        if next.emote_only.is_some() && next.emote_only != prev.emote_only {
            self.push_mod_feed("mode", &toggle_mode_text("Emote-only mode", next.emote_only.as_deref() == Some("1")));
        }
        if next.subs_only.is_some() && next.subs_only != prev.subs_only {
            self.push_mod_feed("mode", &toggle_mode_text("Subscribers-only mode", next.subs_only.as_deref() == Some("1")));
        }
        if next.r9k.is_some() && next.r9k != prev.r9k {
            self.push_mod_feed("mode", &toggle_mode_text("Unique chat mode", next.r9k.as_deref() == Some("1")));
        }
    }

    // ---------- EventSub ----------

    /// `setEsub(next, {namedFeed})`
    pub fn set_esub(&self, active: bool, reason: Option<&str>, named_feed: Option<bool>) {
        let frame = {
            let mut s = self.st.lock().unwrap();
            match named_feed {
                Some(nf) => s.named_feed = nf,
                None if !active => s.named_feed = false, // falls back to the anonymous feed the moment EventSub drops
                None => {}
            }
            let reason = reason.filter(|r| !r.is_empty()).map(String::from);
            if s.esub_active == active && s.esub_reason == reason {
                return;
            }
            s.esub_active = active;
            s.esub_reason = reason;
            if s.stopped {
                return;
            }
            json!({"type": "esub", "esub": {"active": s.esub_active, "reason": s.esub_reason}})
        };
        self.emit(frame);
    }

    pub fn handle_moderate(&self, event: &Value) {
        let Some(action) = event.get("action").and_then(Value::as_str).filter(|a| !a.is_empty()) else { return };
        let actor = or_str(or_str(js_str_or_empty(event.get("moderator_user_name")), js_str_or_empty(event.get("moderator_user_login"))), "A moderator".into());
        if let Some(built) = build_moderate_entry(action, &actor, event) {
            let rec = super::vendor::modfeed_text::moderate_record(action, &actor, event);
            self.push_mod_feed_rec(built.kind, &built.text, rec);
        }
    }

    pub fn handle_automod_hold(&self, event: &Value) {
        if !event.get("message_id").map(crate::util::truthy).unwrap_or(false) {
            return;
        }
        let held = build_held_msg(event, "held", None);
        self.upsert_automod(held.clone());
        self.emit(json!({"type": "automod", "held": held})); // a copy: handleAutomodUpdate later edits the queued object in place
    }

    // Resolves an existing hold (or synthesizes one if a mod resolved it before we connected).
    pub fn handle_automod_update(&self, event: &Value) {
        let Some(mid) = event.get("message_id").filter(|v| crate::util::truthy(v)).cloned() else { return };
        let status = map_automod_status(event["status"].as_str().unwrap_or(""));
        let resolved_by = if status == "expired" {
            None
        } else {
            Some(or_str(js_str_or_empty(event.get("moderator_user_name")), js_str_or_empty(event.get("moderator_user_login")))).filter(|s| !s.is_empty())
        };
        let existing = {
            let mut s = self.st.lock().unwrap();
            match s.automod_queue.iter_mut().find(|h| h["id"] == mid) {
                Some(h) => {
                    h["status"] = json!(status);
                    h["resolvedBy"] = json!(resolved_by);
                    true
                }
                None => false,
            }
        };
        if !existing {
            self.upsert_automod(build_held_msg(event, status, resolved_by.as_deref()));
        }
        self.emit(json!({"type": "automodupdate", "id": mid, "status": status, "resolvedBy": resolved_by}));
    }

    fn upsert_automod(&self, held: Value) {
        let mut s = self.st.lock().unwrap();
        match s.automod_queue.iter().position(|h| h["id"] == held["id"]) {
            Some(i) => s.automod_queue[i] = held,
            None => s.automod_queue.push(held),
        }
        if s.automod_queue.len() > MAX_AUTOMOD_QUEUE {
            let cut = s.automod_queue.len() - MAX_AUTOMOD_QUEUE;
            s.automod_queue.drain(..cut);
        }
    }

    // A restricted user's messages never reach IRC, so this builds a synthetic ChatMsg for late joiners
    // WITHOUT a {type:"chat"} frame (it would double-render); the "suspicious" frame carries `msg` and the
    // view appends it itself.
    pub fn handle_suspicious_message(&self, event: &Value) {
        if !event.is_object() || event["low_trust_status"] == "none" {
            return;
        }
        let message = event.get("message").filter(|m| m.is_object()).cloned().unwrap_or_else(|| json!({}));
        let msg_id = js_str_or_empty(message.get("message_id"));
        let user = self.upsert_suspicious(event);
        let mut msg = Value::Null;
        if event["low_trust_status"] == "restricted" {
            msg = json!({
                "platform": "twitch",
                "id": msg_id,
                "ts": now_ms(),
                "user": user_json(&js_str_or_empty(event.get("user_id")), &js_str_or_empty(event.get("user_login")), &or_str(js_str_or_empty(event.get("user_name")), js_str_or_empty(event.get("user_login"))), ""),
                "badges": "",
                "text": js_str_or_empty(message.get("text")),
                "emotes": "",
                "gifs": "",
                "action": false,
                "firstMsg": false,
                "reply": null,
                "rewardId": null,
                "msgId": null,
            });
            self.push_backlog(msg.clone());
        }
        self.emit(json!({"type": "suspicious", "user": user, "msgId": msg_id, "msg": msg}));
    }

    fn upsert_suspicious(&self, event: &Value) -> Value {
        let evaluation = match event.get("ban_evasion_evaluation") {
            Some(e) if crate::util::truthy(e) && e != "unknown" => e.clone(),
            _ => Value::Null,
        };
        let su = json!({
            "id": js_str_or_empty(event.get("user_id")),
            "login": js_str_or_empty(event.get("user_login")),
            "name": or_str(js_str_or_empty(event.get("user_name")), js_str_or_empty(event.get("user_login"))),
            "status": if event["low_trust_status"] == "restricted" { "restricted" } else { "active_monitoring" },
            "types": if event["types"].is_array() { event["types"].clone() } else { json!([]) },
            "evaluation": evaluation,
            "sharedBanChannels": event["shared_ban_channel_ids"].as_array().map(|a| a.len()).unwrap_or(0),
        });
        self.set_suspicious(su["id"].as_str().unwrap_or("").to_string(), su.clone());
        su
    }

    fn set_suspicious(&self, user_id: String, su: Value) {
        let mut s = self.st.lock().unwrap();
        s.suspicious.insert(user_id, su);
        if s.suspicious.len() > MAX_SUSPICIOUS {
            if let Some(k) = s.suspicious.keys().next().cloned() {
                s.suspicious.shift_remove(&k);
            }
        }
    }

    // "none" removes the user; otherwise upsert, keeping previously known types/evaluation (this event carries neither).
    pub fn handle_suspicious_update(&self, event: &Value) {
        let Some(user_id) = event.get("user_id").filter(|v| crate::util::truthy(v)).map(crate::util::js_string) else { return };
        let actor = or_str(or_str(js_str_or_empty(event.get("moderator_user_name")), js_str_or_empty(event.get("moderator_user_login"))), "A moderator".into());
        let target = or_str(or_str(js_str_or_empty(event.get("user_name")), js_str_or_empty(event.get("user_login"))), "the user".into());
        if event["low_trust_status"] == "none" {
            self.st.lock().unwrap().suspicious.shift_remove(&user_id);
            self.emit(json!({"type": "suspicioususer", "user": null, "userId": user_id}));
            self.push_mod_feed("mode", &format!("{actor} removed {target} from suspicious-user monitoring"));
            return;
        }
        let existing = self.st.lock().unwrap().suspicious.get(&user_id).cloned();
        let status = if event["low_trust_status"] == "restricted" { "restricted" } else { "active_monitoring" };
        let ex = |k: &str, d: Value| existing.as_ref().map(|e| e[k].clone()).unwrap_or(d);
        let user = json!({
            "id": user_id,
            "login": or_str(js_str_or_empty(event.get("user_login")), ex("login", json!("")).as_str().unwrap_or("").to_string()),
            "name": or_str(or_str(js_str_or_empty(event.get("user_name")), ex("name", json!("")).as_str().unwrap_or("").to_string()), js_str_or_empty(event.get("user_login"))),
            "status": status,
            "types": ex("types", json!([])),
            "evaluation": ex("evaluation", Value::Null),
            "sharedBanChannels": ex("sharedBanChannels", json!(0)),
        });
        self.set_suspicious(user_id.clone(), user.clone());
        self.emit(json!({"type": "suspicioususer", "user": user, "userId": user_id}));
        self.push_mod_feed("mode", &if status == "restricted" { format!("{actor} restricted {target}'s messages") } else { format!("{actor} set {target} to monitored") });
    }

    // ---------- channel-point redeems (the user's own channel only) ----------

    // A redeem with no text never reaches chat over IRC, so it goes in as a notice; one with text is already
    // there as a message, which gets its reward patched in if it was unknown.
    fn redeem_to_chat(&self, event: &Value, reward: Option<&Value>) {
        let Some(reward) = reward else { return };
        let id = js_str_or_empty(reward.get("id"));
        let title = js_str_or_empty(reward.get("title"));
        if id.is_empty() || title.is_empty() {
            return;
        }
        let bits_n = js_to_number(reward.get("bits"));
        let bits = if bits_n.is_finite() && bits_n > 0.0 && reward.get("bits").map(|b| b.is_number()).unwrap_or(false) { Some(reward["bits"].clone()) } else { None };
        let info = match &bits {
            Some(b) => json!({"title": title, "cost": null, "bits": b}),
            None => json!({"title": title, "cost": if reward.get("cost").map(|c| c.is_number()).unwrap_or(false) { reward["cost"].clone() } else { Value::Null }}),
        };
        let known = {
            let mut s = self.st.lock().unwrap();
            let known = s.rewards.contains_key(&id);
            s.rewards.insert(id.clone(), info.clone());
            known
        };
        if js_str_or_empty(event.get("user_input")).trim().is_empty() {
            let user = user_json(&js_str_or_empty(event.get("user_id")), &js_str_or_empty(event.get("user_login")), &or_str(js_str_or_empty(event.get("user_name")), js_str_or_empty(event.get("user_login"))), "");
            let who = or_str(user["name"].as_str().unwrap_or("").to_string(), user["login"].as_str().unwrap_or("").to_string());
            let notice = json!({
                "id": or_str(js_str_or_empty(event.get("id")), uuid_v4()),
                "ts": now_ms(),
                "system": format!("{who} redeemed {title}"),
                "msgId": if bits.is_some() { "powerup" } else { "redeem" }, // Bits Power-ups file under the page's Bits toggle, not channel points
                "user": user,
                "text": "",
                "emotes": "",
                "reward": info,
            });
            let mut entry = notice.clone();
            entry["notice"] = json!(true);
            self.push_backlog(entry);
            self.emit(json!({"type": "chatnotice", "notice": notice}));
        } else if !known {
            self.patch_reward(&id, &info);
        }
    }

    pub fn handle_redeem(&self, event: &Value) {
        self.redeem_to_chat(event, event.get("reward"));
    }

    pub fn handle_power_up(&self, event: &Value) {
        self.redeem_to_chat(event, event.get("custom_power_up"));
    }

    // A redeem's PRIVMSG can land before the reward is known; when it is learned, patch the messages already sent.
    fn patch_reward(&self, reward_id: &str, reward: &Value) {
        let touched = {
            let mut s = self.st.lock().unwrap();
            let mut touched = false;
            for e in s.backlog.iter_mut() {
                if e.get("notice").is_none() && e["rewardId"].as_str() == Some(reward_id) && e.get("reward").is_none() {
                    e["reward"] = reward.clone();
                    touched = true;
                }
            }
            touched
        };
        if touched {
            self.emit(json!({"type": "rewardinfo", "rewardId": reward_id, "reward": reward}));
        }
    }

    /// Titles and costs of the channel's rewards: Helix lists them only for the broadcaster's own token.
    pub fn refresh_rewards(&self) {
        let Some(rec) = auth::current() else { return };
        let channel_id = {
            let mut s = self.st.lock().unwrap();
            let Some(cid) = s.channel_id.clone() else { return };
            if cid != rec.user.id || now_ms() - s.rewards_at < REWARDS_TTL_MS {
                return;
            }
            s.rewards_at = now_ms();
            cid
        };
        let me = self.arc();
        tokio::spawn(async move {
            let (rewards, power_ups) = tokio::join!(
                helix_get("/channel_points/custom_rewards", q! {"broadcaster_id" => channel_id.clone()}),
                helix_get("/bits/custom_power_ups", q! {"broadcaster_id" => channel_id.clone()}), // needs bits:read
            );
            let mut all: Vec<(String, Value)> = Vec::new();
            {
                let mut s = me.st.lock().unwrap();
                if let Ok(r) = rewards {
                    for r in r["data"].as_array().cloned().unwrap_or_default() {
                        let info = json!({"title": r["title"], "cost": if r["cost"].is_number() { r["cost"].clone() } else { Value::Null }});
                        s.rewards.insert(js_str_or_empty(r.get("id")), info);
                    }
                }
                if let Ok(p) = power_ups {
                    for p in p["data"].as_array().cloned().unwrap_or_default() {
                        s.rewards.insert(js_str_or_empty(p.get("id")), json!({"title": p["title"], "cost": null, "bits": p["bits"]}));
                    }
                }
                for (id, info) in s.rewards.iter() {
                    all.push((id.clone(), info.clone()));
                }
            }
            for (id, info) in all {
                me.patch_reward(&id, &info);
            }
        });
    }

    // ---------- account changes, live status, pin ----------

    fn on_auth_change(&self) {
        if self.is_stopped() {
            return;
        }
        let user = auth::current().map(|r| r.user.id.clone());
        self.emit_conn(); // canSend / readOnlyReason follow the login
        let (has_channel, same) = {
            let mut s = self.st.lock().unwrap();
            if s.channel_id.is_none() {
                return;
            }
            let same = user == s.auth_user;
            if !same {
                s.auth_user = user;
                s.live = None;
                s.title = None;
            }
            (true, same)
        };
        let _ = has_channel;
        let me = self.arc();
        tokio::spawn(async move { me.refresh_moderation().await }); // scopes may have changed (a re-login for new permissions)
        if same {
            return; // a pending login or a scope refresh: the live poll does not depend on it
        }
        self.start_live_poll();
    }

    fn start_live_poll(&self) {
        let me = self.me.clone();
        let mut s = self.st.lock().unwrap();
        s.live_timer.clear();
        if auth::current().is_none() || s.channel_id.is_none() {
            return;
        }
        if let Some(m) = me.upgrade() {
            tokio::spawn(async move { m.poll_live().await });
        }
        s.live_timer.every(Duration::from_millis(LIVE_POLL_MS), move || {
            let me = me.clone();
            async move {
                if let Some(m) = me.upgrade() {
                    m.poll_live().await;
                }
            }
        });
    }

    async fn poll_live(&self) {
        let Some(cid) = ({
            let s = self.st.lock().unwrap();
            if s.stopped {
                None
            } else {
                s.channel_id.clone()
            }
        }) else {
            return;
        };
        // On failure the next poll tries again; the header just keeps what it had.
        let Ok(page) = helix_get("/streams", q! {"user_id" => cid}).await else { return };
        let stream = page["data"].get(0).filter(|s| !s.is_null());
        let live = stream.is_some();
        let title = stream.and_then(|s| s["title"].as_str()).filter(|t| !t.is_empty()).map(String::from);
        let changed = {
            let mut s = self.st.lock().unwrap();
            if s.live != Some(live) || s.title != title {
                s.live = Some(live);
                s.title = title;
                !s.stopped
            } else {
                false
            }
        };
        if changed {
            self.emit_conn();
        }
    }

    async fn refresh_moderation(&self) {
        let cid = self.channel_id().unwrap_or_else(|| "null".into());
        let acc = get_access(&cid).await;
        if self.is_stopped() {
            return;
        }
        let rec = auth::current();
        let can = rec
            .as_ref()
            .map(|rec| (acc.is_broadcaster || acc.is_mod == Some(true)) && (rec.scopes.iter().any(|s| s == "moderator:manage:chat_messages") || rec.scopes.iter().any(|s| s == "moderator:read:chat_messages")))
            .unwrap_or(false);
        let had_pin = {
            let mut s = self.st.lock().unwrap();
            s.can_read_pin = can;
            s.pin_timer.clear();
            if can {
                let me = self.me.clone();
                s.pin_timer.every(Duration::from_millis(PIN_POLL_MS), move || {
                    let me = me.clone();
                    async move {
                        if let Some(m) = me.upgrade() {
                            m.sync_pin().await;
                        }
                    }
                });
            }
            s.pinned.is_some()
        };
        if can {
            let me = self.arc();
            tokio::spawn(async move { me.sync_pin().await });
            if acc.is_broadcaster {
                self.refresh_rewards();
            }
        } else if had_pin {
            self.set_pinned(None);
        }
    }

    /// Reads the current pin and updates the banner if it changed. A failed read leaves the banner alone; only
    /// Twitch saying nothing is pinned clears it. Concurrent callers share one read.
    pub fn sync_pin(&self) -> BoxFuture<'static, ()> {
        let mut s = self.st.lock().unwrap();
        if s.channel_id.is_none() || !s.can_read_pin {
            return futures_util::future::ready(()).boxed();
        }
        if let Some(f) = &s.pin_syncing {
            return f.clone().boxed();
        }
        let me = self.arc();
        let handle = tokio::spawn(async move {
            let cid = me.channel_id();
            if let (Some(rec), Some(cid)) = (auth::current(), cid) {
                // The next poll tries again on failure.
                if let Ok(page) = helix_get("/chat/pins", q! {"broadcaster_id" => cid, "moderator_id" => rec.user.id.clone()}).await {
                    let raw = page["data"].get(0).filter(|r| !r.is_null()).cloned();
                    let next = raw.and_then(|r| me.pinned_from_helix(&r));
                    me.set_pinned(next);
                }
            }
            me.st.lock().unwrap().pin_syncing = None;
        });
        let fut = async move {
            let _ = handle.await;
        }
        .boxed()
        .shared();
        s.pin_syncing = Some(fut.clone());
        fut.boxed()
    }

    fn sync_pin_soon(&self) {
        let me = self.arc();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(1500)).await;
            me.sync_pin().await;
        });
    }

    // Twitch's pin -> what the banner shows. The message as it came through IRC is used when it is still in the
    // backlog (it has badges, colour and emote positions); otherwise it is rebuilt from Twitch's fragments with
    // the badges and colour of the sender's latest line here.
    fn pinned_from_helix(&self, raw: &Value) -> Option<Value> {
        let id = js_str_or_empty(raw.get("message_id"));
        if id.is_empty() {
            return None;
        }
        let nullable = |k: &str| match raw.get(k) {
            Some(v) if crate::util::truthy(v) => v.clone(),
            _ => Value::Null,
        };
        let pin = json!({
            "by": {
                "id": js_str_or_empty(raw.get("pinned_by_user_id")),
                "login": js_str_or_empty(raw.get("pinned_by_user_login")),
                "name": or_str(js_str_or_empty(raw.get("pinned_by_user_name")), js_str_or_empty(raw.get("pinned_by_user_login"))),
            },
            "startsAt": nullable("starts_at"),
            "endsAt": nullable("ends_at"),
        });
        if let Some(mut seen) = self.find_msg(&id) {
            seen["pin"] = pin;
            return Some(seen);
        }
        let sender_id = js_str_or_empty(raw.get("sender_user_id"));
        let last = if sender_id.is_empty() {
            None
        } else {
            self.st.lock().unwrap().backlog.iter().rev().find(|e| e.get("notice").is_none() && e["user"]["id"].as_str() == Some(sender_id.as_str())).cloned()
        };
        let login = js_str_or_empty(raw.get("sender_user_login"));
        let (text, emotes) = text_from_fragments(raw.get("message").unwrap_or(&Value::Null));
        let ts = raw.get("starts_at").and_then(Value::as_str).and_then(parse_rfc3339_ms).filter(|t| *t != 0).unwrap_or_else(now_ms);
        Some(json!({
            "platform": "twitch",
            "id": id,
            "ts": ts,
            "user": {
                "id": sender_id,
                "login": login,
                "name": or_str(js_str_or_empty(raw.get("sender_user_name")), login.clone()),
                "color": last.as_ref().map(|l| l["user"]["color"].clone()).unwrap_or_else(|| json!("")),
            },
            "badges": last.as_ref().map(|l| l["badges"].clone()).unwrap_or_else(|| json!("")),
            "text": text,
            "emotes": emotes,
            "gifs": "",
            "action": false,
            "firstMsg": false,
            "reply": null,
            "rewardId": null,
            "msgId": null,
            "pin": pin,
        }))
    }

    fn find_msg(&self, id: &str) -> Option<Value> {
        self.st.lock().unwrap().backlog.iter().find(|e| e["id"].as_str() == Some(id) && e.get("notice").is_none()).cloned()
    }

    fn set_pinned(&self, next: Option<Value>) {
        let key = |p: &Option<Value>| match p {
            Some(p) => format!(
                "{}|{}|{}|{}",
                js_str_or_empty(p.get("id")),
                js_str_or_empty(p["pin"].get("endsAt")),
                js_str_or_empty(p["pin"]["by"].get("id")),
                js_str_or_empty(p.get("text"))
            ),
            None => String::new(),
        };
        {
            let mut s = self.st.lock().unwrap();
            if key(&next) == key(&s.pinned) {
                return;
            }
            s.pinned = next.clone();
        }
        self.emit(json!({"type": "pin", "msg": next}));
    }

    /// A pin made from this app: shown at once from the backlog, then confirmed (who pinned it, when it ends)
    /// by the sync_pin() that follows.
    pub fn pin_local(&self, id: &str, by: Value, duration_seconds: &Value) {
        let Some(mut msg) = self.find_msg(id) else { return };
        let now = now_ms();
        let secs = js_to_number(Some(duration_seconds));
        msg["pin"] = json!({
            "by": by,
            "startsAt": iso_ms(now),
            "endsAt": if secs.is_finite() && secs > 0.0 { json!(iso_ms(now + (secs * 1000.0) as i64)) } else { Value::Null },
        });
        self.set_pinned(Some(msg));
    }

    pub fn unpin(&self) {
        self.set_pinned(None);
    }
}

impl Source for TwitchSource {
    fn start(self: Arc<Self>) {
        SOURCES.lock().unwrap().insert(self.login.clone(), self.clone());
        self.emit_conn();
        let me = self.me.clone();
        let id = auth::EVENTS.on(Arc::new(move |_| {
            if let Some(m) = me.upgrade() {
                m.on_auth_change();
            }
        }));
        self.st.lock().unwrap().auth_listener = Some(id);
        hub().add(&self.login, self.clone());
        let me = self.clone();
        tokio::spawn(async move { me.load_backlog().await });
    }

    fn stop(&self) {
        let me = self.arc();
        {
            let mut sources = SOURCES.lock().unwrap();
            if sources.get(&self.login).map(|s| Arc::ptr_eq(s, &me)).unwrap_or(false) {
                sources.remove(&self.login);
            }
        }
        let listener = {
            let mut s = self.st.lock().unwrap();
            s.stopped = true;
            s.live_timer.clear();
            s.pin_timer.clear();
            s.auth_listener.take()
        };
        if let Some(id) = listener {
            auth::EVENTS.off(id);
        }
        hub().remove(&self.login);
        esub().detach(&me);
    }

    fn snapshot(&self) -> Vec<Frame> {
        vec![self.conn_frame(), self.chatlog()]
    }
}

impl HubListener for TwitchSource {
    fn on_joined(&self, room_id: Option<String>, reconnect: bool) {
        if self.is_stopped() {
            return;
        }
        let (had_id, now_id, refetch) = {
            let mut s = self.st.lock().unwrap();
            let had = s.channel_id.clone();
            if let Some(r) = room_id.filter(|r| !r.is_empty()) {
                s.channel_id = Some(r);
            }
            s.room_state = None; // the join ROOMSTATE is a fresh baseline, never a diff
            (had, s.channel_id.clone(), reconnect && now_ms() - s.backlog_at > BACKLOG_REFETCH_MIN_MS)
        };
        self.set_conn("open", "");
        if had_id.is_none() && now_id.is_some() {
            self.emit_conn(); // set_conn skips a same-state emit; the renderer still needs the id
        }
        let me = self.arc();
        if refetch {
            let m = me.clone();
            tokio::spawn(async move { m.load_backlog().await }); // fill the gap the outage left
        }
        if now_id.is_some() {
            let m = me.clone();
            tokio::spawn(async move { esub().attach(m).await });
            let m = me.clone();
            tokio::spawn(async move { m.refresh_moderation().await });
            me.start_live_poll();
        }
    }

    fn on_disconnected(&self, reason: &str) {
        if self.is_stopped() {
            return;
        }
        self.st.lock().unwrap().room_state = None;
        self.set_conn("connecting", &format!("Reconnecting… ({reason})"));
    }

    fn on_channel_error(&self, detail: &str) {
        self.set_conn("error", detail);
    }

    fn on_notice(&self, msg_id: &str, text: &str) {
        if FATAL_NOTICES.contains(&msg_id) {
            self.set_conn("error", if text.is_empty() { "This channel cannot be read." } else { text });
        }
    }

    fn on_line(&self, m: &IrcLine) {
        match m.command.as_str() {
            "PRIVMSG" => {
                let display = or_str(m.tag("display-name").to_string(), m.user.clone());
                let mut msg = build_chat_msg(m, &display);
                if let Some(rid) = msg["rewardId"].as_str().map(String::from) {
                    let info = self.st.lock().unwrap().rewards.get(&rid).cloned();
                    match info {
                        Some(i) => msg["reward"] = i,
                        None => self.refresh_rewards(),
                    }
                }
                self.push_backlog(msg.clone());
                self.emit(json!({"type": "chat", "msg": msg}));
            }
            "USERNOTICE" => {
                if let Some(mut notice) = build_notice(m) {
                    notice["ts"] = json!(js_int_or(m.tag("tmi-sent-ts"), now_ms()));
                    let mut entry = notice.clone();
                    entry["notice"] = json!(true);
                    self.push_backlog(entry);
                    self.emit(json!({"type": "chatnotice", "notice": notice}));
                }
            }
            "CLEARCHAT" => {
                let user_id = Some(m.tag("target-user-id")).filter(|s| !s.is_empty()).map(String::from);
                let login = Some(m.text.clone()).filter(|s| !s.is_empty());
                let duration = Some(js_int_or(m.tag("ban-duration"), 0)).filter(|d| *d != 0);
                self.emit(json!({"type": "clearchat", "userId": user_id, "login": login, "duration": duration}));
                let (pinned_hit, named) = {
                    let mut s = self.st.lock().unwrap();
                    for e in s.backlog.iter_mut() {
                        if e.get("notice").is_none() && user_id.as_ref().map(|u| e["user"]["id"].as_str() == Some(u.as_str())).unwrap_or(true) {
                            e["deleted"] = json!(true);
                        }
                    }
                    let hit = s.pinned.as_ref().map(|p| user_id.is_none() || p["user"]["id"].as_str() == user_id.as_deref()).unwrap_or(false);
                    (hit, s.named_feed)
                };
                // Twitch decides whether clearing the pinned message's author unpins it, so ask rather than guess.
                if pinned_hit {
                    self.sync_pin_soon();
                }
                if !named {
                    match (&user_id, duration) {
                        (None, _) => self.push_mod_feed("clear", "chat was cleared"),
                        (Some(uid), Some(d)) => self.push_mod_feed("timeout", &format!("{} was timed out for {}s", login.as_deref().unwrap_or(uid), d)),
                        (Some(uid), None) => self.push_mod_feed("ban", &format!("{} was banned", login.as_deref().unwrap_or(uid))),
                    }
                }
            }
            "CLEARMSG" => {
                let id = m.tag("target-msg-id");
                if id.is_empty() {
                    return;
                }
                self.emit(json!({"type": "clearmsg", "id": id}));
                let (pin_hit, named) = {
                    let mut s = self.st.lock().unwrap();
                    if let Some(e) = s.backlog.iter_mut().find(|e| e["id"].as_str() == Some(id)) {
                        e["deleted"] = json!(true);
                    }
                    (s.pinned.as_ref().map(|p| p["id"].as_str() == Some(id)).unwrap_or(false), s.named_feed)
                };
                if pin_hit {
                    self.sync_pin_soon();
                }
                if !named {
                    let who = Some(m.tag("login")).filter(|l| !l.is_empty()).unwrap_or("someone");
                    self.push_mod_feed("delete", &format!("a message from {who} was deleted"));
                }
            }
            "ROOMSTATE" => {
                {
                    let mut s = self.st.lock().unwrap();
                    if !m.tag("room-id").is_empty() && s.channel_id.is_none() {
                        s.channel_id = Some(m.tag("room-id").to_string());
                    }
                }
                self.handle_room_state(&m.tags);
            }
            _ => {}
        }
    }
}
