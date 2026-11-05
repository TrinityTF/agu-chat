//! IRCv3 line parsing and the chat message model.
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Line {
    pub tags: HashMap<String, String>,
    pub prefix: String,
    pub cmd: String,
    /// Middle params followed by the trailing param (if any).
    pub params: Vec<String>,
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some(':') => out.push(';'),
            Some('\\') => out.push('\\'),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

pub fn parse(line: &str) -> Option<Line> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    if rest.is_empty() {
        return None;
    }
    let mut tags = HashMap::new();
    if let Some(r) = rest.strip_prefix('@') {
        let (t, r2) = r.split_once(' ')?;
        for kv in t.split(';') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            tags.insert(k.to_string(), unescape(v));
        }
        rest = r2.trim_start();
    }
    let mut prefix = String::new();
    if let Some(r) = rest.strip_prefix(':') {
        let (p, r2) = r.split_once(' ')?;
        prefix = p.to_string();
        rest = r2.trim_start();
    }
    let (cmd, mut rest) = match rest.split_once(' ') {
        Some((c, r)) => (c, r),
        None => (rest, ""),
    };
    let mut params = Vec::new();
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() {
            break;
        }
        if let Some(t) = rest.strip_prefix(':') {
            params.push(t.to_string());
            break;
        }
        match rest.split_once(' ') {
            Some((p, r)) => {
                params.push(p.to_string());
                rest = r;
            }
            None => {
                params.push(rest.to_string());
                break;
            }
        }
    }
    Some(Line { tags, prefix, cmd: cmd.to_string(), params })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Chat,
    /// USERNOTICE system-msg row
    System,
    /// NOTICE / CLEARCHAT info row
    Notice,
}

#[derive(Debug, Clone)]
pub struct TwEmote {
    pub id: String,
    /// Unicode code-point indices, inclusive.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct ChatMsg {
    pub id: String,
    pub login: String,
    pub display: String,
    pub color: Option<[u8; 3]>,
    pub ts_ms: i64,
    pub badges: Vec<(String, String)>,
    pub text: String,
    pub emotes: Vec<TwEmote>,
    pub action: bool,
    pub kind: Kind,
    /// Left accent bar (USERNOTICE related rows)
    pub bar: bool,
    pub deleted: bool,
    pub historical: bool,
}

impl ChatMsg {
    pub fn notice(text: String, kind: Kind) -> Self {
        ChatMsg {
            id: String::new(),
            login: String::new(),
            display: String::new(),
            color: None,
            ts_ms: now_ms(),
            badges: vec![],
            text,
            emotes: vec![],
            action: false,
            kind,
            bar: kind == Kind::System,
            deleted: false,
            historical: false,
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug)]
pub enum Inbound {
    Msgs(String, Vec<ChatMsg>),
    ClearUser { ch: String, login: String, secs: Option<u64> },
    ClearAll(String),
    ClearMsg { ch: String, id: String },
    Notice(String, String),
    RoomState { ch: String, room_id: String },
    Ping(String),
    Reconnect,
    Other,
}

fn channel_of(l: &Line) -> String {
    l.params.first().map(|c| c.trim_start_matches('#').to_lowercase()).unwrap_or_default()
}

fn parse_badges(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter(|b| !b.is_empty())
        .filter_map(|b| b.split_once('/').map(|(a, v)| (a.to_string(), v.to_string())))
        .collect()
}

fn parse_emotes(s: &str) -> Vec<TwEmote> {
    let mut out = Vec::new();
    for part in s.split('/') {
        let Some((id, ranges)) = part.split_once(':') else { continue };
        for r in ranges.split(',') {
            if let Some((a, b)) = r.split_once('-') {
                if let (Ok(a), Ok(b)) = (a.parse::<usize>(), b.parse::<usize>()) {
                    out.push(TwEmote { id: id.to_string(), start: a, end: b });
                }
            }
        }
    }
    out.sort_by_key(|e| e.start);
    out
}

fn build_chat(l: &Line, text_idx: usize) -> ChatMsg {
    let t = &l.tags;
    let login = l.prefix.split('!').next().unwrap_or("").to_string();
    let mut text = l.params.get(text_idx).cloned().unwrap_or_default();
    let mut action = false;
    if let Some(inner) = text.strip_prefix("\u{1}ACTION ") {
        action = true;
        text = inner.trim_end_matches('\u{1}').to_string();
    }
    let color = t.get("color").and_then(|c| {
        let c = c.trim_start_matches('#');
        if c.len() == 6 {
            let v = u32::from_str_radix(c, 16).ok()?;
            Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
        } else {
            None
        }
    });
    let login = if login.is_empty() { t.get("login").cloned().unwrap_or_default() } else { login };
    let display = t
        .get("display-name")
        .filter(|d| !d.is_empty())
        .cloned()
        .unwrap_or_else(|| login.clone());
    ChatMsg {
        id: t.get("id").cloned().unwrap_or_default(),
        login,
        display,
        color,
        ts_ms: t.get("tmi-sent-ts").and_then(|v| v.parse().ok()).unwrap_or_else(now_ms),
        badges: t.get("badges").map(|b| parse_badges(b)).unwrap_or_default(),
        text,
        emotes: t.get("emotes").map(|e| parse_emotes(e)).unwrap_or_default(),
        action,
        kind: Kind::Chat,
        bar: false,
        deleted: false,
        historical: false,
    }
}

pub fn interpret(line: &str, historical: bool) -> Inbound {
    let Some(l) = parse(line) else { return Inbound::Other };
    match l.cmd.as_str() {
        "PING" => Inbound::Ping(l.params.first().cloned().unwrap_or_else(|| "tmi.twitch.tv".into())),
        "RECONNECT" => Inbound::Reconnect,
        "PRIVMSG" => {
            let mut m = build_chat(&l, 1);
            m.historical = historical;
            Inbound::Msgs(channel_of(&l), vec![m])
        }
        "USERNOTICE" => {
            let ch = channel_of(&l);
            let mut v = Vec::new();
            let sys = l.tags.get("system-msg").cloned().unwrap_or_default();
            if !sys.is_empty() {
                let mut s = ChatMsg::notice(sys, Kind::System);
                s.ts_ms = l.tags.get("tmi-sent-ts").and_then(|v| v.parse().ok()).unwrap_or_else(now_ms);
                s.historical = historical;
                v.push(s);
            }
            if l.params.len() > 1 && !l.params[1].is_empty() {
                let mut m = build_chat(&l, 1);
                m.bar = true;
                m.historical = historical;
                v.push(m);
            }
            Inbound::Msgs(ch, v)
        }
        "NOTICE" => Inbound::Notice(channel_of(&l), l.params.get(1).cloned().unwrap_or_default()),
        "CLEARCHAT" => {
            let ch = channel_of(&l);
            match l.params.get(1) {
                Some(login) => Inbound::ClearUser {
                    ch,
                    login: login.clone(),
                    secs: l.tags.get("ban-duration").and_then(|d| d.parse().ok()),
                },
                None => Inbound::ClearAll(ch),
            }
        }
        "CLEARMSG" => Inbound::ClearMsg {
            ch: channel_of(&l),
            id: l.tags.get("target-msg-id").cloned().unwrap_or_default(),
        },
        "ROOMSTATE" => match l.tags.get("room-id") {
            Some(id) => Inbound::RoomState { ch: channel_of(&l), room_id: id.clone() },
            None => Inbound::Other,
        },
        _ => Inbound::Other,
    }
}
