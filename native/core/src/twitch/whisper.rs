//! Twitch whispers. They fit the source model as two reserved Twitch channels (a real login can never contain
//! a colon, so they cannot clash with a channel):
//!
//!   `whisper:<login>`  one conversation: the same `chatlog` / `chat` frames a channel produces, so rows render
//!                      like chat. No IRC join, no mod features; the user's own channel id rides along so the
//!                      emote sets (7TV, BTTV, FFZ) load.
//!   `whisper:*`        the inbox: it only says "someone whispered you" (`{type:"whisper"}`) so the UI can open a
//!                      tab for a conversation that has none yet.
//!
//! Receiving is EventSub `user.whisper.message` on the app's single session (eventsub.rs). Twitch has no
//! whisper history API, so each conversation is kept in `<data dir>/whispers/<login>.json` (last 200 messages),
//! written whether or not a tab is open. Sending is Helix `POST /whispers`; Twitch does not echo our own whispers
//! back, so a sent one is recorded locally.
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{json, Value};

use super::auth;
use super::helix::{helix, helix_get, login_required, user_by_login};
use super::http::HttpError;
use super::rt::{ctx, log};
use super::shared::esub;
use crate::platforms::{Emit, Source};
use crate::protocol::Frame;
use crate::q;
use crate::util::{js_string, now_ms, uuid_v4};

pub const SCOPE_READ: &str = "user:read:whispers";
pub const SCOPE_MANAGE: &str = "user:manage:whispers";
pub const EVENT_TYPE: &str = "user.whisper.message";
pub const RELOGIN: &str = "Log in to Twitch again to use whispers.";

const PREFIX: &str = "whisper:";
const INBOX: &str = "whisper:*";
const MAX_HISTORY: usize = 200;
/// Twitch's limit once the two have whispered before; to a new recipient it is 500 (the send box caps there).
const MAX_TEXT: usize = 10_000;
/// How long a message from us that arrives over EventSub still counts as the echo of one we sent.
const ECHO_MS: i64 = 30_000;

#[derive(Debug, PartialEq)]
pub enum Channel {
    Inbox,
    User(String),
}

/// None for an ordinary channel.
pub fn parse(channel: &str) -> Option<Channel> {
    let rest = channel.strip_prefix(PREFIX)?;
    Some(if rest == "*" { Channel::Inbox } else { Channel::User(rest.to_string()) })
}

pub fn channel_for(login: &str) -> String {
    format!("{PREFIX}{login}")
}

/// The other person of a conversation channel.
pub fn login_of(channel: &str) -> Option<String> {
    match parse(channel)? {
        Channel::User(l) => Some(l),
        Channel::Inbox => None,
    }
}

pub fn inbox_channel() -> &'static str {
    INBOX
}

/// A typed "whisper:Foo" -> "whisper:foo". Errs with the message for the user.
pub fn normalize(input: &str) -> Result<String, String> {
    let rest = input.trim().strip_prefix(PREFIX).unwrap_or(input);
    if rest == "*" {
        return Ok(INBOX.to_string());
    }
    super::channel::normalize_channel(rest).map(|l| channel_for(&l)).map_err(|e| e.replace("channel name", "username"))
}

/// Both whisper scopes: a login from before whispers existed has neither.
pub fn has_scopes(scopes: &[String]) -> bool {
    [SCOPE_READ, SCOPE_MANAGE].iter().all(|s| scopes.iter().any(|x| x == s))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub id: String,
    pub login: String,
    pub name: String,
}

pub struct Incoming {
    pub other: Person,
    /// we are the sender (another client of ours, or an echo)
    pub mine: bool,
    pub msg: Value,
}

fn person(event: &Value, prefix: &str) -> Person {
    let get = |k: &str| event[format!("{prefix}_user_{k}")].as_str().unwrap_or("").to_string();
    let login = get("login");
    Person { id: get("id"), name: Some(get("name")).filter(|n| !n.is_empty()).unwrap_or_else(|| login.clone()), login }
}

fn chat_msg(id: &str, ts: i64, from: &Person, text: &str, emotes: &str) -> Value {
    json!({
        "platform": "twitch",
        "id": id,
        "ts": ts,
        "user": {"id": from.id, "login": from.login, "name": from.name, "color": ""},
        "badges": "",
        "text": text,
        "emotes": emotes,
        "gifs": "",
        "action": false,
        "firstMsg": false,
        "reply": null,
        "rewardId": null,
        "msgId": null,
    })
}

/// A `user.whisper.message` event as a chat message in the conversation with the other person.
pub fn parse_event(event: &Value, me_id: &str) -> Option<Incoming> {
    let (from, to) = (person(event, "from"), person(event, "to"));
    let text = event["whisper"]["text"].as_str().filter(|t| !t.is_empty())?;
    if from.login.is_empty() || to.login.is_empty() {
        return None;
    }
    let id = Some(js_string(&event["whisper_id"])).filter(|i| !i.is_empty()).unwrap_or_else(uuid_v4);
    let mine = from.id == me_id;
    let msg = chat_msg(&id, now_ms(), &from, text, "");
    Some(Incoming { other: if mine { to } else { from }, mine, msg })
}

/// Twitch's `emotes` tag (`25:0-4,6-10/1902:12-16`) for the whole-word matches of `known` (name -> id). A whisper
/// carries no emote positions of its own.
pub fn emotes_tag(text: &str, known: &HashMap<String, String>) -> String {
    let mut found: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    let (mut pos, mut start, mut word) = (0usize, 0usize, String::new());
    let mut flush = |word: &mut String, start: usize, end: usize| {
        if let Some(id) = known.get(word.as_str()) {
            match found.iter_mut().find(|(i, _)| i == id) {
                Some((_, spans)) => spans.push((start, end)),
                None => found.push((id.clone(), vec![(start, end)])),
            }
        }
        word.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            if !word.is_empty() {
                flush(&mut word, start, pos - 1);
            }
        } else {
            if word.is_empty() {
                start = pos;
            }
            word.push(c);
        }
        pos += 1;
    }
    if !word.is_empty() {
        flush(&mut word, start, pos - 1);
    }
    found.iter().map(|(id, spans)| format!("{id}:{}", spans.iter().map(|(a, b)| format!("{a}-{b}")).collect::<Vec<_>>().join(","))).collect::<Vec<_>>().join("/")
}

static GLOBAL_EMOTES: Mutex<Option<Arc<HashMap<String, String>>>> = Mutex::new(None);

/// Twitch's global emotes (name -> id), fetched once; empty while Twitch cannot be asked.
async fn global_emotes() -> Arc<HashMap<String, String>> {
    if let Some(m) = GLOBAL_EMOTES.lock().unwrap().clone() {
        return m;
    }
    let Ok(page) = helix_get("/chat/emotes/global", Vec::new()).await else { return Arc::new(HashMap::new()) };
    let map: HashMap<String, String> = page["data"].as_array().into_iter().flatten().filter_map(|e| Some((e["name"].as_str()?.to_string(), js_string(&e["id"])))).collect();
    let map = Arc::new(map);
    *GLOBAL_EMOTES.lock().unwrap() = Some(map.clone());
    map
}

// ---------- history on disk ----------

fn history_path(dir: &Path, login: &str) -> PathBuf {
    dir.join("whispers").join(format!("{login}.json"))
}

static FILES: Mutex<()> = Mutex::new(());

fn load_history(dir: &Path, login: &str) -> Vec<Value> {
    let _g = FILES.lock().unwrap();
    std::fs::read_to_string(history_path(dir, login)).ok().and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok()).unwrap_or_default()
}

/// Keeps the newest `max`.
pub fn trim(list: &mut Vec<Value>, max: usize) {
    if list.len() > max {
        let cut = list.len() - max;
        list.drain(..cut);
    }
}

/// Adds `msg` unless that id is already there (Twitch may redeliver). The file is rewritten whole: it is small.
fn append_history(dir: &Path, login: &str, msg: &Value) -> std::io::Result<()> {
    let mut list = load_history(dir, login);
    if list.iter().any(|m| m["id"] == msg["id"]) {
        return Ok(());
    }
    list.push(msg.clone());
    trim(&mut list, MAX_HISTORY);
    let _g = FILES.lock().unwrap();
    let path = history_path(dir, login);
    std::fs::create_dir_all(path.parent().unwrap_or(dir))?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string(&list).map_err(std::io::Error::other)?)?;
    std::fs::rename(&tmp, &path)
}

/// Whether `msg` (from us) is the EventSub copy of a whisper we already recorded when sending it.
fn is_echo(history: &[Value], msg: &Value) -> bool {
    history.iter().any(|m| m["user"]["id"] == msg["user"]["id"] && m["text"] == msg["text"] && (m["ts"].as_i64().unwrap_or(0) - msg["ts"].as_i64().unwrap_or(0)).abs() < ECHO_MS)
}

// ---------- live conversations ----------

static LIVE: LazyLock<Mutex<HashMap<String, Arc<Conversation>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static INBOX_EMIT: Mutex<Option<Emit>> = Mutex::new(None);

/// EventSub carries whispers whenever something is listening for them: the inbox or an open conversation.
fn refresh_interest() {
    let wanted = INBOX_EMIT.lock().unwrap().is_some() || !LIVE.lock().unwrap().is_empty();
    esub().set_whispers(wanted);
}

fn record(login: &str, msg: &Value, from: Option<&Person>) {
    let Some(c) = ctx() else { return };
    if let Err(e) = append_history(&c.data_dir, login, msg) {
        log(&format!("could not save the whisper history with {login}: {e}"));
    }
    if let Some(conv) = LIVE.lock().unwrap().get(login).cloned() {
        (conv.emit)(json!({"type": "chat", "msg": msg}));
    }
    if let Some(from) = from {
        if let Some(emit) = INBOX_EMIT.lock().unwrap().clone() {
            emit(json!({"type": "whisper", "login": from.login, "name": from.name}));
        }
    }
}

/// An EventSub `user.whisper.message` event.
pub fn on_event(event: &Value) {
    let Some(rec) = auth::current() else { return };
    let Some(mut inc) = parse_event(event, &rec.user.id) else { return };
    tokio::spawn(async move {
        let known = global_emotes().await;
        let text = inc.msg["text"].as_str().unwrap_or("").to_string();
        inc.msg["emotes"] = json!(emotes_tag(&text, &known));
        if inc.mine && ctx().map(|c| is_echo(&load_history(&c.data_dir, &inc.other.login), &inc.msg)).unwrap_or(false) {
            return;
        }
        record(&inc.other.login, &inc.msg, (!inc.mine).then_some(&inc.other));
    });
}

// ---------- sending ----------

/// Twitch's refusal as a sentence for the user. `to` is the recipient's name.
pub fn explain(status: u16, message: &str, to: &str) -> String {
    let m = message.to_lowercase();
    if m.contains("phone") {
        "Twitch only lets accounts with a verified phone number send whispers. Add one in your Twitch security settings.".into()
    } else if status == 429 {
        "You are whispering too fast, or to too many new people. Wait a moment and try again.".into()
    } else if m.contains("too long") {
        "That whisper is too long (500 characters to someone new, 10,000 otherwise).".into()
    } else if m.contains("settings") || m.contains("not accept") {
        format!("{to} does not accept whispers from you.")
    } else if m.contains("block") {
        format!("Whispers between you and {to} are blocked.")
    } else if status == 401 || m.contains("scope") {
        RELOGIN.into()
    } else if message.is_empty() {
        format!("Twitch refused the whisper (HTTP {status}).")
    } else {
        format!("Twitch refused the whisper: {message}")
    }
}

fn refused(e: HttpError, to: &str) -> HttpError {
    if e.status == 0 || e.local {
        return e; // never reached Twitch, or already ours
    }
    HttpError::local(if e.status == 429 { 429 } else { 400 }, explain(e.status, &e.message, to))
}

/// Whispers `login` as the logged-in user and records it in the conversation. Errors are ours, ready to show.
pub async fn send(login: &str, text: &str) -> Result<(), HttpError> {
    let Some(rec) = auth::current() else { return Err(login_required()) };
    if !has_scopes(&rec.scopes) {
        return Err(HttpError::local(403, RELOGIN));
    }
    if text.is_empty() {
        return Err(HttpError::local(400, "empty message"));
    }
    if text.chars().count() > MAX_TEXT {
        return Err(HttpError::local(400, explain(400, "too long", login)));
    }
    let Some(to) = user_by_login(login).await? else { return Err(HttpError::local(404, format!("There is no Twitch user called {login}."))) };
    let to = Person { id: js_string(&to["id"]), login: login.to_string(), name: js_string(&to["name"]) };
    helix("/whispers", "POST", q! {"from_user_id" => rec.user.id.clone(), "to_user_id" => to.id.clone()}, Some(json!({"message": text}))).await.map_err(|e| refused(e, &to.name))?;
    let me = Person { id: rec.user.id.clone(), login: rec.user.login.clone(), name: Some(rec.user.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| rec.user.login.clone()) };
    let emotes = emotes_tag(text, &*global_emotes().await);
    record(login, &chat_msg(&uuid_v4(), now_ms(), &me, text, &emotes), None);
    Ok(())
}

// ---------- sources ----------

fn sendability() -> Value {
    match auth::current() {
        None => json!({"canSend": false, "readOnlyReason": auth::state().error.unwrap_or_else(|| "Log in to Twitch in Settings to send whispers.".into())}),
        Some(rec) if !has_scopes(&rec.scopes) => json!({"canSend": false, "readOnlyReason": RELOGIN}),
        Some(_) => json!({"canSend": true}),
    }
}

pub struct Conversation {
    login: String,
    emit: Emit,
    auth_listener: Mutex<Option<u64>>,
}

impl Conversation {
    pub fn new(login: String, emit: Emit) -> Arc<dyn Source> {
        Arc::new(Conversation { login, emit, auth_listener: Mutex::new(None) })
    }

    fn conn(&self) -> Frame {
        let mut f = json!({"type": "conn", "state": "open", "detail": ""});
        if let (Some(o), Some(extra)) = (f.as_object_mut(), sendability().as_object()) {
            o.extend(extra.clone());
        }
        if let Some(rec) = auth::current() {
            f["channelId"] = json!(rec.user.id);
        }
        f
    }
}

impl Source for Conversation {
    fn start(self: Arc<Self>) {
        LIVE.lock().unwrap().insert(self.login.clone(), self.clone());
        let me = Arc::downgrade(&self);
        let id = auth::EVENTS.on(Arc::new(move |_| {
            if let Some(c) = me.upgrade() {
                (c.emit)(c.conn()); // canSend follows the login
            }
        }));
        *self.auth_listener.lock().unwrap() = Some(id);
        (self.emit)(self.conn());
        refresh_interest();
    }

    fn stop(&self) {
        {
            let mut live = LIVE.lock().unwrap();
            if live.get(&self.login).map(|c| std::ptr::eq(Arc::as_ptr(c), self)).unwrap_or(false) {
                live.remove(&self.login);
            }
        }
        if let Some(id) = self.auth_listener.lock().unwrap().take() {
            auth::EVENTS.off(id);
        }
        refresh_interest();
    }

    fn snapshot(&self) -> Vec<Frame> {
        let messages = ctx().map(|c| load_history(&c.data_dir, &self.login)).unwrap_or_default();
        let conn = self.conn();
        let chatlog = json!({"type": "chatlog", "channelId": conn["channelId"], "messages": messages, "pinned": null, "modfeed": [], "automod": [], "suspicious": [], "esub": {"active": false, "reason": null}});
        vec![conn, chatlog]
    }
}

pub struct Inbox {
    emit: Emit,
}

impl Inbox {
    pub fn new(emit: Emit) -> Arc<dyn Source> {
        Arc::new(Inbox { emit })
    }
}

impl Source for Inbox {
    fn start(self: Arc<Self>) {
        *INBOX_EMIT.lock().unwrap() = Some(self.emit.clone());
        refresh_interest();
    }

    fn stop(&self) {
        *INBOX_EMIT.lock().unwrap() = None;
        refresh_interest();
    }

    fn snapshot(&self) -> Vec<Frame> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_forms() {
        assert_eq!(parse("whisper:foo"), Some(Channel::User("foo".into())));
        assert_eq!(parse("whisper:*"), Some(Channel::Inbox));
        assert_eq!(parse("foo"), None);
        assert_eq!(login_of("whisper:foo").as_deref(), Some("foo"));
        assert_eq!(login_of("whisper:*"), None);
        assert_eq!(channel_for("foo"), "whisper:foo");
        assert_eq!(normalize("whisper:Foo").unwrap(), "whisper:foo");
        assert_eq!(normalize("whisper:@Foo").unwrap(), "whisper:foo");
        assert_eq!(normalize("whisper:*").unwrap(), INBOX);
        assert!(normalize("whisper:").is_err());
        assert!(normalize("whisper:has space").is_err());
        assert!(normalize("whisper:../x").is_err());
    }

    fn event(from: (&str, &str), to: (&str, &str), text: &str) -> Value {
        json!({
            "from_user_id": from.0, "from_user_login": from.1, "from_user_name": from.1.to_uppercase(),
            "to_user_id": to.0, "to_user_login": to.1, "to_user_name": to.1.to_uppercase(),
            "whisper_id": "w1", "whisper": {"text": text},
        })
    }

    #[test]
    fn a_received_whisper_belongs_to_the_senders_conversation() {
        let inc = parse_event(&event(("2", "bob"), ("1", "me"), "hi there"), "1").unwrap();
        assert!(!inc.mine);
        assert_eq!(inc.other, Person { id: "2".into(), login: "bob".into(), name: "BOB".into() });
        assert_eq!((inc.msg["id"].as_str(), inc.msg["text"].as_str(), inc.msg["user"]["login"].as_str()), (Some("w1"), Some("hi there"), Some("bob")));
    }

    #[test]
    fn a_whisper_we_sent_elsewhere_belongs_to_the_recipients_conversation() {
        let inc = parse_event(&event(("1", "me"), ("2", "bob"), "yo"), "1").unwrap();
        assert!(inc.mine);
        assert_eq!(inc.other.login, "bob");
        assert_eq!(inc.msg["user"]["login"], "me");
    }

    #[test]
    fn a_malformed_event_is_ignored() {
        assert!(parse_event(&json!({}), "1").is_none());
        assert!(parse_event(&event(("2", "bob"), ("1", "me"), ""), "1").is_none());
    }

    #[test]
    fn emote_positions_are_per_word_and_per_id() {
        let known: HashMap<String, String> = [("Kappa".to_string(), "25".to_string()), ("LUL".to_string(), "425618".to_string())].into();
        assert_eq!(emotes_tag("Kappa hi Kappa LUL", &known), "25:0-4,9-13/425618:15-17");
        assert_eq!(emotes_tag("Kappas LUL!", &known), "");
        assert_eq!(emotes_tag("héllo Kappa", &known), "25:6-10"); // positions count characters
        assert_eq!(emotes_tag("", &known), "");
    }

    #[test]
    fn history_keeps_the_newest_200() {
        let mut list: Vec<Value> = (0..250).map(|i| json!({"id": i})).collect();
        trim(&mut list, MAX_HISTORY);
        assert_eq!((list.len(), list[0]["id"].as_i64(), list[199]["id"].as_i64()), (200, Some(50), Some(249)));
        trim(&mut list, MAX_HISTORY);
        assert_eq!(list.len(), 200);
    }

    #[test]
    fn history_file_round_trip_dedupes_and_trims() {
        let dir = std::env::temp_dir().join(format!("agu-whispers-{}", uuid_v4()));
        assert!(load_history(&dir, "bob").is_empty());
        let from = Person { id: "2".into(), login: "bob".into(), name: "Bob".into() };
        for i in 0..205 {
            append_history(&dir, "bob", &chat_msg(&format!("m{i}"), i, &from, "x", "")).unwrap();
        }
        append_history(&dir, "bob", &chat_msg("m204", 204, &from, "x", "")).unwrap(); // redelivered
        let list = load_history(&dir, "bob");
        assert_eq!((list.len(), list[0]["id"].as_str(), list[199]["id"].as_str()), (200, Some("m5"), Some("m204")));
        assert!(load_history(&dir, "alice").is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn our_own_whisper_coming_back_is_an_echo_only_when_recent_and_equal() {
        let me = Person { id: "1".into(), login: "me".into(), name: "Me".into() };
        let sent = vec![chat_msg("a", 1_000_000, &me, "hello", "")];
        assert!(is_echo(&sent, &chat_msg("b", 1_005_000, &me, "hello", "")));
        assert!(!is_echo(&sent, &chat_msg("b", 1_005_000, &me, "other", "")));
        assert!(!is_echo(&sent, &chat_msg("b", 1_000_000 + ECHO_MS + 1, &me, "hello", "")));
    }

    #[tokio::test]
    async fn a_whisper_reaches_the_inbox_the_history_and_an_open_conversation() {
        let dir = std::env::temp_dir().join(format!("agu-whispers-{}", uuid_v4()));
        let log = Arc::new(crate::log::Logger::new(&dir, true));
        let st = crate::store::Store::open(dir.clone(), log.clone());
        super::super::rt::set_ctx(Arc::new(crate::platforms::Ctx::new(st.settings.clone(), st.secrets.clone(), log, "t".into(), dir.clone())));
        let seen: Arc<Mutex<Vec<(&str, Frame)>>> = Arc::default();
        let tap = |who: &'static str| -> Emit {
            let seen = seen.clone();
            Arc::new(move |f| seen.lock().unwrap().push((who, f)))
        };
        let inbox = Inbox::new(tap("inbox"));
        inbox.clone().start();

        let bob = Person { id: "2".into(), login: "zzbob".into(), name: "ZzBob".into() };
        record("zzbob", &chat_msg("w1", 1, &bob, "first", ""), Some(&bob)); // no tab open yet
        assert_eq!(seen.lock().unwrap().iter().map(|(w, f)| (*w, f["type"].as_str().unwrap().to_string())).collect::<Vec<_>>(), vec![("inbox", "whisper".to_string())]);

        let conv = Conversation::new("zzbob".into(), tap("conv"));
        conv.clone().start();
        let snap = conv.snapshot();
        assert_eq!(snap[1]["type"], "chatlog");
        assert_eq!(snap[1]["messages"][0]["text"], "first"); // read back from the file
        record("zzbob", &chat_msg("w2", 2, &bob, "second", ""), Some(&bob));
        let last = seen.lock().unwrap().iter().rev().find(|(w, f)| *w == "conv" && f["type"] == "chat").map(|(_, f)| f.clone()).unwrap();
        assert_eq!((last["type"].as_str(), last["msg"]["text"].as_str()), (Some("chat"), Some("second")));

        conv.stop();
        inbox.stop();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn scopes_need_both() {
        let mut have = vec![SCOPE_READ.to_string()];
        assert!(!has_scopes(&have));
        have.push(SCOPE_MANAGE.to_string());
        assert!(has_scopes(&have));
    }

    #[test]
    fn twitchs_refusals_become_sentences() {
        assert!(explain(403, "The account that's sending the message doesn't have a verified phone number.", "Bob").contains("verified phone number"));
        assert!(explain(403, "The recipient's settings prevent this sender from whispering them.", "Bob").starts_with("Bob does not accept"));
        assert!(explain(403, "The recipient blocked the sender.", "Bob").contains("blocked"));
        assert!(explain(429, "Too Many Requests", "Bob").contains("too fast"));
        assert!(explain(400, "Whisper is too long", "Bob").contains("too long"));
        assert_eq!(explain(401, "Missing scope: user:manage:whispers", "Bob"), RELOGIN);
        assert_eq!(explain(500, "boom", "Bob"), "Twitch refused the whisper: boom");
        assert_eq!(explain(500, "", "Bob"), "Twitch refused the whisper (HTTP 500).");
    }
}
