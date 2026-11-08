//! Port of src/renderer/chat/store.ts: the per-pane message store, a frame reducer for every source of a split,
//! merged into ONE ordered row list. Frame handling is the socket handler of the site's Chat.tsx and the
//! Rumble / Kick / YouTube / X hooks. Rows are immutable (`Arc<Row>`): a delete REPLACES the row, so caches
//! keyed by the row pointer notice. The JS batched notifications per animation frame; here the pane drains its
//! frame queue and refreshes once per drain.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agu_chat_core::protocol::{ConnFrame, Platform, SourceRef};
use serde_json::Value;

use super::twitch::parts::KnownChatter;
use super::types::*;

pub fn source_key(s: &SourceRef) -> String {
    format!("{}:{}", s.platform.as_str(), s.channel)
}

pub fn platform_of_key(sk: &str) -> Platform {
    match sk.split(':').next().unwrap_or("") {
        "kick" => Platform::Kick,
        "youtube" => Platform::Youtube,
        "rumble" => Platform::Rumble,
        "x" => Platform::X,
        _ => Platform::Twitch,
    }
}

pub struct SourceState {
    pub sk: String,
    pub source: SourceRef,
    pub conn: Option<ConnFrame>,
    /// Twitch channel id (7TV/BTTV/FFZ are keyed by it); from a frame's `channelId`.
    pub channel_id: Option<String>,
    pub loaded: bool,
    pub pinned: Option<ChatMsg>,
    pub suspicious: HashMap<String, SuspiciousUser>,
    pub esub: EsubStatus,
    pub modes: Option<Value>,
    pub status: Option<Value>,
    pub rumble_badges: RumbleBadgeDefs,
    pub rumble_emotes: RumbleEmotes,
    pub kick_emotes: Value,
    pub known: HashMap<String, KnownChatter>,
}

impl SourceState {
    fn new(source: SourceRef) -> Self {
        SourceState {
            sk: source_key(&source),
            source,
            conn: None,
            channel_id: None,
            loaded: false,
            pinned: None,
            suspicious: HashMap::new(),
            esub: EsubStatus::default(),
            modes: None,
            status: None,
            rumble_badges: Default::default(),
            rumble_emotes: Default::default(),
            kick_emotes: Value::Null,
            known: HashMap::new(),
        }
    }
}

const ROW_CAP: usize = 1000;
/// while the user has scrolled up, trimming from the top would move what they are reading
const ROW_CAP_HELD: usize = 1600;

#[derive(Default)]
pub struct ChatStore {
    /// In the order the split lists them.
    pub sources: Vec<SourceState>,
    rows: Vec<RowRef>,
    keys: HashSet<String>,
    seq: u64,
    arrival: u64,
    held: bool,
}

fn val_str(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

impl ChatStore {
    pub fn rows(&self) -> &[RowRef] {
        &self.rows
    }

    pub fn source(&self, sk: &str) -> Option<&SourceState> {
        self.sources.iter().find(|s| s.sk == sk)
    }

    pub fn source_mut(&mut self, sk: &str) -> Option<&mut SourceState> {
        self.sources.iter_mut().find(|s| s.sk == sk)
    }

    /// Make the store hold exactly these sources: new ones get state, dropped ones lose their rows.
    pub fn sync(&mut self, refs: &[SourceRef]) {
        let want: Vec<String> = refs.iter().map(source_key).collect();
        let dropped: Vec<String> = self.sources.iter().filter(|s| !want.contains(&s.sk)).map(|s| s.sk.clone()).collect();
        for sk in dropped {
            self.sources.retain(|s| s.sk != sk);
            self.remove_rows_of(&sk);
        }
        for r in refs {
            if self.source(&source_key(r)).is_none() {
                self.sources.push(SourceState::new(r.clone()));
            }
        }
    }

    /// A fresh subscription replays a snapshot: forget what we knew about this source's chat first.
    pub fn reset_source(&mut self, sk: &str) {
        if self.source(sk).is_none() {
            return;
        }
        self.remove_rows_of(sk);
        let st = self.source_mut(sk).unwrap();
        st.loaded = false;
        st.pinned = None;
        st.suspicious.clear();
    }

    pub fn set_held(&mut self, held: bool) -> bool {
        if self.held == held {
            return false;
        }
        self.held = held;
        if !held {
            return self.trim();
        }
        false
    }

    /// A channel id learned from an API answer rather than a frame.
    pub fn set_channel_id(&mut self, sk: &str, id: &str) -> bool {
        match self.source_mut(sk) {
            Some(st) if st.channel_id.as_deref() != Some(id) => {
                st.channel_id = Some(id.to_string());
                true
            }
            _ => false,
        }
    }

    /// Local edits (e.g. a delete the user just did): mark a Twitch message deleted.
    pub fn mark_deleted(&mut self, sk: &str, id: &str) {
        self.map_rows(|r| match &r.body {
            RowBody::Msg(m) if r.sk == sk && m.id == id && !m.deleted => Some(with_body(r, RowBody::Msg(ChatMsg { deleted: true, ..m.clone() }))),
            _ => None,
        });
    }

    /// The relay says an AutoMod hold is gone (404): no broadcast will follow, so flip it here.
    pub fn mark_automod_expired(&mut self, sk: &str, id: &str) {
        self.map_rows(|r| match &r.body {
            RowBody::Automod(h) if r.sk == sk && h.id == id && h.status == "held" => Some(with_body(r, RowBody::Automod(HeldMsg { status: "expired".into(), ..h.clone() }))),
            _ => None,
        });
    }

    // ---- internals --------------------------------------------------------------------------------

    fn remove_rows_of(&mut self, sk: &str) {
        let keys = &mut self.keys;
        self.rows.retain(|r| {
            if r.sk != sk {
                return true;
            }
            keys.remove(&r.key);
            false
        });
    }

    fn trim(&mut self) -> bool {
        let cap = if self.held { ROW_CAP_HELD } else { ROW_CAP };
        if self.rows.len() <= cap {
            return false;
        }
        let n = self.rows.len() - cap;
        for r in self.rows.drain(..n) {
            self.keys.remove(&r.key);
        }
        true
    }

    fn map_rows(&mut self, f: impl Fn(&RowRef) -> Option<RowRef>) -> bool {
        let mut changed = false;
        for r in self.rows.iter_mut() {
            if let Some(n) = f(r) {
                *r = n;
                changed = true;
            }
        }
        changed
    }

    fn add_row(&mut self, mut row: Row) {
        if self.keys.contains(&row.key) {
            return;
        }
        self.keys.insert(row.key.clone());
        self.arrival += 1;
        row.ord = self.arrival;
        let row = Arc::new(row);
        match self.rows.last() {
            Some(last) if row.ts < last.ts => {
                // Out of order (a co-chat's batch, a late backlog line): upper-bound insert by ts.
                let at = self.rows.partition_point(|r| r.ts <= row.ts);
                self.rows.insert(at, row);
            }
            _ => self.rows.push(row),
        }
    }

    fn replace_backlog(&mut self, sk: &str, mut rows: Vec<Row>) {
        self.remove_rows_of(sk);
        rows.sort_by_key(|r| r.ts);
        for mut r in rows {
            r.backlog = true;
            self.add_row(r);
        }
        self.trim();
    }

    fn register(st: &mut SourceState, u: &ChatUser) {
        if u.login.is_empty() {
            return;
        }
        st.known.insert(u.login.to_lowercase(), KnownChatter { name: u.name.clone(), color: u.color.clone(), id: u.id.clone() });
    }

    fn mk(sk: &str, prefix: &str, id: &str) -> String {
        format!("{sk}|{prefix}:{id}")
    }

    fn row(sk: &str, platform: Platform, key: String, ts: i64, body: RowBody) -> Row {
        Row { key, ts, sk: sk.to_string(), platform, ord: 0, backlog: false, body }
    }

    // ---- frames -----------------------------------------------------------------------------------

    /// Reduce one frame into the store; true when something visible changed.
    pub fn apply(&mut self, sk: &str, frame: &Value) -> bool {
        let Some(ty) = frame.get("type").and_then(|t| t.as_str()) else { return false };
        let Some(st) = self.source_mut(sk) else { return false };
        super::mod_history::observe(sk, ty, frame);
        if let Some(cid) = frame.get("channelId").filter(|c| !c.is_null()) {
            let cid = match cid {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            if st.channel_id.as_deref() != Some(cid.as_str()) {
                st.channel_id = Some(cid);
            }
        }
        if ty == "conn" {
            st.conn = Some(parse_conn(frame));
            return true;
        }
        let platform = st.source.platform;
        match platform {
            Platform::Twitch => self.twitch(sk, ty, frame),
            Platform::Rumble => self.rumble(sk, ty, frame),
            Platform::Kick => self.kick(sk, ty, frame),
            Platform::Youtube => self.youtube(sk, ty, frame),
            Platform::X => self.xchat(sk, ty, frame),
        }
    }

    fn parse<T: serde::de::DeserializeOwned + Default>(v: &Value) -> T {
        serde_json::from_value(v.clone()).unwrap_or_default()
    }

    fn twitch(&mut self, sk: &str, ty: &str, f: &Value) -> bool {
        let msg_row = |m: ChatMsg| Self::row(sk, Platform::Twitch, Self::mk(sk, "m", &m.id), m.ts, RowBody::Msg(m));
        let notice_row = |n: Notice| Self::row(sk, Platform::Twitch, Self::mk(sk, "n", &n.id), n.ts, RowBody::Notice(n));
        match ty {
            "chatlog" => {
                let mut rows: Vec<Row> = Vec::new();
                let mut users: Vec<ChatUser> = Vec::new();
                for raw in f.get("messages").and_then(|m| m.as_array()).into_iter().flatten() {
                    if raw.is_null() {
                        continue;
                    }
                    let is_notice = raw.get("notice").map(|n| !n.is_null() && n != &Value::Bool(false)).unwrap_or(false);
                    if is_notice {
                        let n: Notice = Self::parse(raw);
                        users.push(n.user.clone());
                        rows.push(notice_row(n));
                    } else {
                        let m: ChatMsg = Self::parse(raw);
                        users.push(m.user.clone());
                        rows.push(msg_row(m));
                    }
                }
                for x in f.get("modfeed").and_then(|m| m.as_array()).into_iter().flatten() {
                    rows.push(self.feed_row(sk, x));
                }
                for h in f.get("automod").and_then(|m| m.as_array()).into_iter().flatten() {
                    let h: HeldMsg = Self::parse(h);
                    rows.push(Self::row(sk, Platform::Twitch, Self::mk(sk, "a", &h.id), h.ts, RowBody::Automod(h)));
                }
                self.replace_backlog(sk, rows);
                let st = self.source_mut(sk).unwrap();
                for u in &users {
                    Self::register(st, u);
                }
                st.suspicious = f
                    .get("suspicious")
                    .and_then(|s| s.as_array())
                    .into_iter()
                    .flatten()
                    .map(|u| Self::parse::<SuspiciousUser>(u))
                    .map(|u| (u.id.clone(), u))
                    .collect();
                st.esub = parse_esub(f.get("esub"));
                st.pinned = f.get("pinned").filter(|p| !p.is_null()).map(Self::parse::<ChatMsg>);
                st.loaded = true;
            }
            "chat" => {
                let Some(m) = f.get("msg").filter(|m| !m.is_null()) else { return false };
                let m: ChatMsg = Self::parse(m);
                Self::register(self.source_mut(sk).unwrap(), &m.user);
                self.add_row(msg_row(m));
                self.trim();
            }
            "chatnotice" => {
                let Some(n) = f.get("notice").filter(|n| !n.is_null()) else { return false };
                let n: Notice = Self::parse(n);
                Self::register(self.source_mut(sk).unwrap(), &n.user);
                self.add_row(notice_row(n));
                self.trim();
            }
            // A reward the relay learned after its redeem's message went out.
            "rewardinfo" => {
                let rid = val_str(f, "rewardId");
                let reward: RewardInfo = Self::parse(f.get("reward").unwrap_or(&Value::Null));
                return self.map_rows(|r| match &r.body {
                    RowBody::Msg(m) if r.sk == sk && m.reward_id.as_deref() == Some(rid.as_str()) && m.reward.is_none() => {
                        Some(with_body(r, RowBody::Msg(ChatMsg { reward: Some(reward.clone()), ..m.clone() })))
                    }
                    _ => None,
                });
            }
            "clearchat" => {
                let user_id = f.get("userId").filter(|u| !u.is_null()).map(|u| u.as_str().map(|s| s.to_string()).unwrap_or_else(|| u.to_string()));
                let login = val_str(f, "login");
                return self.map_rows(|r| match &r.body {
                    // Same test as the site: no user = the whole chat was cleared.
                    RowBody::Msg(m) if r.sk == sk && !m.deleted && (user_id.is_none() || user_id.as_deref() == Some(m.user.id.as_str()) || (!login.is_empty() && m.user.login == login)) => {
                        Some(with_body(r, RowBody::Msg(ChatMsg { deleted: true, ..m.clone() })))
                    }
                    _ => None,
                });
            }
            "clearmsg" => {
                let id = val_str(f, "id");
                return self.map_rows(|r| match &r.body {
                    RowBody::Msg(m) if r.sk == sk && m.id == id && !m.deleted => Some(with_body(r, RowBody::Msg(ChatMsg { deleted: true, ..m.clone() }))),
                    _ => None,
                });
            }
            "pin" => {
                self.source_mut(sk).unwrap().pinned = f.get("msg").filter(|m| !m.is_null()).map(Self::parse::<ChatMsg>);
            }
            "modfeed" => {
                let Some(feed) = f.get("feed").filter(|x| !x.is_null()) else { return false };
                let row = self.feed_row(sk, feed);
                self.add_row(row);
                self.trim();
            }
            "modes" => {
                self.source_mut(sk).unwrap().modes = f.get("settings").cloned();
            }
            "automod" => {
                let Some(h) = f.get("held").filter(|h| !h.is_null()) else { return false };
                let h: HeldMsg = Self::parse(h);
                self.add_row(Self::row(sk, Platform::Twitch, Self::mk(sk, "a", &h.id), h.ts, RowBody::Automod(h)));
                self.trim();
            }
            // A held message got resolved (by us or another mod): flip its status in place.
            "automodupdate" => {
                let (id, status, by) = (val_str(f, "id"), val_str(f, "status"), f.get("resolvedBy").and_then(|x| x.as_str()).map(|s| s.to_string()));
                return self.map_rows(|r| match &r.body {
                    RowBody::Automod(h) if r.sk == sk && h.id == id => Some(with_body(r, RowBody::Automod(HeldMsg { status: status.clone(), resolved_by: by.clone(), ..h.clone() }))),
                    _ => None,
                });
            }
            // A suspicious user spoke; `msg` is only non-null for restricted users.
            "suspicious" => {
                if let Some(u) = f.get("user").filter(|u| !u.is_null()) {
                    let u: SuspiciousUser = Self::parse(u);
                    self.source_mut(sk).unwrap().suspicious.insert(u.id.clone(), u);
                }
                if let Some(m) = f.get("msg").filter(|m| !m.is_null()) {
                    let m: ChatMsg = Self::parse(m);
                    Self::register(self.source_mut(sk).unwrap(), &m.user);
                    self.add_row(msg_row(m));
                    self.trim();
                }
            }
            "suspicioususer" => {
                let uid = val_str(f, "userId");
                let st = self.source_mut(sk).unwrap();
                match f.get("user").filter(|u| !u.is_null()) {
                    Some(u) => {
                        st.suspicious.insert(uid, Self::parse(u));
                    }
                    None => {
                        st.suspicious.remove(&uid);
                    }
                }
            }
            "esub" => {
                self.source_mut(sk).unwrap().esub = parse_esub(f.get("esub"));
            }
            _ => return false,
        }
        true
    }

    fn feed_row(&mut self, sk: &str, x: &Value) -> Row {
        self.seq += 1;
        let ts = x.get("ts").and_then(|t| t.as_i64()).unwrap_or(0);
        let kind = x.get("feedKind").and_then(|k| k.as_str()).or_else(|| x.get("kind").and_then(|k| k.as_str())).unwrap_or("mode").to_string();
        let entry = ModFeedEntry { ts, feed_kind: kind, text: val_str(x, "text") };
        Self::row(sk, Platform::Twitch, Self::mk(sk, "f", &self.seq.to_string()), ts, RowBody::ModFeed(entry))
    }

    fn rumble(&mut self, sk: &str, ty: &str, f: &Value) -> bool {
        let row = |m: RumbleMsg| Self::row(sk, Platform::Rumble, Self::mk(sk, "r", &m.id), m.ts, RowBody::Rumble(m));
        match ty {
            // Sent twice by design (empty on connect, again once history lands): replaces the list wholesale.
            "chatlog" => {
                let rows: Vec<Row> = arr(f, "messages").iter().map(|m| row(Self::parse(m))).collect();
                self.replace_backlog(sk, rows);
                let st = self.source_mut(sk).unwrap();
                st.status = f.get("status").filter(|s| !s.is_null()).cloned();
                if let Some(b) = f.get("badges").and_then(|b| b.as_object()) {
                    st.rumble_badges = b.clone();
                }
                if let Some(e) = f.get("emotes").and_then(|b| b.as_object()) {
                    st.rumble_emotes = e.clone();
                }
                st.loaded = true;
            }
            "chat" => {
                let Some(m) = f.get("msg").or_else(|| f.get("message")).filter(|m| !m.is_null()) else { return false };
                self.add_row(row(Self::parse(m)));
                self.trim();
            }
            "status" => self.source_mut(sk).unwrap().status = f.get("status").filter(|s| !s.is_null()).cloned(),
            "badges" => self.source_mut(sk).unwrap().rumble_badges = f.get("badges").and_then(|b| b.as_object()).cloned().unwrap_or_default(),
            "emotes" => self.source_mut(sk).unwrap().rumble_emotes = f.get("emotes").and_then(|b| b.as_object()).cloned().unwrap_or_default(),
            "delete" => {
                let ids: HashSet<String> = arr(f, "ids").iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
                return self.map_rows(|r| match &r.body {
                    RowBody::Rumble(m) if r.sk == sk && ids.contains(&m.id) && !m.deleted => Some(with_body(r, RowBody::Rumble(RumbleMsg { deleted: true, ..m.clone() }))),
                    _ => None,
                });
            }
            "mute" => {
                let users: HashSet<String> = arr(f, "userIds").iter().filter_map(|x| x.as_str().map(|s| s.to_string()).or_else(|| Some(x.to_string()))).collect();
                return self.map_rows(|r| match &r.body {
                    RowBody::Rumble(m) if r.sk == sk && users.contains(&m.user.id) && !m.deleted => Some(with_body(r, RowBody::Rumble(RumbleMsg { deleted: true, ..m.clone() }))),
                    _ => None,
                });
            }
            "unconfigured" => self.source_mut(sk).unwrap().loaded = true,
            _ => return false,
        }
        true
    }

    fn kick(&mut self, sk: &str, ty: &str, f: &Value) -> bool {
        let row = |m: KickMsg| Self::row(sk, Platform::Kick, Self::mk(sk, "k", &m.id), m.ts, RowBody::Kick(m));
        match ty {
            "chatlog" => {
                let rows: Vec<Row> = arr(f, "messages").iter().map(|m| row(Self::parse(m))).collect();
                self.replace_backlog(sk, rows);
                let st = self.source_mut(sk).unwrap();
                st.status = f.get("status").filter(|s| !s.is_null()).cloned();
                if let Some(e) = f.get("emotes").filter(|e| !e.is_null()) {
                    st.kick_emotes = e.clone();
                }
                st.loaded = true;
            }
            // The site's Kick hook reads `data.message`, Rumble's reads `data.msg`: accept either.
            "chat" => {
                let Some(m) = f.get("message").or_else(|| f.get("msg")).filter(|m| !m.is_null()) else { return false };
                self.add_row(row(Self::parse(m)));
                self.trim();
            }
            "status" => self.source_mut(sk).unwrap().status = f.get("status").filter(|s| !s.is_null()).cloned(),
            "emotes" => self.source_mut(sk).unwrap().kick_emotes = f.get("emotes").cloned().unwrap_or(Value::Null),
            // Kick's ban event clears that user's history, matching what its own chat does.
            "banned" => {
                let uid = f.get("userId").map(|u| u.as_str().map(|s| s.to_string()).unwrap_or_else(|| u.to_string())).unwrap_or_default();
                return self.map_rows(|r| match &r.body {
                    RowBody::Kick(m) if r.sk == sk && m.user.id == uid && !m.deleted => Some(with_body(r, RowBody::Kick(KickMsg { deleted: true, ..m.clone() }))),
                    _ => None,
                });
            }
            "unconfigured" => self.source_mut(sk).unwrap().loaded = true,
            _ => return false,
        }
        true
    }

    /// YouTube: a poll returns many messages, shipped together in `messages`.
    fn youtube(&mut self, sk: &str, ty: &str, f: &Value) -> bool {
        let row = |m: YouTubeMsg| Self::row(sk, Platform::Youtube, Self::mk(sk, "y", &m.id), m.ts, RowBody::YouTube(m));
        match ty {
            "chatlog" => {
                let rows: Vec<Row> = arr(f, "messages").iter().map(|m| row(Self::parse(m))).collect();
                self.replace_backlog(sk, rows);
                let st = self.source_mut(sk).unwrap();
                st.status = f.get("status").filter(|s| !s.is_null()).cloned();
                st.loaded = true;
            }
            "chat" => {
                for m in arr(f, "messages") {
                    self.add_row(row(Self::parse(m)));
                }
                self.trim();
            }
            "status" => self.source_mut(sk).unwrap().status = f.get("status").filter(|s| !s.is_null()).cloned(),
            "unconfigured" => self.source_mut(sk).unwrap().loaded = true,
            _ => return false,
        }
        true
    }

    fn xchat(&mut self, sk: &str, ty: &str, f: &Value) -> bool {
        let row = |m: XMsg| Self::row(sk, Platform::X, Self::mk(sk, "x", &m.id), m.ts, RowBody::X(m));
        match ty {
            "chatlog" => {
                let rows: Vec<Row> = arr(f, "messages").iter().map(|m| row(Self::parse(m))).collect();
                self.replace_backlog(sk, rows);
                let st = self.source_mut(sk).unwrap();
                st.status = f.get("status").filter(|s| !s.is_null()).cloned();
                st.loaded = true;
            }
            "chat" => {
                for m in arr(f, "messages") {
                    self.add_row(row(Self::parse(m)));
                }
                self.trim();
            }
            // X chat carries user ids, not names: when a name turns up it belongs on what is already on screen.
            "names" => {
                let users: HashMap<String, XUser> = arr(f, "users").iter().map(|u| Self::parse::<XUser>(u)).map(|u| (u.id.clone(), u)).collect();
                if users.is_empty() {
                    return false;
                }
                return self.map_rows(|r| match &r.body {
                    RowBody::X(m) if r.sk == sk && users.contains_key(&m.user.id) => Some(with_body(r, RowBody::X(XMsg { user: users[&m.user.id].clone(), ..m.clone() }))),
                    _ => None,
                });
            }
            "status" => self.source_mut(sk).unwrap().status = f.get("status").filter(|s| !s.is_null()).cloned(),
            "unconfigured" => self.source_mut(sk).unwrap().loaded = true,
            _ => return false,
        }
        true
    }
}

/// Lenient `conn` frame reader (a channel id may arrive as a number).
fn parse_conn(f: &Value) -> ConnFrame {
    let s = |k: &str| f.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
    ConnFrame {
        state: s("state").unwrap_or_default(),
        detail: s("detail"),
        live: f.get("live").and_then(|v| v.as_bool()),
        title: s("title"),
        can_send: f.get("canSend").and_then(|v| v.as_bool()),
        read_only_reason: s("readOnlyReason"),
        channel_id: f.get("channelId").filter(|v| !v.is_null()).map(|v| v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string())),
    }
}

fn arr<'a>(f: &'a Value, k: &str) -> &'a [Value] {
    f.get(k).and_then(|a| a.as_array()).map(|a| a.as_slice()).unwrap_or(&[])
}

fn parse_esub(v: Option<&Value>) -> EsubStatus {
    match v {
        Some(v) => EsubStatus { active: v.get("active").and_then(|a| a.as_bool()).unwrap_or(false), reason: v.get("reason").and_then(|r| r.as_str()).map(|s| s.to_string()) },
        None => EsubStatus::default(),
    }
}

fn with_body(r: &RowRef, body: RowBody) -> RowRef {
    Arc::new(Row { body, ..(**r).clone() })
}

/// Display names newest-first, for @name completion.
pub fn chatter_names(rows: &[RowRef], sks: Option<&HashSet<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for r in rows.iter().rev() {
        if out.len() >= 400 {
            break;
        }
        if let Some(s) = sks {
            if !s.contains(&r.sk) {
                continue;
            }
        }
        let name = match &r.body {
            RowBody::Msg(m) => pick(&m.user.name, &m.user.login),
            RowBody::Rumble(m) => pick(&m.user.name, &m.user.login),
            RowBody::Kick(m) => pick(&m.user.name, &m.user.login),
            RowBody::YouTube(m) => m.user.name.clone(),
            RowBody::X(m) => m.user.name.clone(),
            RowBody::Notice(n) => n.user.name.clone(),
            _ => String::new(),
        };
        if name.is_empty() {
            continue;
        }
        if seen.insert(name.to_lowercase()) {
            out.push(name);
        }
    }
    out
}

fn pick(a: &str, b: &str) -> String {
    if a.is_empty() { b.to_string() } else { a.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (ChatStore, String) {
        let mut s = ChatStore::default();
        let r = SourceRef { platform: Platform::Twitch, channel: "agu".into() };
        s.sync(&[r.clone()]);
        (s, source_key(&r))
    }

    fn chat(id: &str, ts: i64, user: &str) -> Value {
        json!({"type": "chat", "msg": {"id": id, "ts": ts, "user": {"id": format!("u-{user}"), "login": user, "name": user, "color": ""}, "text": "hi", "badges": "", "emotes": ""}})
    }

    #[test]
    fn chat_rows_order_and_dedupe() {
        let (mut s, sk) = store();
        s.apply(&sk, &chat("1", 10, "a"));
        s.apply(&sk, &chat("2", 5, "b"));
        s.apply(&sk, &chat("1", 10, "a"));
        assert_eq!(s.rows().len(), 2);
        assert_eq!(s.rows()[0].ts, 5);
        assert!(s.source(&sk).unwrap().known.contains_key("a"));
    }

    #[test]
    fn clearmsg_replaces_the_row_arc() {
        let (mut s, sk) = store();
        s.apply(&sk, &chat("1", 10, "a"));
        let before = s.rows()[0].clone();
        s.apply(&sk, &json!({"type": "clearmsg", "id": "1"}));
        assert!(!Arc::ptr_eq(&before, &s.rows()[0]));
        assert!(matches!(&s.rows()[0].body, RowBody::Msg(m) if m.deleted));
    }

    #[test]
    fn clearchat_with_user_only_hits_that_user() {
        let (mut s, sk) = store();
        s.apply(&sk, &chat("1", 10, "a"));
        s.apply(&sk, &chat("2", 11, "b"));
        s.apply(&sk, &json!({"type": "clearchat", "userId": "u-a", "login": "a"}));
        let del: Vec<bool> = s.rows().iter().map(|r| matches!(&r.body, RowBody::Msg(m) if m.deleted)).collect();
        assert_eq!(del, vec![true, false]);
        s.apply(&sk, &json!({"type": "clearchat"}));
        assert!(s.rows().iter().all(|r| matches!(&r.body, RowBody::Msg(m) if m.deleted)));
    }

    #[test]
    fn chatlog_replaces_and_marks_backlog() {
        let (mut s, sk) = store();
        s.apply(&sk, &chat("1", 10, "a"));
        s.apply(&sk, &json!({"type": "chatlog", "messages": [{"id": "9", "ts": 3, "user": {"id": "1", "login": "z", "name": "Z", "color": ""}, "text": "old"}, {"id": "n1", "ts": 4, "notice": true, "system": "Z subscribed", "msgId": "sub", "user": {"id": "1", "login": "z", "name": "Z", "color": ""}, "text": ""}], "modfeed": [{"ts": 5, "kind": "ban", "text": "x banned y"}], "pinned": null, "esub": {"active": true, "reason": null}}));
        assert_eq!(s.rows().len(), 3);
        assert!(s.rows().iter().all(|r| r.backlog));
        assert!(matches!(&s.rows()[1].body, RowBody::Notice(_)));
        assert!(matches!(&s.rows()[2].body, RowBody::ModFeed(f) if f.feed_kind == "ban"));
        assert!(s.source(&sk).unwrap().loaded && s.source(&sk).unwrap().esub.active);
    }

    #[test]
    fn trim_keeps_the_cap_and_held_allows_more() {
        let (mut s, sk) = store();
        for i in 0..1100 {
            s.apply(&sk, &chat(&i.to_string(), i, "a"));
        }
        assert_eq!(s.rows().len(), ROW_CAP);
        s.set_held(true);
        for i in 1100..1300 {
            s.apply(&sk, &chat(&i.to_string(), i, "a"));
        }
        assert_eq!(s.rows().len(), 1200);
        s.set_held(false);
        assert_eq!(s.rows().len(), ROW_CAP);
    }

    #[test]
    fn conn_frame_is_kept_and_channel_id_learned() {
        let (mut s, sk) = store();
        s.apply(&sk, &json!({"type": "conn", "state": "open", "channelId": 156200816, "canSend": false}));
        let st = s.source(&sk).unwrap();
        assert_eq!(st.conn.as_ref().unwrap().state, "open");
        assert_eq!(st.channel_id.as_deref(), Some("156200816"));
    }
}
