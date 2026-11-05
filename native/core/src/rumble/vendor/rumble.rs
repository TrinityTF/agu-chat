//! Port of src/main/rumble/vendor/rumble.js (render lib/rumble.js): Rumble co-chat helpers. The relay class
//! is gone (mod.rs owns the lifecycle for a single in-process subscriber).
//!
//! Rumble has no chat WebSocket and no public chat API. Its own web client reads Server-Sent Events from
//! `GET https://<front-end host>.rumble.com/chat/api/chat/<chatId>/stream` (unauthenticated). Chat is per
//! VIDEO, so a source owns re-resolving which video its channel is live on.
//!
//! EVERY request goes to one of Rumble's front-end hosts (wn0, web7: rumble_hosts.rs), never the rumble.com
//! apex, and nothing here tries to get past a Cloudflare challenge: no TLS impersonation, no scripted
//! browsers, no browser User-Agent. The honest product UA is sent (rumble_hosts::fetch_from_rumble).
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use futures_util::StreamExt;
use regex::Regex;
use serde_json::{json, Map, Value};

use super::rumble_hosts::fetch_from_rumble;
use crate::net::{self, FetchOpts};
use crate::util::{js_str_or_empty, js_string, js_trim, now_ms, parse_rfc3339_ms, truthy};

const FETCH_TIMEOUT_MS: u64 = 20_000;

// ---------------------------------------------------------------------------
// Source parsing + resolution
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct RumbleSource {
    /// "video" | "channel"
    pub kind: &'static str,
    pub id: String,
    /// "c" | "user" (channels only)
    pub space: Option<String>,
}

static PAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(v[0-9a-z]+)(?:-[^/]*)?\.html$").unwrap());
static EMBED_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:embed/)?(v[0-9a-z]{4,})$").unwrap());
static CHAN_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(c|user)/([A-Za-z0-9_.-]+)").unwrap());
static BARE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+$").unwrap());
static SCHEME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^https?://").unwrap());
static HOST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^(?:www\.)?rumble\.com").unwrap());

/// Turns whatever a user pasted into a normalized source descriptor. Accepts a channel ("rumble.com/c/Foo",
/// "rumble.com/user/Foo", "c/Foo", "Foo"), a video page URL ("rumble.com/v7eh2s2-some-title.html"), or a bare
/// embed id ("v7capuk"). None when nothing recognizable is in there.
///
/// A `channel` source follows whatever that channel is streaming right now, forever; a `video` source is
/// pinned to one broadcast.
pub fn parse_rumble_source(input: &str) -> Option<RumbleSource> {
    let raw = js_trim(input);
    if raw.is_empty() {
        return None;
    }
    // Strip a scheme/host if present so the rest can treat this as a path.
    let s = SCHEME_RE.replace(raw, "");
    let s = HOST_RE.replace(&s, "");
    let s = s.split('?').next().unwrap_or("");
    let s = s.split('#').next().unwrap_or("");
    let path = s.trim_start_matches('/').trim_end_matches('/');
    if path.is_empty() {
        return None;
    }

    // A video page: "<slug>-<title-words>.html", where the slug is the embed id.
    if let Some(c) = PAGE_RE.captures(path) {
        return Some(RumbleSource { kind: "video", id: c[1].to_lowercase(), space: None });
    }
    // "embed/v7capuk" or a bare embed id.
    if let Some(c) = EMBED_RE.captures(path) {
        return Some(RumbleSource { kind: "video", id: c[1].to_lowercase(), space: None });
    }
    // A channel: "c/Name" or "user/Name". Rumble treats the two as separate namespaces, so the prefix is part
    // of the identity and is preserved.
    if let Some(c) = CHAN_RE.captures(path) {
        return Some(RumbleSource { kind: "channel", id: c[2].to_string(), space: Some(c[1].to_lowercase()) });
    }
    // A bare name with no prefix: assume a channel, which is the common case.
    if BARE_RE.is_match(path) {
        return Some(RumbleSource { kind: "channel", id: path.to_string(), space: Some("c".into()) });
    }
    None
}

/// Stable map key for a parsed source.
pub fn rumble_source_key(s: &RumbleSource) -> String {
    if s.kind == "video" { format!("v:{}", s.id) } else { format!("{}:{}", s.space.as_deref().unwrap_or(""), s.id.to_lowercase()) }
}

pub fn rumble_source_url(s: &RumbleSource) -> String {
    if s.kind == "video" { format!("https://rumble.com/{}", s.id) } else { format!("https://rumble.com/{}/{}", s.space.as_deref().unwrap_or(""), s.id) }
}

/// `encodeURIComponent`
pub fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// What a resolve answers (the JS object; `chat_id` None = null).
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub live: bool,
    pub chat_id: Option<String>,
    pub title: String,
    pub category: String,
    pub author: String,
    pub author_url: String,
    pub url: String,
    pub watching: Option<i64>,
}

/// What Rumble files a video under, for the Chat widget's auto-theme. embedJS has no category field of its
/// own; the only place Rumble states one is the ad-targeting block (`pause_ads_params.categories`). Strictly
/// best-effort: anything missing reads as "no category", never as an error.
pub fn rumble_category_of(body: &Value) -> String {
    let Some(cats) = body["a"]["pause_ads_params"]["categories"].as_array() else { return String::new() };
    let re = Regex::new(r"[-_]+").unwrap();
    cats.iter()
        .filter_map(|c| c.as_str())
        .filter(|c| !js_trim(c).is_empty())
        .take(3)
        .map(|c| re.replace_all(js_trim(c), " ").into_owned())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Resolves a pinned video to its chat id via Rumble's embed metadata route (plain JSON with the numeric chat
/// id as `vid`, plus live status and title in the same trip). `live` is a small enum on Rumble's side
/// (0 offline, 2 live); anything else is treated as not-live rather than guessed at.
pub async fn resolve_rumble_video(embed_id: &str) -> Result<Resolved, String> {
    let path = format!("/embedJS/u3/?request=video&ver=2&v={}", enc(embed_id));
    let mut o = FetchOpts::new(FETCH_TIMEOUT_MS).header("Accept", "application/json");
    o.method = "";
    let r = fetch_from_rumble(&path, o).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("rumble embedJS {}", r.status().as_u16()));
    }
    let body = net::json(r).await.map_err(|e| e.message)?;
    video_from_json(&body, embed_id)
}

pub fn video_from_json(body: &Value, embed_id: &str) -> Result<Resolved, String> {
    if !truthy(&body["vid"]) {
        return Err("rumble embedJS: no video id".into());
    }
    Ok(Resolved {
        live: body["live"] == 2,
        chat_id: Some(js_string(&body["vid"])),
        title: js_str_or_empty(Some(&body["title"])),
        category: rumble_category_of(body),
        author: js_str_or_empty(Some(&body["author"]["name"])),
        author_url: js_str_or_empty(Some(&body["author"]["url"])),
        // `l` is the video's path on rumble.com: the link a viewer follows to see the stream itself.
        url: if truthy(&body["l"]) { format!("https://rumble.com{}", js_string(&body["l"])) } else { rumble_source_url(&RumbleSource { kind: "video", id: embed_id.to_string(), space: None }) },
        watching: None,
    })
}

static JSON_SCRIPT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?s)<script type="application/json">(.*?)</script>"#).unwrap());

/// Resolves a channel to whatever it is streaming right now.
///
/// Rumble renders channel pages server-side and embeds the full listing as a single
/// `<script type="application/json">{"items":[...]}</script>` block, each item carrying `id` (the chat id),
/// `live`, `livestream_status` and `permalink_id`. Returns not-live rather than failing when the channel
/// simply isn't streaming: that is a normal state between broadcasts, not an error.
pub async fn resolve_rumble_channel(space: &str, name: &str) -> Result<Resolved, String> {
    // Two URLs, deliberately: the one we FETCH is the front-end host, the one handed to the user is the apex,
    // since that is the link a human clicks and shares.
    let fetch_path = format!("/{space}/{}", enc(name));
    let url = format!("https://rumble.com/{space}/{}", enc(name));
    let r = fetch_from_rumble(&fetch_path, FetchOpts::new(FETCH_TIMEOUT_MS).header("Accept", "text/html")).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("rumble channel {}", r.status().as_u16()));
    }
    let html = net::text(r).await.map_err(|e| e.message)?;

    let offline = Resolved { live: false, chat_id: None, title: String::new(), category: String::new(), author: name.to_string(), author_url: url.clone(), url: url.clone(), watching: None };
    match channel_from_html(&html, name, &url)? {
        ChannelPage::Done(r) => Ok(r),
        ChannelPage::Permalinks(permalinks) => {
            if permalinks.is_empty() {
                return Ok(offline);
            }
            // embedJS is authoritative about live status, so ask it rather than trying to read a "LIVE" badge
            // out of markup that changes. Capped and sequential: this only runs on the poll for an offline source.
            for permalink in permalinks {
                if let Ok(info) = resolve_rumble_video(&permalink).await {
                    if info.live && info.chat_id.is_some() {
                        let author = if info.author.is_empty() { name.to_string() } else { info.author.clone() };
                        return Ok(Resolved { author, author_url: url, ..info });
                    }
                }
                // A permalink that won't resolve is not fatal: try the next.
            }
            Ok(offline)
        }
    }
}

pub enum ChannelPage {
    Done(Resolved),
    /// no JSON payload (/user/ pages never carry one): the video permalinks of the rendered listing
    Permalinks(Vec<String>),
}

/// The pure half of `resolveRumbleChannel`.
pub fn channel_from_html(html: &str, name: &str, url: &str) -> Result<ChannelPage, String> {
    let mut items: Option<Vec<Value>> = None;
    for m in JSON_SCRIPT_RE.captures_iter(html) {
        let Ok(parsed) = serde_json::from_str::<Value>(&m[1]) else { continue }; // some blocks are unrelated config
        if let Some(a) = parsed["items"].as_array() {
            items = Some(a.clone());
            break;
        }
    }

    if let Some(items) = items {
        // `live: true` and `livestream_status: 2` agree in practice; accepting either means a change to one of
        // them doesn't silently blind the resolver. A channel can have more than one live item (a re-stream);
        // the most-watched one is the one a viewer means.
        let mut live: Vec<&Value> = items.iter().filter(|it| (it["live"] == true || it["livestream_status"] == 2) && truthy(&it["id"])).collect();
        live.sort_by(|a, b| {
            let w = |v: &Value| crate::util::js_to_number(Some(&v["watching_now"])).max(0.0);
            w(b).partial_cmp(&w(a)).unwrap_or(std::cmp::Ordering::Equal)
        });
        let Some(live) = live.first() else {
            return Ok(ChannelPage::Done(Resolved { live: false, chat_id: None, title: String::new(), category: String::new(), author: name.to_string(), author_url: url.to_string(), url: url.to_string(), watching: None }));
        };
        return Ok(ChannelPage::Done(Resolved {
            live: true,
            chat_id: Some(js_string(&live["id"])),
            title: js_str_or_empty(Some(&live["title"])),
            category: String::new(),
            author: name.to_string(),
            author_url: url.to_string(),
            url: if truthy(&live["url"]) { js_string(&live["url"]) } else if truthy(&live["permalink_id"]) { format!("https://rumble.com/{}", js_string(&live["permalink_id"])) } else { url.to_string() },
            watching: live["watching_now"].as_f64().map(|n| n as i64),
        }));
    }

    // No JSON payload. That is NOT an error and NOT an empty channel. The page must still look like a
    // channel/user page: if the listing container is missing too, we were handed something else entirely.
    let Some(listing) = extract_listing_html(html) else { return Err("Rumble returned a page this doesn't recognise".into()) };
    Ok(ChannelPage::Permalinks(listing_permalinks(listing, 6)))
}

/// The channel/user listing section, or None when the page has no such section at all. Everything downstream
/// depends on this slice being the CHANNEL'S OWN content: Rumble pages carry a sidebar rail advertising
/// unrelated live channels, and scraping the whole document would attach someone's chat to a stranger's stream.
pub fn extract_listing_html(html: &str) -> Option<&str> {
    let start = html.find("channel-listing__container")?;
    Some(match html[start..].find("</section>") {
        Some(e) => &html[start..start + e],
        None => &html[start..],
    })
}

static PERMALINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"href="/(v[0-9a-z]+)[^"]*\.html"#).unwrap());

/// Distinct video permalinks (the "v7eh2s2" slug) from a listing slice, capped: each costs an embedJS lookup.
pub fn listing_permalinks(listing_html: &str, limit: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in PERMALINK_RE.captures_iter(listing_html) {
        let id = c[1].to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out.truncate(limit);
    out
}

// The chat's emote list: {name: {file, subsOnly}}, scraped from the chat popup page, which embeds it as an
// argument to the page's own RumbleChat(...) call (a JS object literal, not JSON). Cached hard: the page is
// ~500KB and an emote set changes about never.
const EMOTE_TTL_MS: i64 = 30 * 60_000;
static EMOTE_CACHE: LazyLock<Mutex<HashMap<String, (Value, i64)>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static EMOTE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\{name:"([^"]+)",is_subs_only:(true|false),position:(\d+),file:"([^"]+)"\}"#).unwrap());

pub fn emotes_from_html(html: &str) -> Value {
    let mut emotes = Map::new();
    for m in EMOTE_RE.captures_iter(html) {
        emotes.insert(m[1].to_string(), json!({"file": &m[4], "subsOnly": &m[2] == "true"}));
    }
    Value::Object(emotes)
}

pub async fn fetch_rumble_emotes(chat_id: &str) -> Value {
    if chat_id.is_empty() {
        return json!({});
    }
    if let Some((e, at)) = EMOTE_CACHE.lock().unwrap().get(chat_id) {
        if now_ms() - at < EMOTE_TTL_MS {
            return e.clone();
        }
    }
    let attempt: Result<Value, String> = async {
        let r = fetch_from_rumble(&format!("/chat/popup/{}", enc(chat_id)), FetchOpts::new(FETCH_TIMEOUT_MS).header("Accept", "text/html")).await.map_err(|e| e.message)?;
        if !r.status().is_success() {
            return Err(format!("popup {}", r.status().as_u16()));
        }
        Ok(emotes_from_html(&net::text(r).await.map_err(|e| e.message)?))
    }
    .await;
    // No emotes is a degradation, not a failure: shortcodes stay as text. The empty result is cached too, so a
    // broken page doesn't get refetched on every reconnect.
    let emotes = attempt.unwrap_or_else(|_| json!({}));
    EMOTE_CACHE.lock().unwrap().insert(chat_id.to_string(), (emotes.clone(), now_ms()));
    emotes
}

pub async fn resolve_rumble_source(src: &RumbleSource) -> Result<Resolved, String> {
    if src.kind == "video" {
        resolve_rumble_video(&src.id).await
    } else {
        resolve_rumble_channel(src.space.as_deref().unwrap_or("c"), &src.id).await
    }
}

// ---------------------------------------------------------------------------
// Wire normalization
// ---------------------------------------------------------------------------

/// Rumble sends users and channels in a side table keyed by id, and messages reference them by `user_id` /
/// `channel_id`. A message posted with `channel_id` set was posted AS that channel, which Rumble renders with
/// the channel's name and avatar in place of the person's; the underlying user id is kept for moderation.
pub fn to_rumble_chat_msg(raw: &Value, users: &HashMap<String, Value>, channels: &HashMap<String, Value>) -> Value {
    let null = Value::Null;
    let user = users.get(&js_string(&raw["user_id"])).unwrap_or(&null);
    let channel = if raw["channel_id"].is_null() { &null } else { channels.get(&js_string(&raw["channel_id"])).unwrap_or(&null) };
    let ts = raw["time"].as_str().and_then(parse_rfc3339_ms);

    // A rant is Rumble's paid highlighted message. The price arrives in cents; the tier's colour/duration come
    // from the init payload's config, which the page already has.
    let rant = if truthy(&raw["rant"]) {
        json!({
            "cents": net::num_or0(&raw["rant"]["price_cents"]),
            "duration": net::num_or0(&raw["rant"]["duration"]),
            "expiresAt": if truthy(&raw["rant"]["expires_on"]) { raw["rant"]["expires_on"].as_str().and_then(parse_rfc3339_ms).unwrap_or(0) } else { 0 },
        })
    } else {
        Value::Null
    };
    let pick = |a: &Value, b: &Value| js_str_or_empty(Some(if truthy(a) { a } else { b }));
    let uname = js_str_or_empty(Some(&user["username"]));

    json!({
        "platform": "rumble",
        "id": js_string(&raw["id"]),
        "ts": ts.unwrap_or_else(now_ms),
        "user": {
            "id": js_str_or_empty(Some(&raw["user_id"])),
            "login": uname,
            "name": pick(&channel["username"], &user["username"]),
            "color": js_str_or_empty(Some(&user["color"])),
            "avatar": pick(&channel["image.1"], &user["image.1"]),
        },
        // Rumble badges are opaque string ids ("verified", "whale-gray", "moderator", ...), not Twitch's
        // set/version pairs. Kept as an array: the renderer maps the handful it knows and ignores the rest.
        "badges": user["badges"].as_array().map(|a| a.iter().take(8).cloned().collect::<Vec<_>>()).unwrap_or_default(),
        "text": js_str_or_empty(Some(&raw["text"])),
        // No Twitch-style emote ranges exist on Rumble. Present and empty so the shared renderer needs no branch.
        "emotes": "",
        "action": false,
        "firstMsg": false,
        "reply": null,
        "rewardId": null,
        "msgId": null,
        "isFollower": truthy(&user["is_follower"]),
        // The channel a message was posted as, when it wasn't posted as a person.
        "channel": if truthy(channel) { json!({"id": js_string(&channel["id"]), "name": js_str_or_empty(Some(&channel["username"])), "avatar": js_str_or_empty(Some(&channel["image.1"]))}) } else { Value::Null },
        "rant": rant,
        // Rumble's own notification messages (raids, gifted subs) arrive with type !== "regular".
        "notice": if truthy(&raw["type"]) && raw["type"] != "regular" { json!(js_string(&raw["type"])) } else { Value::Null },
    })
}

// ---------------------------------------------------------------------------

/// Pulls complete SSE frames (blank-line separated) out of `buf`, leaving a partial one in place.
pub fn drain_frames(buf: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(i) = buf.windows(2).position(|w| w == b"\n\n") {
        let frame: Vec<u8> = buf.drain(..i + 2).take(i).collect();
        out.push(String::from_utf8_lossy(&frame).into_owned());
    }
    out
}

/// Whether THIS user may moderate THIS chat. The shared source reads the SSE stream anonymously, so its
/// `can_moderate` is always false: it describes a logged-out viewer. Answering it per user means one
/// authenticated read of the same stream: connect with their session cookie, take the `init` frame, hang up.
/// Returns false rather than failing on any problem: an expired session should hide the mod buttons, not
/// break the page.
pub async fn fetch_can_moderate(session: &str, chat_id: &str) -> bool {
    if session.is_empty() || chat_id.is_empty() {
        return false;
    }
    let read = async {
        let mut o = FetchOpts::new(0).header("Accept", "text/event-stream").header("Cookie", &format!("u_s={session}"));
        o.method = "";
        let res = fetch_from_rumble(&format!("/chat/api/chat/{}/stream", enc(chat_id)), o).await.ok()?;
        if res.status().as_u16() != 200 {
            return None;
        }
        let mut stream = res.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk.ok()?);
            for frame in drain_frames(&mut buf) {
                for line in frame.split('\n') {
                    let Some(rest) = line.strip_prefix("data:") else { continue };
                    if let Ok(ev) = serde_json::from_str::<Value>(js_trim(rest)) {
                        if ev["type"] == "init" {
                            return Some(truthy(&ev["data"]["can_moderate"]));
                        }
                    }
                    // keep reading: a partial frame is not a verdict
                }
            }
            // The init frame is always first; this much data without one means the shape changed.
            if buf.len() > 2_000_000 {
                return None;
            }
        }
        None
    };
    // Dropping the future on timeout hangs up the connection.
    tokio::time::timeout(std::time::Duration::from_secs(15), read).await.ok().flatten().unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<(String, String, Option<String>)> {
        parse_rumble_source(s).map(|x| (x.kind.to_string(), x.id, x.space))
    }
    fn norm(s: &str) -> Option<String> {
        parse_rumble_source(s).map(|x| if x.kind == "video" { x.id } else { format!("{}/{}", x.space.unwrap(), x.id.to_lowercase()) })
    }

    #[test]
    fn source_forms() {
        let ch = |s: &str, id: &str| Some(("channel".to_string(), id.to_string(), Some(s.to_string())));
        let vid = |id: &str| Some(("video".to_string(), id.to_string(), None));
        assert_eq!(p("Foo"), ch("c", "Foo"));
        assert_eq!(p("c/Foo"), ch("c", "Foo"));
        assert_eq!(p("user/Foo"), ch("user", "Foo"));
        assert_eq!(p("/c/Foo/"), ch("c", "Foo"));
        assert_eq!(p("rumble.com/c/Foo"), ch("c", "Foo"));
        assert_eq!(p("https://rumble.com/c/Foo?x=1#y"), ch("c", "Foo"));
        assert_eq!(p("https://www.rumble.com/user/Foo/videos"), ch("user", "Foo"));
        assert_eq!(p("HTTP://RUMBLE.COM/c/Foo"), ch("c", "Foo"));
        assert_eq!(p("https://rumble.com/v7eh2s2-some-title.html"), vid("v7eh2s2"));
        assert_eq!(p("rumble.com/V7EH2S2-Title.html?e9s=x"), vid("v7eh2s2"));
        assert_eq!(p("v7capuk"), vid("v7capuk"));
        assert_eq!(p("embed/v7capuk"), vid("v7capuk"));
        assert_eq!(p("https://rumble.com/embed/v7capuk/"), vid("v7capuk"));
        assert_eq!(p("  Foo.Bar-1  "), ch("c", "Foo.Bar-1"));
        assert_eq!(p(""), None);
        assert_eq!(p("https://rumble.com/"), None);
        assert_eq!(p("foo bar"), None);
        assert_eq!(p("a/b/c"), None);
        assert_eq!(p("C/Foo"), None); // the prefix is case-sensitive in the JS too
        assert_eq!(norm("https://rumble.com/c/FooBar"), Some("c/foobar".into()));
        assert_eq!(norm("user/Foo"), Some("user/foo".into()));
        assert_eq!(norm("v7capuk"), Some("v7capuk".into()));
        let s = parse_rumble_source("user/Foo").unwrap();
        assert_eq!(rumble_source_key(&s), "user:foo");
        assert_eq!(rumble_source_url(&s), "https://rumble.com/user/Foo");
        let v = parse_rumble_source("v7capuk").unwrap();
        assert_eq!((rumble_source_key(&v).as_str(), rumble_source_url(&v).as_str()), ("v:v7capuk", "https://rumble.com/v7capuk"));
    }

    #[test]
    fn video_and_category() {
        let body = json!({"vid": 123, "live": 2, "title": "T", "author": {"name": "A", "url": "https://rumble.com/c/A"}, "l": "/v1-x.html", "a": {"pause_ads_params": {"categories": ["Gaming", "world-of-warcraft", " ", 5]}}});
        let r = video_from_json(&body, "v1").unwrap();
        assert_eq!((r.live, r.chat_id.as_deref(), r.title.as_str(), r.category.as_str(), r.url.as_str()), (true, Some("123"), "T", "Gaming · world of warcraft", "https://rumble.com/v1-x.html"));
        assert!(!video_from_json(&json!({"vid": 1, "live": 0}), "v1").unwrap().live);
        assert!(video_from_json(&json!({}), "v1").is_err());
        assert_eq!(rumble_category_of(&json!({})), "");
        assert_eq!(video_from_json(&json!({"vid": 1}), "vabcde").unwrap().url, "https://rumble.com/vabcde");
    }

    #[test]
    fn channel_pages() {
        let url = "https://rumble.com/c/Foo";
        let items = r#"<script type="application/json">{"unrelated":1}</script><script type="application/json">{"items":[{"id":1,"live":false},{"id":2,"live":true,"title":"low","watching_now":5},{"id":3,"livestream_status":2,"title":"high","watching_now":50,"permalink_id":"v3"}]}</script>"#;
        let ChannelPage::Done(r) = channel_from_html(items, "Foo", url).unwrap() else { panic!() };
        assert_eq!((r.live, r.chat_id.as_deref(), r.title.as_str(), r.url.as_str(), r.watching), (true, Some("3"), "high", "https://rumble.com/v3", Some(50)));
        let ChannelPage::Done(off) = channel_from_html(r#"<script type="application/json">{"items":[{"id":1,"live":false}]}</script>"#, "Foo", url).unwrap() else { panic!() };
        assert!(!off.live && off.chat_id.is_none());
        // no JSON: the listing section only, never the sidebar
        let html = r#"<div class="channel-listing__container"><a href="/v7eh2s2-a.html">x</a><a href="/v7eh2s2-b.html">x</a><a href="/vabc123-c.html">x</a></section><a href="/vsidebar-z.html">"#;
        let ChannelPage::Permalinks(pl) = channel_from_html(html, "Foo", url).unwrap() else { panic!() };
        assert_eq!(pl, vec!["v7eh2s2", "vabc123"]);
        assert!(channel_from_html("<html>nope</html>", "Foo", url).is_err());
        assert_eq!(listing_permalinks("", 6), Vec::<String>::new());
        let many: String = (0..10).map(|i| format!(r#"href="/v{i}abc-x.html""#)).collect();
        assert_eq!(listing_permalinks(&many, 6).len(), 6);
    }

    #[test]
    fn emotes_and_rows() {
        let html = r#"RumbleChat({emotes:[{name:":r+cooldoge:",is_subs_only:false,position:1,file:"a.png"},{name:":sub:",is_subs_only:true,position:2,file:"b.png"}]})"#;
        assert_eq!(emotes_from_html(html), json!({":r+cooldoge:": {"file": "a.png", "subsOnly": false}, ":sub:": {"file": "b.png", "subsOnly": true}}));
        assert_eq!(emotes_from_html("nothing"), json!({}));

        let mut users = HashMap::new();
        users.insert("7".to_string(), json!({"id": 7, "username": "bob", "color": "#f00", "image.1": "u.png", "badges": ["moderator"], "is_follower": true}));
        let mut channels = HashMap::new();
        channels.insert("9".to_string(), json!({"id": 9, "username": "BobTV", "image.1": "c.png"}));
        let raw = json!({"id": 55, "user_id": 7, "channel_id": 9, "text": "hi", "time": "2026-11-05T12:00:00+00:00", "type": "regular", "rant": {"price_cents": 500, "duration": 60}});
        let m = to_rumble_chat_msg(&raw, &users, &channels);
        assert_eq!(m["id"], "55");
        assert_eq!(m["user"], json!({"id": "7", "login": "bob", "name": "BobTV", "color": "#f00", "avatar": "c.png"}));
        assert_eq!(m["badges"], json!(["moderator"]));
        assert_eq!(m["channel"], json!({"id": "9", "name": "BobTV", "avatar": "c.png"}));
        assert_eq!(m["rant"], json!({"cents": 500, "duration": 60, "expiresAt": 0}));
        assert_eq!((m["notice"].clone(), m["isFollower"].clone()), (Value::Null, json!(true)));
        let lone = to_rumble_chat_msg(&json!({"id": "x", "user_id": 1, "text": null, "type": "raid"}), &users, &channels);
        assert_eq!((lone["user"]["name"].clone(), lone["text"].clone(), lone["notice"].clone()), (json!(""), json!(""), json!("raid")));
    }

    #[test]
    fn sse_frames() {
        let mut buf = b"data: {\"a\":1}\n\n: hb\n\ndata: par".to_vec();
        let f = drain_frames(&mut buf);
        assert_eq!(f, vec!["data: {\"a\":1}", ": hb"]);
        assert_eq!(buf, b"data: par");
    }
}

/// `rumbleFetch(url, init)` against ONE given host (the SSE reader picks its own host and keeps its own
/// health bookkeeping): the honest UA + Accept-Language first, then the caller's headers. No retry.
pub async fn rumble_fetch(url: &str, mut init: FetchOpts) -> Result<reqwest::Response, net::FetchError> {
    let mut headers = vec![("User-Agent".to_string(), super::super::ua::get_user_agent()), ("Accept-Language".to_string(), "en-US,en;q=0.9".to_string())];
    headers.append(&mut init.headers);
    init.headers = headers;
    net::fetch(url, init).await
}
