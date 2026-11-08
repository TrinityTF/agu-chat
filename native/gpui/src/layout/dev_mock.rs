//! Port of src/renderer/layout/devMock.ts: a stand-in `AguBridge` so the UI runs and can be screenshotted without
//! the real backend (env `AGU_MOCK=1`; until the backend is wired in it is used either way).
//! Like the JS one it answers subscribe with `conn` frames (connecting, then open/offline/error/unavailable by
//! channel name) and an empty `chatlog`; on top of that, an open source emits fake `chat` frames so the chat
//! placeholder has something to show.
//!
//! Persistence (differs from devMock.ts, which used sessionStorage): `layout.json` and `settings.json` are read
//! from the data folder (env `AGU_DATA_DIR`, else `%APPDATA%\Agu Chat`), and layout changes are written back only
//! when `AGU_DATA_DIR` is set, so a run never touches a real install's files.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::protocol::*;
use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Value};

use crate::layout::tree::sanitize_layout;
use crate::window_state;

type FrameCb = Arc<dyn Fn(&SubId, &Frame) + Send + Sync>;

struct Inner {
    frame_cbs: Mutex<HashMap<u64, FrameCb>>,
    settings_cbs: Mutex<HashMap<u64, Arc<dyn Fn(&Settings) + Send + Sync>>>,
    accounts_cbs: Mutex<HashMap<u64, Arc<dyn Fn(&AccountsState) + Send + Sync>>>,
    updater_cbs: Mutex<HashMap<u64, Arc<dyn Fn(&UpdaterState) + Send + Sync>>>,
    next_cb: AtomicU64,
    next_sub: AtomicU64,
    subs: Mutex<HashMap<SubId, Arc<AtomicBool>>>,
    settings: Mutex<Settings>,
    layout: Mutex<Option<Layout>>,
    accounts: Mutex<AccountsState>,
    updater: Mutex<UpdaterState>,
    twitch_login: Mutex<Option<Arc<AtomicBool>>>,
    modes: Mutex<Value>,
    shield: AtomicBool,
    terms: Mutex<Vec<(String, String)>>,
}

pub struct MockBridge {
    inner: Arc<Inner>,
}

fn read_json(file: &str) -> Option<Value> {
    let dir = window_state::data_dir()?;
    let bytes = std::fs::read(dir.join(file)).ok()?;
    // JSON.parse rejects a BOM; serde does too, so strip it.
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes).to_vec();
    serde_json::from_slice(&bytes).ok()
}

impl MockBridge {
    pub fn new() -> Arc<dyn AguBridge> {
        let settings = read_json("settings.json").and_then(|v| serde_json::from_value::<Settings>(v).ok()).unwrap_or_default();
        let layout = read_json("layout.json").map(|v| sanitize_layout(&v));
        let mut accounts: AccountsState = HashMap::new();
        if std::env::var("AGU_HARNESS").is_ok() {
            // the harness page's accounts: a_g_u__ on Twitch, a Rumble login
            accounts.insert(Platform::Twitch, AccountState { connected: true, user: Some(AccountUser { id: "1".into(), login: "a_g_u__".into(), name: "a_g_u__".into() }), ..Default::default() });
            accounts.insert(Platform::Rumble, AccountState { connected: true, ..Default::default() });
            accounts.insert(Platform::Kick, AccountState { connected: true, user: Some(AccountUser { id: "5".into(), login: "mock-kick".into(), name: "MockKick".into() }), ..Default::default() });
        } else {
            accounts.insert(Platform::Kick, AccountState::default());
            accounts.insert(Platform::Twitch, AccountState::default());
            accounts.insert(Platform::Rumble, AccountState::default());
        }
        for p in [Platform::Youtube, Platform::X] {
            accounts.insert(p, AccountState { unsupported: true, ..Default::default() });
        }
        let inner = Inner {
            frame_cbs: Mutex::new(HashMap::new()),
            settings_cbs: Mutex::new(HashMap::new()),
            accounts_cbs: Mutex::new(HashMap::new()),
            updater_cbs: Mutex::new(HashMap::new()),
            next_cb: AtomicU64::new(1),
            next_sub: AtomicU64::new(1),
            subs: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings),
            layout: Mutex::new(layout),
            accounts: Mutex::new(accounts),
            updater: Mutex::new(UpdaterState { status: "dev".into(), channel: "beta".into(), version: None, notes: vec![], progress: 0.0, error: None }),
            twitch_login: Mutex::new(None),
            modes: Mutex::new(json!({
                "emote_mode": false, "follower_mode": false, "follower_mode_duration": 0, "slow_mode": true,
                "slow_mode_wait_time": 30, "subscriber_mode": false, "unique_chat_mode": false
            })),
            shield: AtomicBool::new(false),
            terms: Mutex::new(vec![("1".into(), "spoiler".into())]),
        };
        Arc::new(MockBridge { inner: Arc::new(inner) })
    }
}

impl Inner {
    fn emit(&self, sub: &SubId, frame: Frame) {
        let cbs: Vec<FrameCb> = self.frame_cbs.lock().unwrap().values().cloned().collect();
        for cb in cbs {
            cb(sub, &frame);
        }
    }

    fn set_account(&self, p: Platform, st: AccountState) {
        let snapshot = {
            let mut a = self.accounts.lock().unwrap();
            a.insert(p, st);
            a.clone()
        };
        let cbs: Vec<_> = self.accounts_cbs.lock().unwrap().values().cloned().collect();
        for cb in cbs {
            cb(&snapshot);
        }
    }

    fn set_updater(&self, f: impl FnOnce(&mut UpdaterState)) {
        let snapshot = {
            let mut u = self.updater.lock().unwrap();
            f(&mut u);
            u.clone()
        };
        let cbs: Vec<_> = self.updater_cbs.lock().unwrap().values().cloned().collect();
        for cb in cbs {
            cb(&snapshot);
        }
    }
}

const NAMES: [(&str, &str); 8] = [
    ("Mock_Viewer", "#ff4d94"),
    ("pogchamp_fan", "#00bfff"),
    ("lurker_supreme", "#9acd32"),
    ("EmoteEnjoyer", "#ffa500"),
    ("quiet_mod", "#8a2be2"),
    ("night_owl_77", "#1e90ff"),
    ("clip_goblin", "#ff6347"),
    ("a_g_u__", "#a970ff"),
];
const LINES: [&str; 10] = [
    "hello chat",
    "this layout looks the same as the old one",
    "can we get a split to the right?",
    "first time here, what did I miss",
    "the tab bar is so clean",
    "LUL",
    "drag the header onto the edge of another split",
    "gg",
    "anyone else seeing the title bar flicker",
    "is this the native build?",
];

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// The chatlog frames of chat/dev/harness.tsx (badge images are the real CDN ones: the harness' data: SVGs
/// cannot be fetched here).
fn harness_log(platform: Platform) -> Value {
    let now = now_ms();
    let user = |id: &str, login: &str, color: &str| {
        let name = format!("{}{}", login[..1].to_uppercase(), &login[1..]);
        json!({ "id": id, "login": login, "name": name, "color": color })
    };
    let msg = |i: i64, u: &Value, text: &str, extra: Value| {
        let mut m = json!({ "platform": "twitch", "id": format!("m{i}"), "ts": now - (60 - i) * 4000, "user": u, "badges": "", "text": text, "emotes": "", "action": false, "firstMsg": false, "reply": null, "rewardId": null, "msgId": null });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            m[k] = v;
        }
        m
    };
    match platform {
        Platform::Twitch => {
            let (a, b, c, m) = (user("1", "a_g_u__", "#9147ff"), user("2", "cooldude", "#ff4500"), user("3", "dimguy", "#1a1a8a"), user("4", "modperson", "#00ff7f"));
            let mut messages = vec![
                msg(1, &a, "hello chat Kappa 7TV test", json!({ "badges": "broadcaster/1", "emotes": "25:11-15" })),
                msg(2, &b, "this is a fairly long message that is going to wrap onto a second line so we can see how the dense layout and the line height behave when text wraps in a narrow split", json!({ "badges": "subscriber/12,moderator/1", "badgeInfo": "subscriber/70" })),
                msg(3, &c, "first time here!", json!({ "firstMsg": true })),
                msg(4, &m, "@a_g_u__ you are mentioned, check https://example.com/page.", json!({ "badges": "moderator/1" })),
                msg(5, &b, "replying to you", json!({ "reply": { "user": "a_g_u__", "text": "hello chat Kappa 7TV test" } })),
                msg(6, &c, "this one will be deleted", json!({ "deleted": true })),
                msg(7, &b, "Cheer100 told you, ogre mound", json!({ "bits": 100 })),
                msg(12, &c, "cheer1 bibleTHUMP5000 Cheer100x plus Kappa1000 gg", json!({ "bits": 6101 })),
                msg(13, &a, "Cheer100 without bits stays text", json!({})),
                msg(8, &c, "redeem text", json!({ "rewardId": "r1", "reward": { "title": "Hydrate", "cost": 500 } })),
                msg(9, &a, "waves", json!({ "action": true })),
                json!({ "notice": true, "id": "n1", "ts": now - 30000, "system": "cooldude subscribed with Prime. They've subscribed for 12 months!", "msgId": "resub", "user": b, "text": "love the stream", "emotes": "" }),
                json!({ "notice": true, "id": "n2", "ts": now - 29000, "system": "5 raiders from somechannel have joined!", "msgId": "raid", "user": c, "text": "" }),
            ];
            if let Some(fx) = crate::chat::twitch::cosmetics::paint_fixture() {
                // painted-name test rows: one message per fixture user
                messages.clear();
                for (i, u) in fx.get("users").and_then(|u| u.as_array()).into_iter().flatten().enumerate() {
                    let g = |k: &str| u.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let user = json!({ "id": g("id"), "login": g("name").to_lowercase(), "name": g("name"), "color": "#ff4500" });
                    messages.push(msg(10 + i as i64, &user, &format!("filler message number {}", 10 + i), json!({})));
                }
            }
            for i in 10..40 {
                if std::env::var_os("AGU_PAINT_FIXTURE").is_some() {
                    break;
                }
                messages.push(msg(i, if i % 2 == 1 { &b } else { &c }, &format!("filler message number {i}"), if i % 2 == 1 { json!({ "badges": "subscriber/12,moderator/1", "badgeInfo": "subscriber/70" }) } else { json!({}) }));
            }
            json!({
                "type": "chatlog", "channelId": "12826", "messages": messages,
                "pinned": msg(100, &m, "Pinned: be nice to each other", json!({ "badges": "moderator/1", "pin": { "by": { "id": "4", "login": "modperson", "name": "Modperson" }, "startsAt": null, "endsAt": null } })),
                "modfeed": [{ "ts": now - 20000, "kind": "timeout", "text": "A moderator timed out someone for 10m" }],
                "automod": [{ "id": "h1", "ts": now - 15000, "user": { "id": "9", "login": "sketchy", "name": "Sketchy" }, "text": "some borderline message", "reason": "automod", "category": "swearing", "level": 2, "status": "held", "resolvedBy": null }],
                "suspicious": [{ "id": "3", "login": "dimguy", "name": "Dimguy", "status": "active_monitoring", "types": ["manually_added"], "evaluation": null, "sharedBanChannels": 0 }],
                "esub": { "active": true, "reason": null }
            })
        }
        Platform::Rumble => json!({ "type": "chatlog", "messages": [
            { "platform": "rumble", "id": "r1", "ts": now - 25000, "user": { "id": "ru1", "login": "rumbler", "name": "Rumbler", "color": "#85c742", "avatar": "" }, "badges": [], "text": "hey from rumble :r+cooldoge: yo", "emotes": "", "action": false, "firstMsg": false, "reply": null, "isFollower": true, "channel": null, "rant": null, "notice": null },
            { "platform": "rumble", "id": "r2", "ts": now - 18000, "user": { "id": "ru2", "login": "rich", "name": "Rich", "color": "#ffaa00", "avatar": "" }, "badges": [], "text": "have a rant", "emotes": "", "action": false, "firstMsg": false, "reply": null, "isFollower": true, "channel": null, "rant": { "cents": 500, "duration": 60, "expiresAt": now + 60000 }, "notice": null }
        ], "status": { "live": true }, "emotes": {}, "badges": {} }),
        Platform::Kick => json!({ "type": "chatlog", "messages": [
            { "platform": "kick", "id": "k1", "ts": now - 22000, "user": { "id": "ki1", "login": "kicker", "name": "Kicker", "color": "#53fc18", "avatar": "", "verified": true, "anonymous": false }, "badges": [{ "type": "moderator", "text": "Moderator", "count": 0 }, { "type": "subscriber", "text": "Subscriber", "count": 3 }], "text": "kick message [emote:37226:KEKW] nice", "reply": null, "action": false, "notice": null }
        ], "status": { "live": true }, "emotes": { "KEKW": { "id": "37226", "group": "global" } } }),
        Platform::Youtube => json!({ "type": "chatlog", "messages": [
            { "platform": "youtube", "id": "y1", "ts": now - 21000, "user": { "id": "yu1", "name": "YTViewer", "avatar": "" }, "badges": [{ "label": "Member", "icon": "", "url": "" }], "parts": [{ "t": "text", "v": "hello youtube " }], "text": "hello youtube", "paid": null, "notice": null },
            { "platform": "youtube", "id": "y2", "ts": now - 14000, "user": { "id": "yu2", "name": "BigSpender", "avatar": "" }, "badges": [], "parts": [{ "t": "text", "v": "super chat!" }], "text": "super chat!", "paid": { "amount": "$10.00", "bg": "rgba(230,81,0,.5)", "headerBg": "", "sticker": false }, "notice": null }
        ], "status": { "live": true } }),
        Platform::X => json!({ "type": "chatlog", "messages": [{ "platform": "x", "id": "x1", "ts": now - 19000, "user": { "id": "xu1", "name": "XPerson", "login": "xperson", "avatar": "", "twitterId": "1" }, "text": "hello from X https://x.com" }], "status": { "live": true } }),
    }
}

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

fn spawn_source(inner: Arc<Inner>, id: SubId, source: SourceRef, alive: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let conn = |state: &str, extra: Value| {
            let mut f = json!({ "type": "conn", "state": state });
            if let (Some(o), Some(e)) = (f.as_object_mut(), extra.as_object()) {
                for (k, v) in e {
                    o.insert(k.clone(), v.clone());
                }
            }
            inner.emit(&id, f);
        };
        std::thread::sleep(Duration::from_millis(20));
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        conn("connecting", json!({}));
        std::thread::sleep(Duration::from_millis(580));
        if !alive.load(Ordering::Relaxed) {
            return;
        }
        let ch = source.channel.to_lowercase();
        let mut chatty = false;
        if ch.contains("err") {
            conn("error", json!({ "detail": "Mock: connection refused" }));
        } else if ch.contains("off") {
            conn("offline", json!({ "detail": "Not live" }));
        } else if ch.contains("na") {
            conn("unavailable", json!({ "detail": "Chat is disabled for this live stream." }));
        } else {
            conn("open", json!({ "live": true, "title": format!("Mock stream on {}: {}", source.platform.as_str(), source.channel), "canSend": std::env::var("AGU_HARNESS").is_ok() && !matches!(source.platform, Platform::Youtube | Platform::X) }));
            chatty = true;
        }
        if chatty && std::env::var("AGU_HARNESS").map(|v| v == "1").unwrap_or(false) {
            // port of chat/dev/harness.tsx: the same frames the Electron harness page replays
            inner.emit(&id, harness_log(source.platform));
            return;
        }
        inner.emit(&id, json!({ "type": "chatlog", "messages": [] }));
        if !chatty {
            return;
        }
        let mut seed = id.bytes().fold(0x9E3779B97F4A7C15u64, |a, b| a.wrapping_mul(31).wrapping_add(b as u64)) ^ source.channel.len() as u64;
        let mut n = 0u64;
        while alive.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(500 + lcg(&mut seed) % 900));
            if !alive.load(Ordering::Relaxed) {
                break;
            }
            let (name, color) = NAMES[(lcg(&mut seed) % NAMES.len() as u64) as usize];
            let text = LINES[(lcg(&mut seed) % LINES.len() as u64) as usize];
            n += 1;
            inner.emit(
                &id,
                json!({
                    "type": "chat",
                    "platform": source.platform.as_str(),
                    "id": format!("mock-{id}-{n}"),
                    "user": { "login": name.to_lowercase(), "name": name, "color": color },
                    "text": text,
                    "ts": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
                }),
            );
        }
    });
}

impl AguBridge for MockBridge {
    fn subscribe(&self, source: SourceRef) -> BoxFuture<'static, Result<SubId, String>> {
        let inner = self.inner.clone();
        async move {
            let id = format!("sub{}", inner.next_sub.fetch_add(1, Ordering::Relaxed));
            let alive = Arc::new(AtomicBool::new(true));
            inner.subs.lock().unwrap().insert(id.clone(), alive.clone());
            spawn_source(inner.clone(), id.clone(), source, alive);
            Ok(id)
        }
        .boxed()
    }

    fn unsubscribe(&self, sub_id: SubId) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        async move {
            if let Some(a) = inner.subs.lock().unwrap().remove(&sub_id) {
                a.store(false, Ordering::Relaxed);
            }
        }
        .boxed()
    }

    fn on_frame(&self, cb: Arc<dyn Fn(&SubId, &Frame) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_cb.fetch_add(1, Ordering::Relaxed);
        self.inner.frame_cbs.lock().unwrap().insert(id, cb);
        let inner = self.inner.clone();
        Box::new(move || {
            inner.frame_cbs.lock().unwrap().remove(&id);
        })
    }

    // Just enough relay to try the Modes panel: a mod everywhere, with in-memory chat settings and terms.
    fn api(&self, _source: SourceRef, path: String, init: ApiInit) -> BoxFuture<'static, ApiResult> {
        let inner = self.inner.clone();
        async move {
            let (p, query) = path.split_once('?').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((path.clone(), String::new()));
            let method = init.method.clone().unwrap_or_else(|| "GET".into());
            let body = init.body.clone().unwrap_or_else(|| json!({}));
            let ok = |b: Value| ApiResult { status: 200, body: b };
            match p.as_str() {
                "/api/mod/user" if std::env::var("AGU_HARNESS").is_ok() => ok(json!({ "user": { "id": "2", "login": "cooldude", "displayName": "Cooldude", "avatar": "", "createdAt": "2016-04-30T12:00:00Z" }, "followedAt": "2019-07-09T12:00:00Z" })),
                "/api/twitch/web" => {
                    let signed_in = std::env::var("AGU_MOCK_WEB").map(|v| v != "out").unwrap_or(true);
                    ok(if signed_in { json!({ "signedIn": true, "pending": false, "expired": false, "login": "a_g_u__", "sameUser": std::env::var("AGU_MOCK_WEB").map(|v| v != "other").unwrap_or(true), "appLogin": "a_g_u__" }) } else { json!({ "signedIn": false, "pending": false, "expired": false }) })
                }
                "/api/twitch/web/login" | "/api/twitch/web/cancel" | "/api/twitch/web/logout" => ok(json!({ "ok": true })),
                "/api/twitch/card" => {
                    if std::env::var("AGU_MOCK_WEB").map(|v| v == "out").unwrap_or(false) {
                        return ApiResult { status: 401, body: json!({ "error": "Sign in on twitch.tv first", "code": "signed-out" }) };
                    }
                    let login = query.split('&').find_map(|kv| kv.strip_prefix("login=")).unwrap_or("cooldude").to_string();
                    let t = |days: i64| chrono::DateTime::from_timestamp_millis(now_ms() - days * 86_400_000).map(|d| d.to_rfc3339()).unwrap_or_default();
                    ok(json!({
                        "user": { "id": "2", "login": login, "displayName": "Cooldude", "avatar": "", "createdAt": "2016-04-30T12:00:00Z" },
                        "followedAt": "2019-07-09T12:00:00Z",
                        "sub": { "tier": 2, "months": 14, "gift": false, "prime": false },
                        "isModerator": false,
                        "badges": [
                            { "title": "Subscriber", "description": "14-Month Subscriber", "url": "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/2" },
                            { "title": "Moderator", "description": "Moderator", "url": "https://static-cdn.jtvnw.net/badges/v1/3267646d-33f0-4b17-b3df-f923a41db1d0/2" },
                            { "title": "cheer 1000", "description": "cheer 1000", "url": "https://static-cdn.jtvnw.net/badges/v1/0d85a29f-79d1-4d5d-8c5f-e5d8f5bea4e5/2" }
                        ],
                        "canAccessModLogs": true,
                        "ban": null,
                        "logs": {
                            "messages": { "count": 1000, "more": true, "label": "1000+" },
                            "warnings": { "label": "1", "items": [{ "id": "w1", "ts": t(3), "text": "Modperson warned Cooldude: spam", "type": "WARN" }] },
                            "timeouts": { "label": "2", "items": [{ "id": "t1", "ts": t(2), "text": "Modperson timed out Cooldude for 10m: caps", "type": "TIMEOUT" }, { "id": "t2", "ts": t(1), "text": "Modperson timed out Cooldude for 1h", "type": "TIMEOUT" }] },
                            "untimeouts": { "label": "1", "items": [{ "id": "u1", "ts": t(1), "text": "Modperson removed timeout on Cooldude", "type": "UNTIMEOUT" }] },
                            "bans": { "label": "0", "items": [] }, "unbans": { "label": "0", "items": [] },
                            "comments": [{ "id": "c1", "ts": t(5), "text": "Keeps spamming links, watch this one", "by": "modperson", "byName": "Modperson", "byId": "4" }, { "id": "c2", "ts": t(1), "text": "Calmed down after the timeout", "by": "a_g_u__", "byName": "a_g_u__", "byId": "1" }]
                        },
                        "sameUser": std::env::var("AGU_MOCK_WEB").map(|v| v != "other").unwrap_or(true),
                        "me": { "login": "a_g_u__" }
                    }))
                }
                "/api/twitch/card/messages" => {
                    let t = |m: i64| chrono::DateTime::from_timestamp_millis(now_ms() - m * 60_000).map(|d| d.to_rfc3339()).unwrap_or_default();
                    ok(json!({ "items": [
                        { "kind": "chat", "id": "x1", "ts": t(4), "text": "this is a fairly long message that is going to wrap onto a second line so we can see how it looks" },
                        { "kind": "action", "ts": t(6), "text": "Modperson timed out Cooldude for 10m" },
                        { "kind": "caught", "id": "x2", "ts": t(9), "text": "some borderline message", "status": "DENIED" },
                        { "kind": "chat", "id": "x3", "ts": t(20), "text": "first message" }
                    ], "more": false, "cursor": null }))
                }
                "/api/twitch/card/comment" => ok(json!({ "comment": { "id": "c3", "ts": chrono::Utc::now().to_rfc3339(), "text": "(mock) comment", "by": "a_g_u__", "byName": "a_g_u__", "byId": "1" }, "ok": true })),
                "/api/mod/caps" => ok(json!({ "pin": true, "modes": true, "shield": true, "terms": true, "ban": true, "warn": true, "delete": true, "automod": true, "loggedIn": true, "isMod": true })),
                "/api/rumble/caps" | "/api/kick/caps" => ok(json!({ "connected": true, "canModerate": true, "canDelete": true })),
                "/api/chat/badges" if std::env::var("AGU_HARNESS").is_ok() => ok(json!({ "channelId": "12826", "global": [
                    { "set_id": "moderator", "versions": [{ "id": "1", "image_url_2x": "https://static-cdn.jtvnw.net/badges/v1/3267646d-33f0-4b17-b3df-f923a41db1d0/2", "title": "Moderator" }] },
                    { "set_id": "broadcaster", "versions": [{ "id": "1", "image_url_2x": "https://static-cdn.jtvnw.net/badges/v1/5527c58c-fb7d-422d-b71b-f309dcb85cc1/2", "title": "Broadcaster" }] },
                    { "set_id": "subscriber", "versions": [{ "id": "12", "image_url_2x": "https://static-cdn.jtvnw.net/badges/v1/5d9f2208-5dd8-11e7-8513-2ff4adfae661/2", "title": "Sub 12" }] }], "channel": [] })),
                "/api/chat/cheermotes" if std::env::var("AGU_HARNESS").is_ok() => {
                    let tier = |bits: u64, color: &str, name: &str| {
                        let u = |kind: &str, ext: &str| json!({ "1": format!("https://d3aqoihi2n8ty8.cloudfront.net/actions/{name}/dark/{kind}/{bits}/1.{ext}"), "2": format!("https://d3aqoihi2n8ty8.cloudfront.net/actions/{name}/dark/{kind}/{bits}/2.{ext}") });
                        json!({ "min_bits": bits, "color": color, "images": { "dark": { "animated": u("animated", "gif"), "static": u("static", "png") } } })
                    };
                    let tiers = |n: &str| json!([tier(1, "#979797", n), tier(100, "#9c3ee8", n), tier(1000, "#1db2a5", n), tier(5000, "#0099fe", n), tier(10000, "#f43021", n)]);
                    ok(json!({ "data": [{ "prefix": "Cheer", "tiers": tiers("cheer") }, { "prefix": "BibleThump", "tiers": tiers("biblethump") }] }))
                }
                "/api/chat/settings" => {
                    let mut m = inner.modes.lock().unwrap();
                    if method == "POST" {
                        if let (Some(mo), Some(bo)) = (m.as_object_mut(), body.as_object()) {
                            for (k, v) in bo {
                                mo.insert(k.clone(), v.clone());
                            }
                        }
                    }
                    ok(m.clone())
                }
                "/api/chat/shield" => {
                    if method == "PUT" {
                        inner.shield.store(body.get("is_active").and_then(|v| v.as_bool()).unwrap_or(false), Ordering::Relaxed);
                    }
                    ok(json!({ "is_active": inner.shield.load(Ordering::Relaxed) }))
                }
                "/api/chat/terms" => {
                    let mut t = inner.terms.lock().unwrap();
                    if method == "POST" {
                        let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis().to_string()).unwrap_or_default();
                        t.push((id, body.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string()));
                    }
                    if method == "DELETE" {
                        let want = query.split('&').find_map(|kv| kv.strip_prefix("id=")).unwrap_or("").to_string();
                        t.retain(|(id, _)| *id != want);
                    }
                    ok(json!({ "terms": t.iter().map(|(id, text)| json!({ "id": id, "text": text })).collect::<Vec<_>>() }))
                }
                _ => ApiResult { status: 404, body: json!({ "error": "not found" }) },
            }
        }
        .boxed()
    }

    fn accounts_get(&self) -> BoxFuture<'static, AccountsState> {
        let inner = self.inner.clone();
        async move { inner.accounts.lock().unwrap().clone() }.boxed()
    }

    fn accounts_login(&self, platform: Platform, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let inner = self.inner.clone();
        async move {
            match platform {
                Platform::Twitch => {
                    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) + 15 * 60_000;
                    let pending = DevicePrompt {
                        user_code: "ABCDEFGH".into(),
                        verification_uri: "https://www.twitch.tv/activate?device-code=ABCDEFGH".into(),
                        expires_at: expires,
                    };
                    inner.set_account(Platform::Twitch, AccountState { pending: Some(pending.clone()), ..Default::default() });
                    let cancelled = Arc::new(AtomicBool::new(false));
                    *inner.twitch_login.lock().unwrap() = Some(cancelled.clone());
                    let inner2 = inner.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(8));
                        if !cancelled.load(Ordering::Relaxed) {
                            inner2.set_account(
                                Platform::Twitch,
                                AccountState {
                                    connected: true,
                                    user: Some(AccountUser { id: "1".into(), login: "mock_user".into(), name: "Mock_User".into() }),
                                    scopes: Some(vec!["chat:read".into(), "chat:edit".into(), "moderator:manage:banned_users".into()]),
                                    ..Default::default()
                                },
                            );
                        }
                    });
                    serde_json::to_value(pending).map_err(|e| e.to_string())
                }
                Platform::Rumble => {
                    let username = args.get("username").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let password = args.get("password").and_then(|v| v.as_str()).map(|s| s.to_string());
                    if let Some(code) = args.get("code").and_then(|v| v.as_str()) {
                        if code != "123456" {
                            return Err("Wrong code (mock accepts 123456).".into());
                        }
                        inner.set_account(
                            Platform::Rumble,
                            AccountState { connected: true, user: Some(AccountUser { id: "r1".into(), login: "mockrumble".into(), name: "MockRumble".into() }), ..Default::default() },
                        );
                        return Ok(json!({ "ok": true }));
                    }
                    if username.as_deref() == Some("2fa") {
                        return Ok(json!({ "twoFactor": true }));
                    }
                    if password.as_deref() == Some("bad") {
                        return Err("Invalid username or password.".into());
                    }
                    let u = username.unwrap_or_default();
                    inner.set_account(
                        Platform::Rumble,
                        AccountState { connected: true, user: Some(AccountUser { id: "r1".into(), login: u.clone(), name: u }), ..Default::default() },
                    );
                    Ok(json!({ "ok": true }))
                }
                Platform::Kick => {
                    // the browser round trip: pending, then logged in a few seconds later
                    let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) + 5 * 60_000;
                    inner.set_account(Platform::Kick, AccountState { pending: Some(DevicePrompt { user_code: String::new(), verification_uri: "https://kick.com/".into(), expires_at: expires }), ..Default::default() });
                    let cancelled = Arc::new(AtomicBool::new(false));
                    *inner.twitch_login.lock().unwrap() = Some(cancelled.clone());
                    let inner2 = inner.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(4));
                        if !cancelled.load(Ordering::Relaxed) {
                            inner2.set_account(Platform::Kick, AccountState { connected: true, user: Some(AccountUser { id: "5".into(), login: "mock-kick".into(), name: "MockKick".into() }), ..Default::default() });
                        }
                    });
                    Ok(json!({ "pending": true }))
                }
                _ => Err("Not supported in the mock.".into()),
            }
        }
        .boxed()
    }

    fn accounts_cancel(&self, platform: Platform) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        async move {
            if let Some(c) = inner.twitch_login.lock().unwrap().take() {
                c.store(true, Ordering::Relaxed);
            }
            inner.set_account(platform, AccountState::default());
        }
        .boxed()
    }

    fn accounts_logout(&self, platform: Platform) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        async move { inner.set_account(platform, AccountState::default()) }.boxed()
    }

    fn on_accounts(&self, cb: Arc<dyn Fn(&AccountsState) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_cb.fetch_add(1, Ordering::Relaxed);
        self.inner.accounts_cbs.lock().unwrap().insert(id, cb);
        let inner = self.inner.clone();
        Box::new(move || {
            inner.accounts_cbs.lock().unwrap().remove(&id);
        })
    }

    fn settings_get(&self) -> Settings {
        self.inner.settings.lock().unwrap().clone()
    }

    fn settings_set(&self, patch: Value) -> BoxFuture<'static, Settings> {
        let inner = self.inner.clone();
        async move {
            let next = {
                let mut cur = inner.settings.lock().unwrap();
                let mut as_json = serde_json::to_value(&*cur).unwrap_or(json!({}));
                if let (Some(map), Some(p)) = (as_json.as_object_mut(), patch.as_object()) {
                    for (k, v) in p {
                        // Like the mock: unknown keys and values of the wrong type are ignored.
                        let same_kind = map.get(k).map(|old| std::mem::discriminant(old) == std::mem::discriminant(v)).unwrap_or(false);
                        if same_kind {
                            map.insert(k.clone(), if let Value::String(s) = v { Value::String(s.trim().to_string()) } else { v.clone() });
                        }
                    }
                }
                if let Ok(s) = serde_json::from_value::<Settings>(as_json) {
                    *cur = s;
                }
                cur.clone()
            };
            let cbs: Vec<_> = inner.settings_cbs.lock().unwrap().values().cloned().collect();
            for cb in cbs {
                cb(&next);
            }
            next
        }
        .boxed()
    }

    fn on_settings(&self, cb: Arc<dyn Fn(&Settings) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_cb.fetch_add(1, Ordering::Relaxed);
        self.inner.settings_cbs.lock().unwrap().insert(id, cb);
        let inner = self.inner.clone();
        Box::new(move || {
            inner.settings_cbs.lock().unwrap().remove(&id);
        })
    }

    fn layout_get(&self) -> Option<Layout> {
        self.inner.layout.lock().unwrap().clone()
    }

    fn layout_set(&self, layout: Layout) {
        if window_state::writable() {
            if let Some(dir) = window_state::data_dir() {
                if let Ok(s) = serde_json::to_string_pretty(&layout) {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::fs::write(dir.join("layout.json"), s);
                }
            }
        }
        *self.inner.layout.lock().unwrap() = Some(layout);
    }

    fn updater_get(&self) -> UpdaterState {
        self.inner.updater.lock().unwrap().clone()
    }

    fn updater_check(&self) -> BoxFuture<'static, ()> {
        let inner = self.inner.clone();
        async move {
            inner.set_updater(|u| {
                u.status = "checking".into();
                u.error = None;
            });
            std::thread::spawn(move || {
                let step = |ms: u64, f: Box<dyn FnOnce(&mut UpdaterState)>| {
                    std::thread::sleep(Duration::from_millis(ms));
                    inner.set_updater(f);
                };
                step(
                    800,
                    Box::new(|u| {
                        u.status = "available".into();
                        u.version = Some("0.0.1-beta.2".into());
                        u.notes = vec!["Mock: layout work".into(), "Mock: new settings dialog".into()];
                    }),
                );
                step(600, Box::new(|u| {
                    u.status = "downloading".into();
                    u.progress = 10.0;
                }));
                step(600, Box::new(|u| u.progress = 60.0));
                step(600, Box::new(|u| {
                    u.status = "ready".into();
                    u.progress = 100.0;
                }));
            });
        }
        .boxed()
    }

    fn updater_install(&self) -> BoxFuture<'static, ()> {
        async {}.boxed()
    }

    fn on_updater(&self, cb: Arc<dyn Fn(&UpdaterState) + Send + Sync>) -> Unsubscribe {
        let id = self.inner.next_cb.fetch_add(1, Ordering::Relaxed);
        self.inner.updater_cbs.lock().unwrap().insert(id, cb);
        let inner = self.inner.clone();
        Box::new(move || {
            inner.updater_cbs.lock().unwrap().remove(&id);
        })
    }

    fn app_info(&self) -> AppInfo {
        AppInfo { version: "0.0.1-beta.1".into(), packaged: false }
    }

    fn open_external(&self, _url: String) {}

    fn sounds_import(&self, _path: std::path::PathBuf) -> BoxFuture<'static, Result<String, String>> {
        async { Ok("mock-sound.mp3".to_string()) }.boxed()
    }

    fn sounds_read(&self, _file: String) -> BoxFuture<'static, Result<Vec<u8>, String>> {
        async { Err("No sound files in the mock.".to_string()) }.boxed()
    }
}
