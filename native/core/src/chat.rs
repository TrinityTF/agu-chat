//! The channel model both frontends drive: applies network events to per-channel message
//! lists (cap, backlog merge, deletions) and keeps the emote/badge maps.
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use crate::irc::{ChatMsg, Kind};
use crate::logf;
use crate::maps::Maps;
use crate::net::{self, Event, Net, Wake};

pub const MAX_ROWS: usize = 1000;

/// Frontend-specific per-row data (layout cache, ...). `invalidate` is called when the row's
/// content changed in place (deleted).
pub trait RowData: Default {
    fn invalidate(&mut self) {
        *self = Self::default();
    }
}

pub struct Row<X> {
    pub msg: ChatMsg,
    pub seq: u64,
    pub x: X,
}

pub struct Channel<X> {
    pub name: String,
    pub room_id: Option<String>,
    pub rows: VecDeque<Row<X>>,
    pub maps: Maps,
    /// Bumped whenever the emote/badge maps change (frontends re-tokenize on mismatch).
    pub gen: u32,
    /// Messages ever received (not capped).
    pub total: u64,
    pub seq: u64,
    emotes_started: bool,
}

impl<X: RowData> Channel<X> {
    fn new(name: &str) -> Self {
        Channel {
            name: name.to_string(),
            room_id: None,
            rows: VecDeque::new(),
            maps: Maps::default(),
            gen: 0,
            total: 0,
            seq: 0,
            emotes_started: false,
        }
    }

    fn push(&mut self, msg: ChatMsg, trimmed: &mut Vec<X>) {
        self.seq += 1;
        self.total += 1;
        self.rows.push_back(Row { msg, seq: self.seq, x: X::default() });
        while self.rows.len() > MAX_ROWS {
            if let Some(r) = self.rows.pop_front() {
                trimmed.push(r.x);
            }
        }
    }

    fn mark_deleted(&mut self, f: impl Fn(&ChatMsg) -> bool) {
        for r in self.rows.iter_mut() {
            if r.msg.kind == Kind::Chat && !r.msg.deleted && f(&r.msg) {
                r.msg.deleted = true;
                r.x.invalidate();
            }
        }
    }
}

/// What an applied event did to a channel's row list, in terms a virtualized list can follow.
pub enum Change<X> {
    Appended { ch: usize, n: usize },
    /// `n` rows dropped from the front because of the cap. `removed` has their row data when known.
    Trimmed { ch: usize, n: usize, removed: Vec<X> },
    /// Backlog rows inserted at the front.
    Prepended { ch: usize, n: usize },
    /// Rows changed in place (deleted, emote/badge maps changed) without moving.
    Restyled { ch: usize },
    /// Header-level change (connection state).
    Meta,
}

pub struct Chat<X> {
    pub channels: Vec<Channel<X>>,
    pub global: Maps,
    pub connected: bool,
    pub net: Net,
    pub rt: tokio::runtime::Handle,
    pub client: reqwest::Client,
    rx: Receiver<Event>,
    _runtime: Arc<tokio::runtime::Runtime>,
}

impl<X: RowData> Chat<X> {
    /// Start networking for `channels`. `wake` is called (from any thread) when events arrive.
    pub fn start(channels: Vec<String>, url: String, runtime: Arc<tokio::runtime::Runtime>, wake: Wake) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(net::UA)
            .timeout(Duration::from_secs(20))
            .build()
            .expect("http client");
        let (tx, rx) = crossbeam_channel::unbounded();
        let net = Net { client: client.clone(), tx, wake };
        let rt = runtime.handle().clone();
        let bench = std::env::var("AGU_IRC_URL").is_ok();
        {
            let n = net.clone();
            rt.spawn(async move { n.load_globals().await });
            for c in &channels {
                let (n, c) = (net.clone(), c.clone());
                rt.spawn(async move { n.load_channel_badges(&c).await });
            }
            rt.spawn(net::irc_task(net.clone(), url, channels.clone(), bench));
        }
        Chat {
            channels: channels.iter().map(|c| Channel::new(c)).collect(),
            global: Maps::default(),
            connected: false,
            net,
            rt,
            client,
            rx,
            _runtime: runtime,
        }
    }

    pub fn total_messages(&self) -> u64 {
        self.channels.iter().map(|c| c.total).sum()
    }

    fn idx(&self, ch: &str) -> Option<usize> {
        self.channels.iter().position(|c| c.name == ch)
    }

    /// Apply up to `max` queued events. Returns the changes and whether more are waiting.
    pub fn drain(&mut self, max: usize) -> (Vec<Change<X>>, bool) {
        let mut changes = Vec::new();
        let mut n = 0;
        while let Ok(e) = self.rx.try_recv() {
            self.apply(e, &mut changes);
            n += 1;
            if n >= max {
                return (changes, !self.rx.is_empty());
            }
        }
        (changes, false)
    }

    fn push_to(&mut self, i: usize, msg: ChatMsg, changes: &mut Vec<Change<X>>) {
        let mut trimmed = Vec::new();
        self.channels[i].push(msg, &mut trimmed);
        changes.push(Change::Appended { ch: i, n: 1 });
        if !trimmed.is_empty() {
            changes.push(Change::Trimmed { ch: i, n: trimmed.len(), removed: trimmed });
        }
    }

    fn restyle_all(&self, changes: &mut Vec<Change<X>>) {
        for i in 0..self.channels.len() {
            changes.push(Change::Restyled { ch: i });
        }
    }

    pub fn apply(&mut self, e: Event, changes: &mut Vec<Change<X>>) {
        match e {
            Event::Connected(c) => {
                self.connected = c;
                logf!("connection state: {c}");
                changes.push(Change::Meta);
            }
            Event::Msgs { ch, msgs } => {
                if let Some(i) = self.idx(&ch) {
                    for m in msgs {
                        self.push_to(i, m, changes);
                    }
                }
            }
            Event::Backlog { ch, msgs } => {
                if let Some(i) = self.idx(&ch) {
                    let s = &mut self.channels[i];
                    let have: HashSet<String> = s.rows.iter().map(|r| r.msg.id.clone()).collect();
                    let mut add = 0;
                    for m in msgs.into_iter().rev() {
                        if !m.id.is_empty() && have.contains(&m.id) {
                            continue;
                        }
                        s.seq += 1;
                        let seq = s.seq;
                        s.rows.push_front(Row { msg: m, seq, x: X::default() });
                        add += 1;
                    }
                    s.total += add as u64;
                    let mut dropped = 0;
                    while s.rows.len() > MAX_ROWS {
                        s.rows.pop_front();
                        dropped += 1;
                    }
                    // Net effect on the list: `add` rows at the front, then the oldest `dropped` gone.
                    changes.push(Change::Prepended { ch: i, n: add });
                    if dropped > 0 {
                        changes.push(Change::Trimmed { ch: i, n: dropped, removed: Vec::new() });
                    }
                }
            }
            Event::ClearUser { ch, login, secs } => {
                if let Some(i) = self.idx(&ch) {
                    let l = login.to_lowercase();
                    self.channels[i].mark_deleted(|m| m.login.to_lowercase() == l);
                    changes.push(Change::Restyled { ch: i });
                    let txt = match secs {
                        Some(d) => format!("{login} has been timed out for {d}s."),
                        None => format!("{login} has been permanently banned."),
                    };
                    self.push_to(i, ChatMsg::notice(txt, Kind::Notice), changes);
                }
            }
            Event::ClearAll(ch) => {
                if let Some(i) = self.idx(&ch) {
                    self.push_to(i, ChatMsg::notice("Chat has been cleared by a moderator.".into(), Kind::Notice), changes);
                }
            }
            Event::ClearMsg { ch, id } => {
                if let Some(i) = self.idx(&ch) {
                    self.channels[i].mark_deleted(|m| m.id == id);
                    changes.push(Change::Restyled { ch: i });
                }
            }
            Event::Notice { ch, text } => {
                if let Some(i) = self.idx(&ch) {
                    self.push_to(i, ChatMsg::notice(text, Kind::Notice), changes);
                }
            }
            Event::RoomState { ch, room_id } => {
                if let Some(i) = self.idx(&ch) {
                    let s = &mut self.channels[i];
                    if s.room_id.is_none() {
                        s.room_id = Some(room_id.clone());
                    }
                    if !s.emotes_started {
                        s.emotes_started = true;
                        logf!("room-id for {ch}: {room_id}");
                        let net = self.net.clone();
                        self.rt.spawn(async move { net.load_channel_emotes(&ch, &room_id).await });
                    }
                }
            }
            Event::Emotes { ch, provider, list } => {
                let count = list.len();
                match &ch {
                    Some(c) => {
                        let Some(i) = self.idx(c) else { return };
                        self.channels[i].gen += 1;
                        self.channels[i].maps.add_emotes(provider, true, list);
                    }
                    None => {
                        for s in self.channels.iter_mut() {
                            s.gen += 1;
                        }
                        self.global.add_emotes(provider, false, list);
                    }
                }
                logf!("emotes {} {:?}: {count}", ch.as_deref().unwrap_or("global"), provider);
                self.restyle_all(changes);
            }
            Event::Badges { ch, list } => {
                match &ch {
                    Some(c) => {
                        let Some(i) = self.idx(c) else { return };
                        self.channels[i].gen += 1;
                        self.channels[i].maps.add_badges(list);
                    }
                    None => {
                        for s in self.channels.iter_mut() {
                            s.gen += 1;
                        }
                        self.global.add_badges(list);
                    }
                }
                self.restyle_all(changes);
            }
        }
    }
}
