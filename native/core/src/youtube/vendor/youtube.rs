//! Port of src/main/youtube/vendor/youtube.js (vendored from modtools.agu.lol render/lib/youtube.js).
//! InnerTube, the calls youtube.com itself makes: one watch-page fetch resolves the live status and carries
//! the InnerTube key, the client version and the first chat continuation; then `get_live_chat` polls.
//! Everything here is pure or takes its fetch inputs explicitly, so the source in ../mod.rs owns timers and
//! lifecycle. The caller's honest product UA is sent (never a browser string).
//!
//! Differences from the JS: the `signal` option is gone (a source aborts its tasks, see net::Jobs);
//! `String::match`-style regexes use the `regex` crate with explicit ASCII classes for `\w`.
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::net::{self, FetchOpts};
use crate::util::{js_str_or_empty, js_string, js_to_number, now_ms, truthy};

// SOCS is YouTube's consent cookie. Without it a channel page can answer with a consent bounce. Standard
// practice (yt-dlp does the same); it is a consent flag, not an identity.
const CONSENT_COOKIE: &str = "SOCS=CAI";

// ---------------------------------------------------------------------------
// Source parsing
// ---------------------------------------------------------------------------

static VIDEO_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap());
static CHANNEL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^UC[A-Za-z0-9_-]{22}$").unwrap());
static HANDLE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@[A-Za-z0-9_.-]{1,60}$").unwrap());
static YT_HOST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(^|\.)(youtube\.com|youtu\.be)$").unwrap());
static BARE_WORD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]{1,60}$").unwrap());

#[derive(Clone, Debug, PartialEq)]
pub struct YtSource {
    /// "video" | "channel" | "handle"
    pub kind: &'static str,
    pub id: String,
}

fn src(kind: &'static str, id: &str) -> Option<YtSource> {
    Some(YtSource { kind, id: id.to_string() })
}

/// Accepts what someone would actually paste: a handle, a channel URL, a video link, or any of those bare.
/// A VIDEO source pins one broadcast; a channel or handle follows whatever that channel is streaming.
pub fn parse_youtube_source(input: &str) -> Option<YtSource> {
    let mut raw = crate::util::js_trim(input).to_string();
    if raw.is_empty() {
        return None;
    }
    // "@LofiGirl/live" is how people paste a handle copied off a /live link.
    if raw.starts_with('@') && raw.contains('/') {
        raw = raw.split('/').next().unwrap_or("").to_string();
    }

    if CHANNEL_RE.is_match(&raw) {
        return src("channel", &raw);
    }
    if HANDLE_RE.is_match(&raw) {
        return src("handle", &raw.to_lowercase());
    }
    if VIDEO_RE.is_match(&raw) {
        return src("video", &raw);
    }

    let lower = raw.to_lowercase();
    let with_scheme = if lower.starts_with("http://") || lower.starts_with("https://") { raw.clone() } else { format!("https://{raw}") };
    let u = url::Url::parse(&with_scheme).ok()?;
    let host = u.host_str().unwrap_or("").to_string();
    if !YT_HOST_RE.is_match(&host) {
        // A bare word ("lofigirl") is most likely a handle typed without its @. Only when it is not a URL.
        return if BARE_WORD_RE.is_match(&raw) && !raw.contains('.') { src("handle", &format!("@{}", raw.to_lowercase())) } else { None };
    }

    let segs: Vec<&str> = u.path_segments().map(|p| p.filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    if host.to_lowercase().ends_with("youtu.be") {
        let id = segs.first().copied().unwrap_or("");
        return if VIDEO_RE.is_match(id) { src("video", id) } else { None };
    }

    if let Some((_, v)) = u.query_pairs().find(|(k, _)| k == "v") {
        if VIDEO_RE.is_match(&v) {
            return src("video", &v);
        }
    }

    let s0 = segs.first().copied().unwrap_or("");
    let s1 = segs.get(1).copied().unwrap_or("");
    if s0 == "channel" && CHANNEL_RE.is_match(s1) {
        return src("channel", s1);
    }
    if (s0 == "live" || s0 == "shorts" || s0 == "embed") && VIDEO_RE.is_match(s1) {
        return src("video", s1);
    }
    if s0.starts_with('@') && HANDLE_RE.is_match(s0) {
        return src("handle", &s0.to_lowercase());
    }
    // /c/Name and /user/Name are legacy vanity URLs with no stable id in them; handled as handles.
    if (s0 == "c" || s0 == "user") && !s1.is_empty() {
        return src("handle", &format!("@{}", s1.to_lowercase()));
    }
    None
}

/// The string a normalized source is typed back as: normalizeChannel(sourceString(parse(x))) is stable.
pub fn source_string(s: &YtSource) -> String {
    s.id.clone()
}

pub fn youtube_source_url(s: &YtSource) -> String {
    match s.kind {
        "video" => format!("https://www.youtube.com/watch?v={}", s.id),
        "channel" => format!("https://www.youtube.com/channel/{}", s.id),
        _ => format!("https://www.youtube.com/{}", s.id),
    }
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async fn yt_fetch(url: &str, user_agent: &str, method: &'static str, body: Option<Value>) -> Result<reqwest::Response, net::FetchError> {
    let mut o = FetchOpts::new(15_000).method(method).header("User-Agent", user_agent).header("Accept-Language", "en-US,en").header("Cookie", CONSENT_COOKIE);
    if let Some(b) = body {
        o = o.header("content-type", "application/json").body(b.to_string());
    }
    net::fetch(url, o).await
}

/// `Object.values(c)[0]` of the first continuation entry that has a `continuation`.
fn first_continuation(list: &Value) -> Option<Value> {
    for c in list.as_array().into_iter().flatten() {
        let v = c.as_object().and_then(|o| o.values().next());
        if let Some(v) = v {
            if truthy(&v["continuation"]) {
                return Some(v.clone());
            }
        }
    }
    None
}

fn text_of(node: &Value) -> String {
    if !truthy(node) {
        return String::new();
    }
    if truthy(&node["simpleText"]) {
        return js_string(&node["simpleText"]);
    }
    let joined: String = node["runs"].as_array().into_iter().flatten().map(|r| js_str_or_empty(Some(&r["text"]))).collect();
    crate::util::js_trim(&joined).to_string()
}

/// The JSON object assigned right after `marker` in a page, found by brace matching (the page inlines it
/// as a JS assignment, not as a JSON script block). Strings are skipped so braces inside text don't count.
pub fn json_after(html: &str, marker: &str) -> Option<Value> {
    let i = html.find(marker)?;
    let start = i + marker.len() + html[i + marker.len()..].find('{')?;
    let b = html.as_bytes();
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for k in start..b.len() {
        let c = b[k];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return serde_json::from_str(&html[start..=k]).ok();
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Resolution + bootstrap, from one watch page
// ---------------------------------------------------------------------------

/// What `readWatchPage` returns. Empty strings stand for the JS `undefined` / `""` / `null` of each field.
#[derive(Clone, Debug, Default)]
pub struct WatchPage {
    pub exists: bool,
    pub blocked: bool,
    pub live: bool,
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub channel_id: String,
    pub key: String,
    pub client_version: String,
    pub continuation: String,
    pub timeout_ms: u64,
    /// YouTube's own words when chat is unavailable ("Chat is disabled for this live stream."), surfaced verbatim.
    pub notice: Option<String>,
    pub error: Option<String>,
}

fn capture(re: &Regex, html: &str) -> String {
    re.captures(html).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_default()
}

static KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""INNERTUBE_API_KEY":"([^"]+)""#).unwrap());
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""INNERTUBE_CLIENT_VERSION":"([^"]+)""#).unwrap());

/// Which video is this source live on, and everything needed to read its chat.
///
/// /@handle/live and /channel/<id>/live answer with the watch page of the current live video when there is
/// one, and with the channel's browse page when there is not. So the page's own data decides: a player
/// response whose videoDetails.isLive is true is a live stream, anything else is "not live". Presence of a
/// video id alone proves nothing (a channel's /live can show its last, ended stream).
pub async fn read_watch_page(s: &YtSource, user_agent: &str) -> Result<WatchPage, String> {
    let url = if s.kind == "video" { youtube_source_url(s) } else { format!("{}/live", youtube_source_url(s)) };
    let r = yt_fetch(&url, user_agent, "", None).await.map_err(|e| e.message)?;
    if r.status().as_u16() == 404 {
        return Ok(WatchPage { exists: false, ..Default::default() });
    }
    if !r.status().is_success() {
        return Err(format!("YouTube answered {}", r.status().as_u16()));
    }
    // A consent wall or bot check lands off youtube.com; say so rather than reading it as "offline".
    let host = r.url().host_str().unwrap_or("").to_string();
    if !Regex::new(r"(?i)(^|\.)youtube\.com$").unwrap().is_match(&host) {
        return Ok(WatchPage { exists: true, blocked: true, ..Default::default() });
    }
    let html = net::text(r).await.map_err(|e| e.message)?;
    Ok(parse_watch_page(&html))
}

/// The pure half of `readWatchPage`.
pub fn parse_watch_page(html: &str) -> WatchPage {
    let key = capture(&KEY_RE, html);
    let client_version = capture(&VERSION_RE, html);
    let player = json_after(html, "var ytInitialPlayerResponse = ").or_else(|| json_after(html, "window[\"ytInitialPlayerResponse\"] = "));
    let data = json_after(html, "var ytInitialData = ").or_else(|| json_after(html, "window[\"ytInitialData\"] = "));
    let Some(data) = data.filter(|_| !key.is_empty()) else {
        return WatchPage { exists: true, blocked: true, error: Some("YouTube returned a page this client does not recognise".into()), ..Default::default() };
    };
    let player = player.unwrap_or(Value::Null);

    let vd = &player["videoDetails"];
    let channel_meta = &data["metadata"]["channelMetadataRenderer"];
    if !truthy(&vd["videoId"]) {
        // A browse page: a channel that is not streaming.
        return WatchPage {
            exists: true,
            author: js_str_or_empty(Some(&channel_meta["title"])),
            channel_id: js_str_or_empty(Some(&channel_meta["externalId"])),
            key,
            client_version,
            ..Default::default()
        };
    }

    let base = WatchPage {
        exists: true,
        video_id: js_string(&vd["videoId"]),
        title: js_str_or_empty(Some(&vd["title"])),
        author: js_str_or_empty(Some(&vd["author"])),
        channel_id: js_str_or_empty(Some(&vd["channelId"])),
        key,
        client_version,
        ..Default::default()
    };
    let micro = &player["microformat"]["playerMicroformatRenderer"];
    let live = vd["isLive"] == Value::Bool(true) || truthy(&micro["liveBroadcastDetails"]["isLiveNow"]);
    if !live {
        return base;
    }

    let bar = &data["contents"]["twoColumnWatchNextResults"]["conversationBar"];
    let lc = &bar["liveChatRenderer"];
    let none = "YouTube didn't return a chat for this stream".to_string();
    if !truthy(lc) || truthy(&lc["isReplay"]) {
        // YouTube says this in plain English; passing it through beats a blank column with no explanation.
        let msg = text_of(&bar["conversationBarRenderer"]["availabilityMessage"]["messageRenderer"]["text"]);
        return WatchPage { live: true, notice: Some(if msg.is_empty() { none } else { msg }), ..base };
    }
    let Some(cont) = first_continuation(&lc["continuations"]) else {
        return WatchPage { live: true, notice: Some(none), ..base };
    };
    WatchPage {
        live: true,
        continuation: js_string(&cont["continuation"]),
        timeout_ms: timeout_of(&cont),
        notice: None,
        ..base
    }
}

fn timeout_of(cont: &Value) -> u64 {
    let n = js_to_number(Some(&cont["timeoutMs"]));
    if n.is_nan() || n <= 0.0 {
        0
    } else {
        n as u64
    }
}

/// What a poll carries to the next one.
#[derive(Clone, Debug)]
pub struct ChatState {
    pub key: String,
    pub client_version: String,
    pub continuation: String,
    pub video_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct Metadata {
    pub title: String,
    pub viewers: Option<i64>,
}

/// The stream's title and live viewer count. updated_metadata is what youtube.com's own watch page calls
/// to refresh those; ~7 KB. Called far less often than YouTube suggests (its 5s is a ticking counter).
pub async fn fetch_metadata(c: &ChatState, user_agent: &str) -> Result<Metadata, String> {
    let body = json!({"context": {"client": {"clientName": "WEB", "clientVersion": c.client_version}}, "videoId": c.video_id});
    let r = yt_fetch(&format!("https://www.youtube.com/youtubei/v1/updated_metadata?key={}&prettyPrint=false", c.key), user_agent, "POST", Some(body)).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("updated_metadata returned {}", r.status().as_u16()));
    }
    let j = net::json(r).await.map_err(|e| e.message)?;
    Ok(metadata_from_json(&j))
}

pub fn metadata_from_json(j: &Value) -> Metadata {
    let mut out = Metadata::default();
    for a in j["actions"].as_array().into_iter().flatten() {
        let vc = &a["updateViewershipAction"]["viewCount"]["videoViewCountRenderer"];
        if truthy(vc) {
            // "10,107 watching now" -- separators are locale-shaped, so strip everything that isn't a digit.
            let digits: String = text_of(&vc["viewCount"]).chars().filter(|c| c.is_ascii_digit()).collect();
            out.viewers = if digits.is_empty() { None } else { digits.parse::<i64>().ok() };
        }
        if truthy(&a["updateTitleAction"]) {
            out.title = text_of(&a["updateTitleAction"]["title"]);
        }
    }
    out
}

pub struct PollResult {
    pub actions: Vec<Value>,
    pub continuation: String,
    pub timeout_ms: u64,
}

/// One poll. Returns the next continuation, or None when the chat has ended.
pub async fn poll_chat(c: &ChatState, user_agent: &str) -> Result<Option<PollResult>, String> {
    let body = json!({"context": {"client": {"clientName": "WEB", "clientVersion": c.client_version}}, "continuation": c.continuation});
    let r = yt_fetch(&format!("https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?key={}&prettyPrint=false", c.key), user_agent, "POST", Some(body)).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("live chat poll returned {}", r.status().as_u16()));
    }
    let j = net::json(r).await.map_err(|e| e.message)?;
    Ok(poll_from_json(&j))
}

pub fn poll_from_json(j: &Value) -> Option<PollResult> {
    let cc = &j["continuationContents"]["liveChatContinuation"];
    // No continuationContents means the chat is over (or the token expired); the caller must re-resolve.
    if !truthy(cc) {
        return None;
    }
    let next = first_continuation(&cc["continuations"]);
    Some(PollResult {
        actions: cc["actions"].as_array().cloned().unwrap_or_default(),
        continuation: next.as_ref().map(|n| js_string(&n["continuation"])).unwrap_or_default(),
        timeout_ms: next.as_ref().map(timeout_of).unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Message normalization (unchanged from the site)
// ---------------------------------------------------------------------------

/// ARGB integers, which is what YouTube ships for Super Chat tiers. Only a real JSON number counts
/// (`Number.isFinite` does not coerce).
fn argb_to_css(v: &Value) -> String {
    let Some(n) = v.as_f64().filter(|n| n.is_finite()) else { return String::new() };
    let u = (n.trunc().rem_euclid(4_294_967_296.0)) as u32; // `n >>> 0`
    let a = ((u >> 24) & 255) as f64 / 255.0;
    format!("rgba({}, {}, {}, {:.3})", (u >> 16) & 255, (u >> 8) & 255, u & 255, a)
}

/// Message text arrives as "runs" -- alternating text and emoji objects, where each emoji carries its own
/// image URL, so the parts are shipped ready-rendered. `text` is the plain-text form.
fn parse_runs(message: &Value) -> (Vec<Value>, String) {
    let mut parts = Vec::new();
    let mut text = String::new();
    for run in message["runs"].as_array().into_iter().flatten() {
        if truthy(&run["text"]) {
            let t = js_string(&run["text"]);
            text.push_str(&t);
            parts.push(json!({"t": "text", "v": t}));
            continue;
        }
        let e = &run["emoji"];
        if !truthy(e) {
            continue;
        }
        let url = last_thumb(&e["image"]["thumbnails"]);
        let label = {
            let s = &e["shortcuts"][0];
            let l = if truthy(s) { js_string(s) } else { js_str_or_empty(Some(&e["emojiId"])) };
            l
        };
        if !url.is_empty() {
            parts.push(json!({"t": "emoji", "u": url, "a": label, "custom": truthy(&e["isCustomEmoji"])}));
        }
        text.push_str(&label);
    }
    (parts, text)
}

fn last_thumb(thumbs: &Value) -> String {
    thumbs.as_array().and_then(|a| a.last()).map(|t| js_str_or_empty(Some(&t["url"]))).unwrap_or_default()
}

fn parse_badges(list: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for b in list.as_array().into_iter().flatten() {
        let r = &b["liveChatAuthorBadgeRenderer"];
        if !truthy(r) {
            continue;
        }
        out.push(json!({
            "label": js_str_or_empty(Some(&r["tooltip"])),
            // Members get real artwork; moderator/owner/verified come as icon types we style ourselves.
            "icon": js_str_or_empty(Some(&r["icon"]["iconType"])),
            "url": last_thumb(&r["customThumbnail"]["thumbnails"]),
        }));
    }
    out.truncate(6);
    out
}

/// "@someone" -> "someone". A name that is only "@" is left alone.
fn strip_handle(name: &str) -> String {
    if name.chars().count() > 1 && name.starts_with('@') {
        name[1..].to_string()
    } else {
        name.to_string()
    }
}

/// One InnerTube action -> a chat row, or None for the many action types that are not messages.
pub fn to_youtube_chat_msg(action: &Value) -> Option<Value> {
    let item = &action["addChatItemAction"]["item"];
    if !truthy(item) {
        return None;
    }
    let text = &item["liveChatTextMessageRenderer"];
    let paid = &item["liveChatPaidMessageRenderer"];
    let sticker = &item["liveChatPaidStickerRenderer"];
    let member = &item["liveChatMembershipItemRenderer"];
    let r = [text, paid, sticker, member].into_iter().find(|v| truthy(v))?;

    let ts = js_to_number(r.get("timestampUsec"));
    let ts_ms = if ts.is_finite() { (ts / 1000.0 + 0.5).floor() } else { now_ms() as f64 }; // Math.round
    let msg = if truthy(&r["message"]) { &r["message"] } else { &r["headerSubtext"] };
    let (parts, plain) = parse_runs(msg);
    let pr = if truthy(paid) { paid } else { sticker };

    Some(json!({
        "platform": "youtube",
        "id": js_str_or_empty(Some(&r["id"])),
        "ts": net::f64_value(ts_ms),
        "user": {
            // The channel id is the only identifier YouTube moderation can act on.
            "id": js_str_or_empty(Some(&r["authorExternalChannelId"])),
            // YouTube gives the author as a HANDLE ("@someone"); the leading @ is stripped so a merged timeline
            // reads the same across platforms.
            "name": strip_handle(&js_str_or_empty(Some(&r["authorName"]["simpleText"]))),
            "avatar": last_thumb(&r["authorPhoto"]["thumbnails"]),
        },
        "badges": parse_badges(&r["authorBadges"]),
        // what moderation asks YouTube's context menu with; null when the item has none
        "menu": r["contextMenuEndpoint"]["liveChatItemContextMenuEndpoint"]["params"].as_str().filter(|p| !p.is_empty()).map(|p| json!(p)).unwrap_or(Value::Null),
        "parts": parts,
        "text": plain,
        // Super Chats and Super Stickers carry their own tier colour, passed through rather than re-derived.
        "paid": if truthy(pr) {
            json!({
                "amount": js_str_or_empty(Some(&pr["purchaseAmountText"]["simpleText"])),
                "bg": argb_to_css(&pr["bodyBackgroundColor"]),
                "headerBg": argb_to_css(&pr["headerBackgroundColor"]),
                "sticker": truthy(sticker),
            })
        } else { Value::Null },
        // New members and milestones render as notices rather than chat lines.
        "notice": if truthy(member) { json!("membership") } else { Value::Null },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Option<(&'static str, String)> {
        parse_youtube_source(s).map(|x| (x.kind, x.id))
    }

    #[test]
    fn source_forms() {
        let ch = "UCabcdefghijklmnopqrstuv"; // UC + 22
        assert_eq!(ch.len(), 24);
        assert_eq!(p(ch), Some(("channel", ch.into())));
        assert_eq!(p("@LofiGirl"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("@LofiGirl/live"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("lofigirl"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("  lofigirl  "), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("youtube.com/watch?v=dQw4w9WgXcQ&t=5"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("https://youtu.be/dQw4w9WgXcQ?t=1"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("https://www.youtube.com/live/dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("https://www.youtube.com/shorts/dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("https://www.youtube.com/embed/dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p(&format!("https://www.youtube.com/channel/{ch}")), Some(("channel", ch.into())));
        assert_eq!(p("https://www.youtube.com/@LofiGirl"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("https://www.youtube.com/@LofiGirl/live"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("https://www.youtube.com/c/LofiGirl"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("https://www.youtube.com/user/LofiGirl"), Some(("handle", "@lofigirl".into())));
        assert_eq!(p("https://m.youtube.com/watch?v=dQw4w9WgXcQ"), Some(("video", "dQw4w9WgXcQ".into())));
        assert_eq!(p("https://www.youtube.com/"), None);
        assert_eq!(p("https://www.youtube.com/watch?v=short"), None);
        assert_eq!(p("https://example.com/watch?v=dQw4w9WgXcQ"), None);
        assert_eq!(p("example.com"), None);
        assert_eq!(p("foo bar"), None);
        assert_eq!(p(""), None);
        assert_eq!(source_string(&parse_youtube_source("@LofiGirl").unwrap()), "@lofigirl");
        assert_eq!(youtube_source_url(&parse_youtube_source("@LofiGirl").unwrap()), "https://www.youtube.com/@lofigirl");
    }

    #[test]
    fn brace_matching_skips_strings() {
        let html = r#"x var a = {"k":"} {","n":{"z":1}}; var b = 2"#;
        assert_eq!(json_after(html, "var a = "), Some(json!({"k": "} {", "n": {"z": 1}})));
        assert_eq!(json_after(html, "var nope = "), None);
        assert_eq!(json_after("var a = {\"k\":", "var a = "), None);
    }

    #[test]
    fn watch_page_states() {
        let wrap = |player: &str, data: &str| format!(r#"<script>"INNERTUBE_API_KEY":"KEY","INNERTUBE_CLIENT_VERSION":"2.1"; var ytInitialPlayerResponse = {player}; var ytInitialData = {data};</script>"#);
        // browse page: not streaming
        let off = parse_watch_page(&wrap(r#"{}"#, r#"{"metadata":{"channelMetadataRenderer":{"title":"Chan","externalId":"UC1"}}}"#));
        assert!(off.exists && !off.blocked && !off.live && off.video_id.is_empty());
        assert_eq!((off.author.as_str(), off.channel_id.as_str(), off.key.as_str()), ("Chan", "UC1", "KEY"));
        // unrecognised page
        let bad = parse_watch_page("<html>consent</html>");
        assert!(bad.blocked && bad.error.is_some());
        // live with chat
        let live_data = r#"{"contents":{"twoColumnWatchNextResults":{"conversationBar":{"liveChatRenderer":{"continuations":[{"timedContinuationData":{"continuation":"C1","timeoutMs":5000}}]}}}}}"#;
        let live = parse_watch_page(&wrap(r#"{"videoDetails":{"videoId":"v1","isLive":true,"title":"T","author":"A","channelId":"UC1"}}"#, live_data));
        assert!(live.live);
        assert_eq!((live.video_id.as_str(), live.continuation.as_str(), live.timeout_ms, live.client_version.as_str()), ("v1", "C1", 5000, "2.1"));
        // live via microformat only
        let micro = parse_watch_page(&wrap(r#"{"videoDetails":{"videoId":"v1"},"microformat":{"playerMicroformatRenderer":{"liveBroadcastDetails":{"isLiveNow":true}}}}"#, live_data));
        assert!(micro.live);
        // ended stream: a video id proves nothing
        let ended = parse_watch_page(&wrap(r#"{"videoDetails":{"videoId":"v1","isLive":false}}"#, live_data));
        assert!(!ended.live && ended.continuation.is_empty());
        // live without chat: YouTube's own words
        let nochat = parse_watch_page(&wrap(r#"{"videoDetails":{"videoId":"v1","isLive":true}}"#, r#"{"contents":{"twoColumnWatchNextResults":{"conversationBar":{"conversationBarRenderer":{"availabilityMessage":{"messageRenderer":{"text":{"runs":[{"text":"Chat is disabled for this live stream."}]}}}}}}}}"#));
        assert!(nochat.live && nochat.continuation.is_empty());
        assert_eq!(nochat.notice.as_deref(), Some("Chat is disabled for this live stream."));
        let plain = parse_watch_page(&wrap(r#"{"videoDetails":{"videoId":"v1","isLive":true}}"#, r#"{"contents":{}}"#));
        assert_eq!(plain.notice.as_deref(), Some("YouTube didn't return a chat for this stream"));
    }

    #[test]
    fn poll_metadata_and_rows() {
        let poll = poll_from_json(&json!({"continuationContents": {"liveChatContinuation": {"actions": [{}, {}], "continuations": [{"invalidationContinuationData": {"continuation": "N", "timeoutMs": 3000}}]}}})).unwrap();
        assert_eq!((poll.actions.len(), poll.continuation.as_str(), poll.timeout_ms), (2, "N", 3000));
        assert!(poll_from_json(&json!({})).is_none());
        let end = poll_from_json(&json!({"continuationContents": {"liveChatContinuation": {}}})).unwrap();
        assert_eq!((end.continuation.as_str(), end.timeout_ms), ("", 0));

        let m = metadata_from_json(&json!({"actions": [{"updateViewershipAction": {"viewCount": {"videoViewCountRenderer": {"viewCount": {"simpleText": "10,107 watching now"}}}}}, {"updateTitleAction": {"title": {"runs": [{"text": "New "}, {"text": "title"}]}}}]}));
        assert_eq!((m.title.as_str(), m.viewers), ("New title", Some(10107)));
        assert_eq!(metadata_from_json(&json!({})).viewers, None);

        let action = json!({"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {
            "id": "m1", "timestampUsec": "1794052800123456", "authorExternalChannelId": "UCx", "authorName": {"simpleText": "@bob"},
            "authorPhoto": {"thumbnails": [{"url": "a"}, {"url": "b"}]},
            "message": {"runs": [{"text": "hi "}, {"emoji": {"emojiId": "e", "shortcuts": [":smile:"], "isCustomEmoji": true, "image": {"thumbnails": [{"url": "e1"}, {"url": "e2"}]}}}]},
            "contextMenuEndpoint": {"liveChatItemContextMenuEndpoint": {"params": "MENU"}},
            "authorBadges": [{"liveChatAuthorBadgeRenderer": {"tooltip": "Moderator", "icon": {"iconType": "MODERATOR"}}}]}}}});
        let row = to_youtube_chat_msg(&action).unwrap();
        assert_eq!(row["id"], "m1");
        assert_eq!(row["ts"], 1794052800123i64);
        assert_eq!(row["user"], json!({"id": "UCx", "name": "bob", "avatar": "b"}));
        assert_eq!(row["text"], "hi :smile:");
        assert_eq!(row["menu"], "MENU");
        assert_eq!(row["parts"], json!([{"t": "text", "v": "hi "}, {"t": "emoji", "u": "e2", "a": ":smile:", "custom": true}]));
        assert_eq!(row["badges"], json!([{"label": "Moderator", "icon": "MODERATOR", "url": ""}]));
        assert_eq!((&row["paid"], &row["notice"]), (&Value::Null, &Value::Null));

        let paid = to_youtube_chat_msg(&json!({"addChatItemAction": {"item": {"liveChatPaidMessageRenderer": {"id": "p", "timestampUsec": "1000", "purchaseAmountText": {"simpleText": "$5.00"}, "bodyBackgroundColor": 4278248959u32, "headerBackgroundColor": 4278237396u32, "message": {"runs": [{"text": "x"}]}}}}})).unwrap();
        assert_eq!(paid["paid"], json!({"amount": "$5.00", "bg": "rgba(0, 229, 255, 1.000)", "headerBg": "rgba(0, 184, 212, 1.000)", "sticker": false}));
        let member = to_youtube_chat_msg(&json!({"addChatItemAction": {"item": {"liveChatMembershipItemRenderer": {"id": "g", "headerSubtext": {"runs": [{"text": "Welcome"}]}}}}})).unwrap();
        assert_eq!((&member["notice"], &member["text"]), (&json!("membership"), &json!("Welcome")));
        assert_eq!(paid["menu"], Value::Null);
        assert!(to_youtube_chat_msg(&json!({"removeChatItemAction": {}})).is_none());
        assert!(to_youtube_chat_msg(&json!({"addChatItemAction": {"item": {"liveChatViewerEngagementMessageRenderer": {}}}})).is_none());
        assert_eq!(strip_handle("@"), "@");
        assert_eq!(strip_handle("@a"), "a");
    }
}
