//! 7TV EventAPI (`wss://events.7tv.io/v3`): live changes to the channels' 7TV emote sets.
//!
//! ONE socket for the whole app, shared by every pane: a pane subscribes `emote_set.update` for each open
//! channel's active set and `user.update` for the channel's 7TV user (catches the channel switching its active
//! set), and unsubscribes when it closes; the socket is closed when nothing is subscribed. The protocol: the
//! server says Hello (op 1) with a heartbeat interval and sends Heartbeat (op 2) at that pace; a silent socket is
//! dropped and reconnected. Reconnects back off (1 s doubling to 60 s, reset by a Hello), resubscribe everything,
//! and tell the listener (`SevenEvent::Resynced`) so it can reload what changed while the socket was down.
//! Reconnect (op 4) and End of Stream (op 7) from the server reconnect too.
//!
//! Parsing and applying the dispatches are plain functions (tested below). `SevenSocket` is the connection: it
//! runs on the net runtime and calls its listener from there; the desktop hops that onto the UI thread, Android
//! feeds its session.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use agu_chat_core::ws::{self, WsEvent, WsHandle};
use serde_json::{json, Value};

use super::emotes::{seven_tv_emote, EmoteMap, ProviderEmote};
use crate::net::Net;

pub const EVENTS_URL: &str = "wss://events.7tv.io/v3";
pub const EMOTE_SET_UPDATE: &str = "emote_set.update";
pub const USER_UPDATE: &str = "user.update";

// ---- protocol ------------------------------------------------------------------------------------

/// One change to an emote set's emotes.
#[derive(Clone, Debug, PartialEq)]
pub enum EmoteChange {
    Added(Arc<ProviderEmote>),
    Removed(Arc<ProviderEmote>),
    /// renamed, or its flags changed (zero-width); `old` and `new` share the emote id
    Updated { old: Arc<ProviderEmote>, new: Arc<ProviderEmote> },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dispatch {
    /// `emote_set.update`: who changed the set, and how.
    EmoteSet { set_id: String, actor: String, changes: Vec<EmoteChange> },
    /// `user.update` that switched one of the user's connections to another emote set.
    UserSet { user_id: String, actor: String, set_id: String, set_name: String },
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Hello { heartbeat_ms: u64, session_id: String },
    Heartbeat,
    Dispatch(Dispatch),
    Reconnect,
    Ack,
    Error(String),
    EndOfStream(u64, String),
    Unknown,
}

fn st(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

/// The name a change is credited to: the 7TV display name, else the username.
fn actor_name(body: &Value) -> String {
    let a = body.get("actor").unwrap_or(&Value::Null);
    let d = st(a, "display_name");
    if d.is_empty() {
        st(a, "username")
    } else {
        d
    }
}

/// An ActiveEmote of a change (`value` / `old_value`) as a provider emote; None for anything else.
fn active_emote(v: Option<&Value>) -> Option<Arc<ProviderEmote>> {
    let v = v.filter(|v| v.is_object())?;
    let e = seven_tv_emote(v);
    (!e.id.is_empty() && !e.name.is_empty()).then_some(e)
}

fn changes_of<'a>(body: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    body.get(key).and_then(|a| a.as_array()).into_iter().flatten().filter(|c| c.get("key").and_then(|k| k.as_str()) == Some("emotes"))
}

fn parse_dispatch(d: &Value) -> Dispatch {
    let body = d.get("body").unwrap_or(&Value::Null);
    match d.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        EMOTE_SET_UPDATE => {
            let mut changes = Vec::new();
            for c in changes_of(body, "pushed") {
                if let Some(e) = active_emote(c.get("value")) {
                    changes.push(EmoteChange::Added(e));
                }
            }
            for c in changes_of(body, "updated") {
                match (active_emote(c.get("old_value")), active_emote(c.get("value"))) {
                    (Some(old), Some(new)) => changes.push(EmoteChange::Updated { old, new }),
                    (None, Some(new)) => changes.push(EmoteChange::Added(new)),
                    (Some(old), None) => changes.push(EmoteChange::Removed(old)),
                    (None, None) => {}
                }
            }
            for c in changes_of(body, "pulled") {
                if let Some(e) = active_emote(c.get("old_value")) {
                    changes.push(EmoteChange::Removed(e));
                }
            }
            Dispatch::EmoteSet { set_id: st(body, "id"), actor: actor_name(body), changes }
        }
        USER_UPDATE => {
            // updated: [{key: "connections", index, value: [{key: "emote_set", old_value, value: {id, name}},
            // {key: "emote_set_id", old_value, value: "<id>"}]}]
            let mut set: Option<(String, String)> = None;
            for c in body.get("updated").and_then(|a| a.as_array()).into_iter().flatten() {
                if c.get("key").and_then(|k| k.as_str()) != Some("connections") {
                    continue;
                }
                for f in c.get("value").and_then(|a| a.as_array()).into_iter().flatten() {
                    match f.get("key").and_then(|k| k.as_str()) {
                        Some("emote_set") => {
                            let v = f.get("value").unwrap_or(&Value::Null);
                            let id = st(v, "id");
                            if !id.is_empty() {
                                set = Some((id, st(v, "name")));
                            }
                        }
                        Some("emote_set_id") if set.is_none() => {
                            if let Some(id) = f.get("value").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                                set = Some((id.to_string(), String::new()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            match set {
                Some((set_id, set_name)) => Dispatch::UserSet { user_id: st(body, "id"), actor: actor_name(body), set_id, set_name },
                None => Dispatch::Other,
            }
        }
        _ => Dispatch::Other,
    }
}

/// One server message.
pub fn parse(text: &str) -> Incoming {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Incoming::Unknown };
    let d = v.get("d").unwrap_or(&Value::Null);
    match v.get("op").and_then(|o| o.as_u64()) {
        Some(0) => Incoming::Dispatch(parse_dispatch(d)),
        Some(1) => Incoming::Hello { heartbeat_ms: d.get("heartbeat_interval").and_then(|h| h.as_u64()).unwrap_or(45_000), session_id: st(d, "session_id") },
        Some(2) => Incoming::Heartbeat,
        Some(4) => Incoming::Reconnect,
        Some(5) => Incoming::Ack,
        Some(6) => Incoming::Error(st(d, "message")),
        Some(7) => Incoming::EndOfStream(d.get("code").and_then(|c| c.as_u64()).unwrap_or(0), st(d, "message")),
        _ => Incoming::Unknown,
    }
}

pub fn subscribe_msg(kind: &str, object_id: &str) -> String {
    json!({"op": 35, "d": {"type": kind, "condition": {"object_id": object_id}}}).to_string()
}

pub fn unsubscribe_msg(kind: &str, object_id: &str) -> String {
    json!({"op": 36, "d": {"type": kind, "condition": {"object_id": object_id}}}).to_string()
}

/// Apply set changes to a channel's 7TV emote map (keyed by emote name, like the merge).
pub fn apply_changes(map: &mut EmoteMap, changes: &[EmoteChange]) {
    let remove = |map: &mut EmoteMap, e: &ProviderEmote| {
        if map.get(&e.name).map(|x| x.id == e.id).unwrap_or(false) {
            map.remove(&e.name);
        } else if let Some(k) = map.iter().find(|(_, x)| x.id == e.id).map(|(k, _)| k.clone()) {
            // the map knew it by another name (a rename we missed)
            map.remove(&k);
        }
    };
    for c in changes {
        match c {
            EmoteChange::Added(e) => {
                map.insert(e.name.clone(), e.clone());
            }
            EmoteChange::Removed(e) => remove(map, e),
            EmoteChange::Updated { old, new } => {
                remove(map, old);
                map.insert(new.name.clone(), new.clone());
            }
        }
    }
}

/// Object ids are compared without case: 7TV answers with the ULID form of the id it was given.
pub fn same_id(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

// ---- the shared connection ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum SevenEvent {
    Dispatch(Dispatch),
    /// The socket came back after a drop: changes made meanwhile were missed.
    Resynced,
}

/// Heard from the socket's thread.
pub type Listener = Arc<dyn Fn(SevenEvent) + Send + Sync>;

const BACKOFF_MIN_MS: u64 = 1_000;
const BACKOFF_MAX_MS: u64 = 60_000;
/// No Hello this long after connecting: start over.
const HELLO_TIMEOUT: Duration = Duration::from_secs(30);

/// The reconnect delay after `failures` failed attempts in a row: 1 s doubling to 60 s.
pub fn backoff_ms(failures: u32) -> u64 {
    (BACKOFF_MIN_MS << failures.min(6)).min(BACKOFF_MAX_MS)
}

/// How long the socket may stay silent: two heartbeats and some slack.
pub fn silence_limit(heartbeat_ms: u64) -> Duration {
    Duration::from_millis(heartbeat_ms.max(5_000) * 2 + 5_000)
}

struct State {
    rt: tokio::runtime::Handle,
    user_agent: String,
    me: Weak<Mutex<State>>,
    listener: Listener,
    /// (type, object id) -> how many subscribers
    subs: HashMap<(String, String), usize>,
    ws: Option<WsHandle>,
    /// bumped per socket: events of an older socket are ignored
    gen: u64,
    /// a Hello arrived on the current socket
    ready: bool,
    /// a socket was ready before (the next Hello is a reconnect)
    was_ready: bool,
    opened_at: Instant,
    last_msg: Instant,
    heartbeat_ms: u64,
    failures: u32,
    retry_at: Option<Instant>,
}

#[derive(Clone)]
pub struct SevenSocket(Arc<Mutex<State>>);

impl SevenSocket {
    pub fn new(net: &Arc<Net>, listener: Listener) -> SevenSocket {
        let now = Instant::now();
        let inner = Arc::new_cyclic(|me| {
            Mutex::new(State {
                rt: net.rt.handle().clone(),
                user_agent: net.user_agent.clone(),
                me: me.clone(),
                listener,
                subs: HashMap::new(),
                ws: None,
                gen: 0,
                ready: false,
                was_ready: false,
                opened_at: now,
                last_msg: now,
                heartbeat_ms: 45_000,
                failures: 0,
                retry_at: None,
            })
        });
        // the watchdog: silent sockets, missing Hellos and due reconnects (ends with the last handle)
        let weak = Arc::downgrade(&inner);
        net.rt.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                let Some(a) = weak.upgrade() else { break };
                SevenSocket(a).tick();
            }
        });
        SevenSocket(inner)
    }

    pub fn subscribe(&self, kind: &str, object_id: &str) {
        if object_id.is_empty() {
            return;
        }
        let mut s = self.0.lock().unwrap();
        let n = s.subs.entry((kind.to_string(), object_id.to_string())).or_insert(0);
        *n += 1;
        if *n > 1 {
            return;
        }
        if s.ready {
            if let Some(w) = &s.ws {
                w.send(&subscribe_msg(kind, object_id));
            }
        } else if s.ws.is_none() && s.retry_at.is_none() {
            Self::connect(&mut s);
        }
    }

    pub fn unsubscribe(&self, kind: &str, object_id: &str) {
        let mut s = self.0.lock().unwrap();
        let key = (kind.to_string(), object_id.to_string());
        let Some(n) = s.subs.get_mut(&key) else { return };
        *n -= 1;
        if *n > 0 {
            return;
        }
        s.subs.remove(&key);
        if s.ready {
            if let Some(w) = &s.ws {
                w.send(&unsubscribe_msg(kind, object_id));
            }
        }
        if s.subs.is_empty() {
            // nothing left to hear about: no idle socket
            Self::drop_socket(&mut s);
            s.retry_at = None;
            s.failures = 0;
            s.was_ready = false;
        }
    }

    fn connect(s: &mut State) {
        s.gen += 1;
        s.ready = false;
        s.retry_at = None;
        s.opened_at = Instant::now();
        s.last_msg = s.opened_at;
        let (gen, me, ua) = (s.gen, s.me.clone(), s.user_agent.clone());
        let _rt = s.rt.enter();
        s.ws = Some(ws::connect(EVENTS_URL.to_string(), ua, move |ev| {
            if let Some(a) = me.upgrade() {
                SevenSocket(a).on_ws(gen, ev);
            }
        }));
    }

    fn drop_socket(s: &mut State) {
        s.gen += 1;
        s.ready = false;
        if let Some(w) = s.ws.take() {
            w.close();
        }
    }

    /// The socket is gone or must go: reconnect after a delay, if anything is still subscribed.
    fn lost(s: &mut State, delay_ms: Option<u64>) {
        Self::drop_socket(s);
        if s.subs.is_empty() {
            return;
        }
        let ms = delay_ms.unwrap_or_else(|| {
            let ms = backoff_ms(s.failures);
            s.failures = s.failures.saturating_add(1);
            ms
        });
        s.retry_at = Some(Instant::now() + Duration::from_millis(ms));
    }

    fn tick(&self) {
        let mut s = self.0.lock().unwrap();
        let now = Instant::now();
        if let Some(at) = s.retry_at {
            if now >= at && !s.subs.is_empty() {
                Self::connect(&mut s);
            }
            return;
        }
        if s.ws.is_none() {
            return;
        }
        let stale = if s.ready { now.duration_since(s.last_msg) > silence_limit(s.heartbeat_ms) } else { now.duration_since(s.opened_at) > HELLO_TIMEOUT };
        if stale {
            Self::lost(&mut s, None);
        }
    }

    fn on_ws(&self, gen: u64, ev: WsEvent) {
        let mut out: Vec<SevenEvent> = Vec::new();
        let listener = {
            let mut s = self.0.lock().unwrap();
            if gen != s.gen {
                return;
            }
            s.last_msg = Instant::now();
            match ev {
                WsEvent::Open => {}
                WsEvent::Close(..) | WsEvent::Error(_) => Self::lost(&mut s, None),
                WsEvent::Message(text) => match parse(&text) {
                    Incoming::Hello { heartbeat_ms, .. } => {
                        s.heartbeat_ms = heartbeat_ms;
                        s.ready = true;
                        s.failures = 0;
                        if let Some(w) = &s.ws {
                            for (kind, id) in s.subs.keys() {
                                w.send(&subscribe_msg(kind, id));
                            }
                        }
                        if std::mem::replace(&mut s.was_ready, true) {
                            out.push(SevenEvent::Resynced);
                        }
                    }
                    Incoming::Dispatch(Dispatch::Other) => {}
                    Incoming::Dispatch(d) => out.push(SevenEvent::Dispatch(d)),
                    // the server asks for a new connection: no need to wait long
                    Incoming::Reconnect => Self::lost(&mut s, Some(500)),
                    Incoming::EndOfStream(..) => Self::lost(&mut s, None),
                    Incoming::Heartbeat | Incoming::Ack | Incoming::Error(_) | Incoming::Unknown => {}
                },
            }
            s.listener.clone()
        };
        // the listener runs without the state locked: it may subscribe / unsubscribe
        for ev in out {
            listener(ev);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(id: &str, name: &str, flags: i64) -> Value {
        json!({"id": id, "name": name, "flags": flags, "timestamp": 1, "actor_id": "u1",
               "data": {"id": id, "name": "Original", "flags": 0, "animated": true, "host": {"url": format!("//cdn.7tv.app/emote/{id}")}}})
    }

    fn dispatch(kind: &str, body: Value) -> String {
        json!({"op": 0, "t": 1, "d": {"type": kind, "body": body}}).to_string()
    }

    #[test]
    fn parses_control_messages() {
        assert_eq!(parse(r#"{"op":1,"d":{"heartbeat_interval":47327,"session_id":"S","subscription_limit":500},"t":1}"#), Incoming::Hello { heartbeat_ms: 47327, session_id: "S".into() });
        assert_eq!(parse(r#"{"d":{"count":0},"op":2,"t":1,"s":1}"#), Incoming::Heartbeat);
        assert_eq!(parse(r#"{"op":4,"d":{}}"#), Incoming::Reconnect);
        assert_eq!(parse(r#"{"d":{"command":"SUBSCRIBE","data":{"id":2}},"op":5}"#), Incoming::Ack);
        assert_eq!(parse(r#"{"d":{"code":4002,"message":"Invalid Payload"},"op":7}"#), Incoming::EndOfStream(4002, "Invalid Payload".into()));
        assert_eq!(parse("not json"), Incoming::Unknown);
    }

    #[test]
    fn subscribe_payloads() {
        let v: Value = serde_json::from_str(&subscribe_msg(EMOTE_SET_UPDATE, "SET")).unwrap();
        assert_eq!(v, json!({"op": 35, "d": {"type": "emote_set.update", "condition": {"object_id": "SET"}}}));
        let v: Value = serde_json::from_str(&unsubscribe_msg(USER_UPDATE, "U")).unwrap();
        assert_eq!(v["op"], 36);
        assert_eq!(v["d"]["type"], "user.update");
    }

    #[test]
    fn parses_emote_set_update() {
        let msg = dispatch(
            "emote_set.update",
            json!({
                "id": "SET1", "kind": 3,
                "actor": {"id": "u1", "username": "a_g_u__", "display_name": "A_g_u__"},
                "pushed": [{"key": "emotes", "index": 10, "type": "object", "value": active("E1", "PotatWind", 0)}],
                "pulled": [{"key": "emotes", "index": 3, "type": "object", "old_value": active("E2", "OldOne", 0), "value": null}],
                "updated": [{"key": "emotes", "index": 4, "type": "object", "old_value": active("E3", "Before", 0), "value": active("E3", "After", 1)}]
            }),
        );
        let Incoming::Dispatch(Dispatch::EmoteSet { set_id, actor, changes }) = parse(&msg) else { panic!("not a set dispatch") };
        assert_eq!(set_id, "SET1");
        assert_eq!(actor, "A_g_u__");
        assert_eq!(changes.len(), 3);
        match &changes[0] {
            EmoteChange::Added(e) => {
                // the set's name for it, not the emote's original name
                assert_eq!((e.id.as_str(), e.name.as_str()), ("E1", "PotatWind"));
                assert!(e.animated && !e.zero_width);
                assert_eq!(e.urls[1].as_deref(), Some("https://cdn.7tv.app/emote/E1/2x.webp"));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&changes[1], EmoteChange::Updated { old, new } if old.name == "Before" && new.name == "After" && new.zero_width));
        assert!(matches!(&changes[2], EmoteChange::Removed(e) if e.name == "OldOne"));
    }

    #[test]
    fn actor_falls_back_to_username_and_ignores_other_keys() {
        let msg = dispatch("emote_set.update", json!({"id": "S", "actor": {"username": "bob"}, "pushed": [{"key": "name", "value": "x"}]}));
        assert_eq!(parse(&msg), Incoming::Dispatch(Dispatch::EmoteSet { set_id: "S".into(), actor: "bob".into(), changes: vec![] }));
    }

    #[test]
    fn parses_user_update_set_switch() {
        let msg = dispatch(
            "user.update",
            json!({
                "id": "USER", "kind": 1, "actor": {"display_name": "A_g_u__"},
                "updated": [{"key": "connections", "index": 0, "nested": true, "value": [
                    {"key": "emote_set", "old_value": {"id": "OLD", "name": "Old set"}, "value": {"id": "NEW", "name": "New set"}},
                    {"key": "emote_set_id", "old_value": "OLD", "value": "NEW"}
                ]}]
            }),
        );
        assert_eq!(parse(&msg), Incoming::Dispatch(Dispatch::UserSet { user_id: "USER".into(), actor: "A_g_u__".into(), set_id: "NEW".into(), set_name: "New set".into() }));
        // only the id field changed
        let msg = dispatch("user.update", json!({"id": "USER", "updated": [{"key": "connections", "value": [{"key": "emote_set_id", "value": "NEW"}]}]}));
        assert!(matches!(parse(&msg), Incoming::Dispatch(Dispatch::UserSet { set_id, .. }) if set_id == "NEW"));
        // a profile change is not a set switch
        let msg = dispatch("user.update", json!({"id": "USER", "updated": [{"key": "display_name", "old_value": "a", "value": "b"}]}));
        assert_eq!(parse(&msg), Incoming::Dispatch(Dispatch::Other));
    }

    #[test]
    fn applies_changes_to_an_emote_map() {
        let mut map = EmoteMap::new();
        let e = |id: &str, name: &str| seven_tv_emote(&active(id, name, 0));
        map.insert("Keep".into(), e("K", "Keep"));
        map.insert("Gone".into(), e("G", "Gone"));
        map.insert("Old".into(), e("R", "Old"));
        // an emote the map knew under another name is still found by id
        map.insert("Stale".into(), e("S", "Stale"));
        apply_changes(
            &mut map,
            &[
                EmoteChange::Added(e("N", "PotatWind")),
                EmoteChange::Removed(e("G", "Gone")),
                EmoteChange::Updated { old: e("R", "Old"), new: e("R", "New") },
                EmoteChange::Removed(e("S", "Renamed")),
            ],
        );
        let mut names: Vec<&str> = map.keys().map(|k| k.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["Keep", "New", "PotatWind"]);
        assert_eq!(map["New"].id, "R");
        // removing something that is not there changes nothing; a same-named emote with another id stays
        apply_changes(&mut map, &[EmoteChange::Removed(e("X", "Keep"))]);
        assert_eq!(map["Keep"].id, "K");
    }

    #[test]
    fn backoff_and_silence() {
        assert_eq!(backoff_ms(0), 1_000);
        assert_eq!(backoff_ms(1), 2_000);
        assert_eq!(backoff_ms(5), 32_000);
        assert_eq!(backoff_ms(6), 60_000);
        assert_eq!(backoff_ms(50), 60_000);
        assert_eq!(silence_limit(45_000), Duration::from_millis(95_000));
    }

    /// Against the real EventAPI (read only): Hello parses, a subscription is acknowledged.
    /// `cargo test live_event_api -- --ignored`
    #[test]
    #[ignore]
    fn live_event_api() {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<WsEvent>();
        let _g = rt.enter();
        let h = ws::connect(EVENTS_URL.to_string(), "agu-chat-test".to_string(), move |ev| {
            let _ = tx.send(ev);
        });
        let mut got_hello = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let Ok(ev) = rx.recv_timeout(Duration::from_secs(5)) else { continue };
            if let WsEvent::Message(t) = ev {
                match parse(&t) {
                    Incoming::Hello { heartbeat_ms, .. } => {
                        assert!(heartbeat_ms > 1000);
                        got_hello = true;
                        // 7TV's global emote set
                        h.send(&subscribe_msg(EMOTE_SET_UPDATE, "01HKQT8EWR000ESSWF3625XCS4"));
                    }
                    Incoming::Ack => {
                        assert!(got_hello);
                        h.close();
                        return;
                    }
                    Incoming::EndOfStream(code, msg) => panic!("end of stream {code} {msg}"),
                    _ => {}
                }
            }
        }
        panic!("no hello / ack");
    }

    #[test]
    fn ids_compare_without_case() {
        assert!(same_id("01FE9DRF000009TR6M9N941CYW", "01fe9drf000009tr6m9n941cyw"));
        assert!(!same_id("", ""));
    }
}
