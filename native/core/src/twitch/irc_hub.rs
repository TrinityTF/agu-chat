//! Port of src/main/twitch/ircHub.js: ONE anonymous Twitch IRC connection (wss://irc-ws.chat.twitch.tv,
//! justinfanNNNN) shared by every Twitch source in the app: JOIN / PART per channel. The connection-health
//! logic is render/lib/ircRelay.js's (connect timeout, join timeout via ROOMSTATE, idle watchdog,
//! channel-idle re-JOIN probe, jittered backoff 1s -> 60s), reshaped for several channels on one socket: a
//! channel that will not join must not tear down the others' connection.
//!
//! JS ran this on the event loop, so state never raced. Here the state lives in a `Mailbox` (util.rs): socket
//! events, timers and add/remove calls are jobs that run one at a time, in order, which keeps the structure
//! of the JS (`_retire`, `_pump`, ...) as it was. A listener is called inline from that loop and must only
//! post work back (never block on the hub).
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::rt::{log, user_agent};
use super::vendor::irc::{parse_irc_line, IrcLine};
use crate::util::{now_ms, MailTimer, Mailbox};
use crate::ws::{self, WsEvent, WsHandle};

pub fn irc_url() -> String {
    std::env::var("AGU_IRC_URL").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "wss://irc-ws.chat.twitch.tv:443".to_string())
}

#[derive(Clone, Debug)]
pub struct HubOpts {
    pub connect_timeout_ms: u64,
    pub join_timeout_ms: u64,
    pub watchdog_interval_ms: u64,
    /// Twitch PINGs even a quiet chat, so ten silent minutes mean a dead socket
    pub idle_timeout_ms: i64,
    /// Twitch can silently stop delivering a channel while the socket stays healthy (PINGs keep coming, no
    /// PART, no RECONNECT). Re-JOIN after this much channel silence; a missing ROOMSTATE reply is proof the
    /// membership is gone. A genuinely quiet channel pays one PART/JOIN pair.
    pub channel_idle_ms: i64,
    pub probe_timeout_ms: i64,
    pub reconnect_base_ms: u64,
    pub reconnect_max_ms: u64,
    /// Twitch allows 20 JOINs per 10s for an anonymous user
    pub join_spacing_ms: u64,
    pub rejoin_after_error_ms: u64,
}

impl Default for HubOpts {
    fn default() -> Self {
        HubOpts {
            connect_timeout_ms: 15_000,
            join_timeout_ms: 15_000,
            watchdog_interval_ms: 30_000,
            idle_timeout_ms: 10 * 60_000,
            channel_idle_ms: 5 * 60_000,
            probe_timeout_ms: 20_000,
            reconnect_base_ms: 1000,
            reconnect_max_ms: 60_000,
            join_spacing_ms: 550,
            rejoin_after_error_ms: 60_000,
        }
    }
}

/// `{onJoined(roomId, {reconnect}), onDisconnected(reason), onChannelError(detail), onNotice(msgId, text),
/// onLine(parsedIrcLine)}`
pub trait HubListener: Send + Sync {
    fn on_joined(&self, _room_id: Option<String>, _reconnect: bool) {}
    fn on_disconnected(&self, _reason: &str) {}
    fn on_channel_error(&self, _detail: &str) {}
    fn on_notice(&self, _msg_id: &str, _text: &str) {}
    fn on_line(&self, _m: &IrcLine) {}
}

// Inbound commands that prove Twitch is still routing a channel to us. PING and other tmi.twitch.tv-level
// frames are excluded on purpose: a zombied JOIN keeps receiving those.
const CHANNEL_SCOPED: [&str; 6] = ["PRIVMSG", "USERNOTICE", "ROOMSTATE", "CLEARCHAT", "CLEARMSG", "NOTICE"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ConnState {
    Off,
    Connecting,
    Open,
}

impl ConnState {
    fn as_str(self) -> &'static str {
        match self {
            ConnState::Off => "off",
            ConnState::Connecting => "connecting",
            ConnState::Open => "open",
        }
    }
}

struct Entry {
    login: String,
    /// identity: `this.channels.get(login) === entry` in JS
    gen: u64,
    listener: Arc<dyn HubListener>,
    joined: bool,
    was_joined: bool,
    join_fails: u32,
    join_timer: MailTimer,
    retry_timer: MailTimer,
    last_channel_at: i64,
    /// 0 = no probe in flight
    probe_at: i64,
    probe_fails: u32,
}

impl Entry {
    fn clear(&mut self, keep_flags: bool) {
        self.join_timer.clear();
        self.retry_timer.clear();
        self.probe_at = 0;
        if !keep_flags {
            self.joined = false;
        }
    }
}

#[derive(Default, Clone)]
struct Stats {
    connect_attempts: u32,
    reconnects: u32,
    stale_resets: u32,
    last_disconnect: Option<String>,
    last_notice: Option<String>,
}

pub struct HubState {
    opts: HubOpts,
    mb: Mailbox<HubState>,
    channels: HashMap<String, Entry>,
    gen_seq: u64,
    ws: Option<(u64, WsHandle)>,
    ws_seq: u64,
    state: ConnState,
    nick: Option<String>,
    reconnect_delay: u64,
    reconnect_timer: MailTimer,
    connect_timer: MailTimer,
    watchdog_timer: MailTimer,
    join_queue: Vec<(String, u64)>,
    pump_timer: MailTimer,
    last_irc_at: i64,
    stats: Stats,
}

/// The shared connection. Cheap handle; every call just posts to the hub's event loop.
pub struct IrcHub {
    mb: Mailbox<HubState>,
}

impl IrcHub {
    /// Must be called inside the tokio runtime.
    pub fn new() -> Self {
        Self::with_opts(HubOpts::default())
    }

    pub fn with_opts(opts: HubOpts) -> Self {
        let mb = Mailbox::spawn(|mb| HubState {
            reconnect_delay: opts.reconnect_base_ms,
            opts,
            mb,
            channels: HashMap::new(),
            gen_seq: 0,
            ws: None,
            ws_seq: 0,
            state: ConnState::Off,
            nick: None,
            reconnect_timer: MailTimer::new(),
            connect_timer: MailTimer::new(),
            watchdog_timer: MailTimer::new(),
            join_queue: Vec::new(),
            pump_timer: MailTimer::new(),
            last_irc_at: 0,
            stats: Stats::default(),
        });
        IrcHub { mb }
    }

    pub fn add(&self, login: &str, listener: Arc<dyn HubListener>) {
        let login = login.to_string();
        self.mb.post(move |h| h.add(login, listener));
    }

    pub fn remove(&self, login: &str) {
        let login = login.to_string();
        self.mb.post(move |h| h.remove(&login));
    }

    pub fn close_all(&self) {
        self.mb.post(|h| h.close_all());
    }

    pub fn status(&self) -> futures_util::future::BoxFuture<'static, Value> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.mb.post(move |h| {
            let _ = tx.send(h.status());
        });
        Box::pin(async move { rx.await.unwrap_or(Value::Null) })
    }
}

impl Default for IrcHub {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for IrcHub {
    fn drop(&mut self) {
        self.mb.post(|h| h.close_all());
    }
}

fn err_text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl HubState {
    fn add(&mut self, login: String, listener: Arc<dyn HubListener>) {
        if let Some(mut old) = self.channels.remove(&login) {
            old.clear(true);
            let g = old.gen;
            self.join_queue.retain(|(l, gen)| !(l == &login && *gen == g));
        }
        self.gen_seq += 1;
        let gen = self.gen_seq;
        self.channels.insert(
            login.clone(),
            Entry {
                login: login.clone(),
                gen,
                listener,
                joined: false,
                was_joined: false,
                join_fails: 0,
                join_timer: MailTimer::new(),
                retry_timer: MailTimer::new(),
                last_channel_at: 0,
                probe_at: 0,
                probe_fails: 0,
            },
        );
        if self.state == ConnState::Open {
            self.queue_join(&login);
        } else {
            self.ensure_connected();
        }
    }

    fn remove(&mut self, login: &str) {
        let Some(mut entry) = self.channels.remove(login) else { return };
        entry.clear(true);
        let g = entry.gen;
        self.join_queue.retain(|(l, gen)| !(l == login && *gen == g));
        if self.state == ConnState::Open && self.ws.is_some() {
            self.send(&format!("PART #{login}"));
        }
        if self.channels.is_empty() {
            self.close_all();
        }
    }

    fn status(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "channels": self.channels.values().map(|e| json!({"login": e.login, "joined": e.joined, "joinFails": e.join_fails, "probeAt": if e.probe_at != 0 { json!(e.probe_at) } else { Value::Null }})).collect::<Vec<_>>(),
            "connectAttempts": self.stats.connect_attempts,
            "reconnects": self.stats.reconnects,
            "staleResets": self.stats.stale_resets,
            "lastDisconnect": self.stats.last_disconnect,
            "lastNotice": self.stats.last_notice,
        })
    }

    fn close_all(&mut self) {
        self.reconnect_timer.clear();
        self.ws = None; // dropping the handle ends the socket task
        self.clear_socket_timers();
        self.state = ConnState::Off;
        self.nick = None;
        self.join_queue.clear();
        for e in self.channels.values_mut() {
            e.clear(false);
        }
    }

    // ---------- connection ----------

    fn ensure_connected(&mut self) {
        if self.state != ConnState::Off || self.reconnect_timer.is_set() || self.channels.is_empty() {
            return;
        }
        self.state = ConnState::Connecting;
        self.connect();
    }

    fn connect(&mut self) {
        self.ws_seq += 1;
        let id = self.ws_seq;
        let mb = self.mb.clone();
        let handle = ws::connect(irc_url(), user_agent(), move |ev| mb.post(move |h| h.on_ws(id, ev)));
        self.ws = Some((id, handle));
        self.stats.connect_attempts += 1;
        log("[irc] connecting");
        let mb = self.mb.clone();
        self.connect_timer.set(&mb, Duration::from_millis(self.opts.connect_timeout_ms), move |h| h.retire(id, "WebSocket connect timeout", false, true));
    }

    fn ws_is(&self, id: u64) -> bool {
        self.ws.as_ref().map(|(i, _)| *i == id).unwrap_or(false)
    }

    fn on_ws(&mut self, id: u64, ev: WsEvent) {
        match ev {
            WsEvent::Open => {
                if !self.ws_is(id) {
                    return; // a socket we already gave up on: dropping its handle ended it
                }
                self.connect_timer.clear();
                self.last_irc_at = now_ms();
                let nick = format!("justinfan{}", 10_000 + (rand::random::<u32>() % 80_000));
                self.nick = Some(nick.clone());
                self.state = ConnState::Open;
                let ok = {
                    let (_, h) = self.ws.as_ref().unwrap();
                    h.send("CAP REQ :twitch.tv/tags twitch.tv/commands") && h.send(&format!("NICK {nick}"))
                };
                if !ok {
                    return self.retire(id, "registration send failed: socket closed", false, false);
                }
                let at = self.last_irc_at;
                let logins: Vec<String> = self.channels.keys().cloned().collect();
                for login in logins {
                    if let Some(e) = self.channels.get_mut(&login) {
                        e.last_channel_at = at; // baseline the membership clock at connect
                    }
                    self.queue_join(&login);
                }
                self.start_watchdog(id);
            }
            WsEvent::Message(data) => {
                if !self.ws_is(id) {
                    return;
                }
                self.last_irc_at = now_ms();
                self.on_data(&data, id);
            }
            WsEvent::Close(code, reason) => {
                let suffix = if reason.is_empty() { String::new() } else { format!(": {reason}") };
                let code = code.map(|c| c.to_string()).unwrap_or_else(|| "unknown".into());
                self.retire(id, &format!("WebSocket closed ({code}){suffix}"), false, false);
            }
            WsEvent::Error(e) => self.retire(id, &format!("WebSocket error: {}", err_text(e)), false, false),
        }
    }

    /// `_send`: false (after retiring the socket) when it could not be written.
    fn send(&mut self, line: &str) -> bool {
        let Some((id, h)) = self.ws.as_ref() else { return false };
        if h.send(line) {
            return true;
        }
        let id = *id;
        self.retire(id, "send failed: socket closed", false, false);
        false
    }

    fn clear_socket_timers(&mut self) {
        self.connect_timer.clear();
        self.watchdog_timer.clear();
        self.pump_timer.clear();
    }

    fn notify_disconnected(&mut self, reason: &str) {
        let mut listeners = Vec::new();
        for e in self.channels.values_mut() {
            e.joined = false;
            e.clear(true);
            listeners.push(e.listener.clone());
        }
        for l in listeners {
            l.on_disconnected(reason);
        }
    }

    // The only failure/replacement path. Dropping the socket first makes error+close, late timers and stale
    // callbacks idempotent.
    fn retire(&mut self, id: u64, reason: &str, immediate: bool, stale: bool) {
        if !self.ws_is(id) {
            return;
        }
        self.ws = None;
        self.clear_socket_timers();
        self.state = ConnState::Off;
        self.nick = None;
        self.join_queue.clear();
        self.stats.last_disconnect = Some(reason.to_string());
        if stale {
            self.stats.stale_resets += 1;
        }
        log(&format!("[irc] {reason}"));
        self.notify_disconnected(reason);
        self.schedule_reconnect(if immediate { Some(rand::random::<u64>() % 250) } else { None });
    }

    fn schedule_reconnect(&mut self, delay_override: Option<u64>) {
        if self.channels.is_empty() || self.reconnect_timer.is_set() {
            return;
        }
        let base = delay_override.unwrap_or(self.reconnect_delay);
        // Jitter regular retries so nothing re-JOINs in the same instant after a shared outage.
        let jitter = if delay_override.is_none() { rand::random::<u64>() % ((base / 4).clamp(1, 1000)) } else { 0 };
        self.stats.reconnects += 1;
        let mb = self.mb.clone();
        self.reconnect_timer.set(&mb, Duration::from_millis(base + jitter), |h| {
            h.reconnect_timer.clear();
            h.ensure_connected();
        });
        self.reconnect_delay = (self.reconnect_delay * 2).min(self.opts.reconnect_max_ms);
    }

    // ---------- joining ----------

    fn queue_join(&mut self, login: &str) {
        let Some(gen) = self.channels.get(login).map(|e| e.gen) else { return };
        if !self.join_queue.iter().any(|(l, g)| l == login && *g == gen) {
            self.join_queue.push((login.to_string(), gen));
        }
        self.pump();
    }

    fn pump(&mut self) {
        if self.pump_timer.is_set() || self.state != ConnState::Open {
            return;
        }
        if self.join_queue.is_empty() {
            return;
        }
        let (login, gen) = self.join_queue.remove(0);
        if self.channels.get(&login).map(|e| e.gen) == Some(gen) {
            self.send_join(&login);
        }
        if !self.join_queue.is_empty() {
            let mb = self.mb.clone();
            self.pump_timer.set(&mb, Duration::from_millis(self.opts.join_spacing_ms), |h| {
                h.pump_timer.clear();
                h.pump();
            });
        }
    }

    fn send_join(&mut self, login: &str) {
        let Some(ws_id) = self.ws.as_ref().map(|(i, _)| *i) else { return };
        if !self.send(&format!("JOIN #{login}")) {
            return;
        }
        let Some(gen) = self.channels.get(login).map(|e| e.gen) else { return };
        let mb = self.mb.clone();
        let l = login.to_string();
        let timeout = Duration::from_millis(self.opts.join_timeout_ms);
        if let Some(e) = self.channels.get_mut(login) {
            e.join_timer.set(&mb, timeout, move |h| h.join_timeout(&l, gen, ws_id));
        }
    }

    // Transport-open is not channel-open: Twitch confirms a JOIN with ROOMSTATE. No ROOMSTATE in time -> try
    // once more; a second miss is reported to the channel, and the whole socket is retired only when nothing
    // on it has joined (then the socket itself is the suspect).
    fn join_timeout(&mut self, login: &str, gen: u64, ws_id: u64) {
        let Some(e) = self.channels.get_mut(login) else { return };
        if e.gen != gen {
            return;
        }
        e.join_timer.clear();
        if !self.ws_is(ws_id) || self.channels[login].joined {
            return;
        }
        let e = self.channels.get_mut(login).unwrap();
        e.join_fails += 1;
        if e.join_fails == 1 {
            log(&format!("[irc:{login}] join timeout, retrying"));
            if self.send(&format!("PART #{login}")) {
                self.queue_join(login);
            }
            return;
        }
        let listener = e.listener.clone();
        listener.on_channel_error(&format!("Could not join #{login}. The channel may not exist, or Twitch is not answering."));
        if !self.channels.values().any(|e| e.joined) {
            return self.retire(ws_id, &format!("channel join timeout (#{login})"), false, true);
        }
        let mb = self.mb.clone();
        let l = login.to_string();
        let delay = Duration::from_millis(self.opts.rejoin_after_error_ms);
        let e = self.channels.get_mut(login).unwrap();
        e.retry_timer.set(&mb, delay, move |h| {
            let ok = h.ws_is(ws_id) && h.channels.get(&l).map(|e| e.gen == gen && !e.joined).unwrap_or(false);
            if let Some(e) = h.channels.get_mut(&l) {
                e.retry_timer.clear();
            }
            if ok {
                h.queue_join(&l);
            }
        });
    }

    fn mark_joined(&mut self, login: &str, room_id: Option<String>) {
        let Some(e) = self.channels.get_mut(login) else { return };
        e.join_timer.clear();
        e.joined = true;
        e.join_fails = 0;
        e.probe_fails = 0;
        e.last_channel_at = now_ms();
        let reconnect = e.was_joined;
        e.was_joined = true;
        let listener = e.listener.clone();
        self.reconnect_delay = self.opts.reconnect_base_ms;
        log(&format!("[irc:{login}] joined"));
        listener.on_joined(room_id, reconnect);
    }

    // ---------- watchdog ----------

    fn start_watchdog(&mut self, ws_id: u64) {
        self.watchdog_timer.clear();
        if self.opts.watchdog_interval_ms == 0 || self.opts.idle_timeout_ms <= 0 {
            return;
        }
        let mb = self.mb.clone();
        let every = Duration::from_millis(self.opts.watchdog_interval_ms);
        self.watchdog_timer.every(&mb, every, move |h| h.watchdog_tick(ws_id));
    }

    fn watchdog_tick(&mut self, ws_id: u64) {
        if !self.ws_is(ws_id) {
            return;
        }
        let now = now_ms();
        let idle = now - self.last_irc_at;
        if idle > self.opts.idle_timeout_ms {
            return self.retire(ws_id, &format!("no inbound IRC traffic for {}s", idle / 1000), false, true);
        }
        let logins: Vec<String> = self.channels.keys().cloned().collect();
        for login in logins {
            if !self.ws_is(ws_id) {
                return;
            }
            if self.channels.get(&login).map(|e| e.joined).unwrap_or(false) {
                self.check_membership(&login, ws_id, now);
            }
        }
    }

    fn check_membership(&mut self, login: &str, ws_id: u64, now: i64) {
        let (channel_idle_ms, probe_timeout_ms) = (self.opts.channel_idle_ms, self.opts.probe_timeout_ms);
        if channel_idle_ms <= 0 {
            return;
        }
        let (probe_at, probe_fails, last_channel_at) = {
            let e = &self.channels[login];
            (e.probe_at, e.probe_fails, e.last_channel_at)
        };
        if probe_at != 0 {
            if now - probe_at <= probe_timeout_ms {
                return;
            }
            let fails = probe_fails + 1;
            {
                let e = self.channels.get_mut(login).unwrap();
                e.probe_at = 0;
                e.probe_fails = fails;
            }
            // Another channel still delivering means the socket is fine and only this JOIN died.
            let others_alive = self.channels.values().any(|e| e.login != login && e.joined && now - e.last_channel_at <= channel_idle_ms);
            if !others_alive || fails >= 2 {
                return self.retire(ws_id, &format!("channel membership probe went unanswered (#{login})"), false, true);
            }
            self.channels.get_mut(login).unwrap().joined = false;
            self.queue_join(login);
            return;
        }
        if now - last_channel_at <= channel_idle_ms {
            return;
        }
        // Set before sending: the PART echo arrives on this same socket and must not read as an eviction.
        self.channels.get_mut(login).unwrap().probe_at = now;
        log(&format!("[irc:{login}] channel silent; re-JOINing to verify membership"));
        if self.send(&format!("PART #{login}")) {
            self.send(&format!("JOIN #{login}"));
        }
    }

    // ---------- inbound ----------

    fn on_data(&mut self, data: &str, ws_id: u64) {
        for line in data.split("\r\n") {
            if line.is_empty() {
                continue;
            }
            if line.starts_with("PING") {
                // Twitch requires the PONG payload to match the PING payload.
                if !self.send(&format!("PONG{}", &line[4..])) {
                    return;
                }
                continue;
            }
            let m = parse_irc_line(line);
            let has_entry = !m.channel.is_empty() && self.channels.contains_key(&m.channel);
            if has_entry && CHANNEL_SCOPED.contains(&m.command.as_str()) {
                self.channels.get_mut(&m.channel).unwrap().last_channel_at = now_ms();
            }
            if m.command == "RECONNECT" {
                return self.retire(ws_id, "Twitch requested RECONNECT", true, false);
            }
            if m.command == "NOTICE" {
                let text = if m.text.is_empty() { "IRC NOTICE".to_string() } else { m.text.clone() };
                self.stats.last_notice = Some(text.clone());
                log(&format!("[irc] NOTICE{}: {}", if m.channel.is_empty() { String::new() } else { format!(" #{}", m.channel) }, text));
                if has_entry {
                    let l = self.channels[&m.channel].listener.clone();
                    l.on_notice(m.tag("msg-id"), &m.text);
                }
                continue;
            }
            if !has_entry {
                continue;
            }
            let login = m.channel.clone();
            if m.command == "ROOMSTATE" {
                let joined = {
                    let e = self.channels.get_mut(&login).unwrap();
                    e.probe_at = 0;
                    e.probe_fails = 0;
                    e.joined
                };
                if !joined {
                    let rid = Some(m.tag("room-id")).filter(|s| !s.is_empty()).map(String::from);
                    self.mark_joined(&login, rid);
                }
            } else if m.command == "PART" && self.nick.as_deref() == Some(m.user.as_str()) {
                // A membership probe PARTs on purpose before re-JOINing, and a retry does too.
                let e = &self.channels[&login];
                if e.probe_at != 0 || !e.joined {
                    continue;
                }
                return self.retire(ws_id, &format!("Twitch removed us from #{login}"), true, false);
            }
            if let Some(e) = self.channels.get(&login) {
                let l = e.listener.clone();
                l.on_line(&m);
            }
            if !self.ws_is(ws_id) {
                return;
            }
        }
    }
}
