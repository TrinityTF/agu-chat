//! Networking on a background tokio runtime: the shared IRC socket, backlog, emote and badge lists.
use std::time::Duration;

use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;

use crate::irc::{self, ChatMsg, Inbound};
use crate::logf;

pub const UA: &str = "AguChat-native-prototype/0.1";

#[derive(Clone, Debug)]
pub struct EmoteDef {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    SevenTv,
    Bttv,
    Ffz,
}

impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Provider::SevenTv => "7TV",
            Provider::Bttv => "BTTV",
            Provider::Ffz => "FFZ",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BadgeDef {
    pub set: String,
    pub version: String,
    pub url: String,
    pub title: String,
}

pub enum Event {
    Connected(bool),
    Msgs { ch: String, msgs: Vec<ChatMsg> },
    Backlog { ch: String, msgs: Vec<ChatMsg> },
    ClearUser { ch: String, login: String, secs: Option<u64> },
    ClearAll(String),
    ClearMsg { ch: String, id: String },
    Notice { ch: String, text: String },
    RoomState { ch: String, room_id: String },
    /// ch None = global list
    Emotes { ch: Option<String>, provider: Provider, list: Vec<EmoteDef> },
    Badges { ch: Option<String>, list: Vec<BadgeDef> },
}

#[derive(Clone)]
pub struct Net {
    pub client: reqwest::Client,
    pub tx: Sender<Event>,
    pub ctx: egui::Context,
}

impl Net {
    fn send(&self, e: Event) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }

    async fn json(&self, url: &str) -> Option<Value> {
        match self.client.get(url).send().await.and_then(|r| r.error_for_status()) {
            Ok(r) => r.json::<Value>().await.ok(),
            Err(e) => {
                logf!("GET {url} failed: {e}");
                None
            }
        }
    }

    pub async fn load_globals(&self) {
        let (a, b, c, d) = tokio::join!(
            self.json("https://7tv.io/v3/emote-sets/global"),
            self.json("https://api.betterttv.net/3/cached/emotes/global"),
            self.json("https://api.betterttv.net/3/cached/frankerfacez/emotes/global"),
            self.json("https://api.ivr.fi/v2/twitch/badges/global"),
        );
        if let Some(v) = a {
            self.send(Event::Emotes { ch: None, provider: Provider::SevenTv, list: seventv(v.get("emotes")) });
        }
        if let Some(v) = b {
            self.send(Event::Emotes { ch: None, provider: Provider::Bttv, list: bttv(&v) });
        }
        if let Some(v) = c {
            self.send(Event::Emotes { ch: None, provider: Provider::Ffz, list: ffz(&v) });
        }
        if let Some(v) = d {
            self.send(Event::Badges { ch: None, list: badges(&v) });
        }
        logf!("globals loaded");
    }

    pub async fn load_channel_badges(&self, ch: &str) {
        let url = format!("https://api.ivr.fi/v2/twitch/badges/channel?login={ch}");
        if let Some(v) = self.json(&url).await {
            self.send(Event::Badges { ch: Some(ch.to_string()), list: badges(&v) });
        }
    }

    pub async fn load_channel_emotes(&self, ch: &str, room_id: &str) {
        let u1 = format!("https://7tv.io/v3/users/twitch/{room_id}");
        let u2 = format!("https://api.betterttv.net/3/cached/users/twitch/{room_id}");
        let u3 = format!("https://api.betterttv.net/3/cached/frankerfacez/users/twitch/{room_id}");
        let (a, b, c) = tokio::join!(self.json(&u1), self.json(&u2), self.json(&u3));
        let ch = Some(ch.to_string());
        if let Some(v) = a {
            let list = seventv(v.get("emote_set").and_then(|s| s.get("emotes")));
            self.send(Event::Emotes { ch: ch.clone(), provider: Provider::SevenTv, list });
        }
        if let Some(v) = b {
            let mut list = bttv(v.get("channelEmotes").unwrap_or(&Value::Null));
            list.extend(bttv(v.get("sharedEmotes").unwrap_or(&Value::Null)));
            self.send(Event::Emotes { ch: ch.clone(), provider: Provider::Bttv, list });
        }
        if let Some(v) = c {
            self.send(Event::Emotes { ch, provider: Provider::Ffz, list: ffz(&v) });
        }
    }

    pub async fn load_backlog(&self, ch: &str) {
        let url = format!("https://recent-messages.robotty.de/api/v2/recent-messages/{ch}?limit=200");
        let Some(v) = self.json(&url).await else { return };
        let mut out = Vec::new();
        if let Some(lines) = v.get("messages").and_then(|m| m.as_array()) {
            for l in lines.iter().filter_map(|l| l.as_str()) {
                match irc::interpret(l, true) {
                    Inbound::Msgs(_, m) => out.extend(m),
                    Inbound::RoomState { room_id, .. } => {
                        self.send(Event::RoomState { ch: ch.to_string(), room_id });
                    }
                    _ => {}
                }
            }
        }
        logf!("backlog {ch}: {} messages", out.len());
        self.send(Event::Backlog { ch: ch.to_string(), msgs: out });
    }
}

fn str_of<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn seventv(v: Option<&Value>) -> Vec<EmoteDef> {
    let mut out = Vec::new();
    for e in v.and_then(|v| v.as_array()).into_iter().flatten() {
        let name = str_of(e, "name");
        let host = e.get("data").and_then(|d| d.get("host")).map(|h| str_of(h, "url")).unwrap_or("");
        if name.is_empty() || host.is_empty() {
            continue;
        }
        let url = if host.starts_with("//") { format!("https:{host}/1x.webp") } else { format!("{host}/1x.webp") };
        out.push(EmoteDef { name: name.to_string(), url });
    }
    out
}

fn bttv(v: &Value) -> Vec<EmoteDef> {
    let mut out = Vec::new();
    for e in v.as_array().into_iter().flatten() {
        let (id, code) = (str_of(e, "id"), str_of(e, "code"));
        if id.is_empty() || code.is_empty() {
            continue;
        }
        out.push(EmoteDef { name: code.to_string(), url: format!("https://cdn.betterttv.net/emote/{id}/1x.webp") });
    }
    out
}

fn ffz(v: &Value) -> Vec<EmoteDef> {
    let mut out = Vec::new();
    for e in v.as_array().into_iter().flatten() {
        let code = str_of(e, "code");
        let url = e.get("images").map(|i| str_of(i, "1x")).unwrap_or("");
        if code.is_empty() || url.is_empty() {
            continue;
        }
        out.push(EmoteDef { name: code.to_string(), url: url.to_string() });
    }
    out
}

fn badges(v: &Value) -> Vec<BadgeDef> {
    let mut out = Vec::new();
    for s in v.as_array().into_iter().flatten() {
        let set = str_of(s, "set_id");
        for ver in s.get("versions").and_then(|x| x.as_array()).into_iter().flatten() {
            let url = str_of(ver, "image_url_1x");
            if url.is_empty() {
                continue;
            }
            out.push(BadgeDef {
                set: set.to_string(),
                version: str_of(ver, "id").to_string(),
                url: url.to_string(),
                title: str_of(ver, "title").to_string(),
            });
        }
    }
    out
}

/// One shared websocket for all channels, reconnecting with backoff.
pub async fn irc_task(net: Net, url: String, channels: Vec<String>, bench: bool) {
    let mut backoff = 1u64;
    let nick = format!("justinfan{:05}", rand::random::<u32>() % 100000);
    let mut first = true;
    loop {
        logf!("connecting to {url}");
        match tokio::time::timeout(Duration::from_secs(20), tokio_tungstenite::connect_async(url.as_str())).await {
            Ok(Ok((mut ws, _))) => {
                logf!("websocket connected");
                let login = [
                    "PASS SCHMOOPIIE".to_string(),
                    format!("NICK {nick}"),
                    "CAP REQ :twitch.tv/tags twitch.tv/commands".to_string(),
                ];
                let mut ok = true;
                for l in login.iter() {
                    if ws.send(Message::text(l.clone())).await.is_err() {
                        ok = false;
                    }
                }
                for c in &channels {
                    if ws.send(Message::text(format!("JOIN #{c}"))).await.is_err() {
                        ok = false;
                    }
                }
                if ok {
                    net.send(Event::Connected(true));
                    logf!("connected and joined: {}", channels.join(","));
                    if first && !bench {
                        for c in &channels {
                            let n = net.clone();
                            let c = c.clone();
                            tokio::spawn(async move { n.load_backlog(&c).await });
                        }
                    }
                    first = false;
                    backoff = 1;
                    let mut n_msgs: u64 = 0;
                    loop {
                        let next = tokio::time::timeout(Duration::from_secs(600), ws.next()).await;
                        let msg = match next {
                            Ok(Some(Ok(m))) => m,
                            Ok(Some(Err(e))) => {
                                logf!("ws error: {e}");
                                break;
                            }
                            Ok(None) => {
                                logf!("ws closed");
                                break;
                            }
                            Err(_) => {
                                logf!("ws idle timeout");
                                break;
                            }
                        };
                        let text = match msg {
                            Message::Text(t) => t.to_string(),
                            Message::Ping(p) => {
                                let _ = ws.send(Message::Pong(p)).await;
                                continue;
                            }
                            Message::Close(_) => break,
                            _ => continue,
                        };
                        let mut reconnect = false;
                        for line in text.split("\r\n").filter(|l| !l.is_empty()) {
                            match irc::interpret(line, false) {
                                Inbound::Ping(p) => {
                                    let _ = ws.send(Message::text(format!("PONG :{p}"))).await;
                                }
                                Inbound::Reconnect => reconnect = true,
                                Inbound::Msgs(ch, msgs) => {
                                    n_msgs += msgs.len() as u64;
                                    net.send(Event::Msgs { ch, msgs });
                                }
                                Inbound::ClearUser { ch, login, secs } => net.send(Event::ClearUser { ch, login, secs }),
                                Inbound::ClearAll(ch) => net.send(Event::ClearAll(ch)),
                                Inbound::ClearMsg { ch, id } => net.send(Event::ClearMsg { ch, id }),
                                Inbound::Notice(ch, text) => net.send(Event::Notice { ch, text }),
                                Inbound::RoomState { ch, room_id } => net.send(Event::RoomState { ch, room_id }),
                                Inbound::Other => {}
                            }
                        }
                        if n_msgs > 0 && n_msgs % 500 == 0 {
                            logf!("received {n_msgs} chat messages");
                        }
                        if reconnect {
                            break;
                        }
                    }
                }
            }
            Ok(Err(e)) => logf!("connect failed: {e}"),
            Err(_) => logf!("connect timed out"),
        }
        net.send(Event::Connected(false));
        tokio::time::sleep(Duration::from_secs(backoff)).await;
        backoff = (backoff * 2).min(30);
    }
}
