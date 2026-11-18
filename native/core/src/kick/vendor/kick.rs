//! Port of src/main/kick/vendor/kick.js (adapted from modtools.agu.lol render/lib/kick.js and kickApi.js).
//! Public, unauthenticated, read-only Kick calls and the row shape. Every request carries the honest product
//! UA: never a browser string (measured: no UA -> 200, an honest product UA -> 200, a Chrome UA -> 403).
//!
//! Differences from the JS: the `signal` option is gone (a source aborts its tasks instead, see net::Jobs).
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde_json::{json, Map, Value};

use crate::net::{self, num_or0, str_nullish, FetchOpts};
use crate::util::{js_str_or_empty, js_string, js_trim, now_ms, parse_rfc3339_ms, truthy};

/// `/^[A-Za-z0-9_-]{1,25}$/`
fn slug_ok(s: &str) -> bool {
    (1..=25).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// kick.com/<slug>, or a bare slug. Kick's URLs hyphenate what a username spells with underscores
/// ("some_user" is kick.com/some-user), so underscores are normalised to hyphens for the lookup.
pub fn parse_kick_source(input: &str) -> Option<String> {
    let raw = js_trim(input);
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_lowercase();
    let is_http = lower.starts_with("http://") || lower.starts_with("https://");
    let mut slug = raw.to_string();
    if is_http || lower.starts_with("kick.com") || lower.starts_with("www.kick.com") {
        let with_scheme = if is_http { raw.to_string() } else { format!("https://{raw}") };
        let u = url::Url::parse(&with_scheme).ok()?;
        let host = u.host_str().unwrap_or("").to_lowercase();
        if host != "kick.com" && !host.ends_with(".kick.com") {
            return None;
        }
        // kick.com/<slug> and nothing deeper: a link to a clip or video points at a channel too.
        slug = js_trim(u.path_segments().and_then(|mut p| p.find(|s| !s.is_empty())).unwrap_or("")).to_string();
    }
    let slug = slug.strip_prefix('@').unwrap_or(&slug);
    if slug_ok(slug) {
        Some(slug.to_lowercase().replace('_', "-"))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async fn kick_fetch(url: &str, user_agent: &str, method: &'static str, body: Option<Value>) -> Result<reqwest::Response, net::FetchError> {
    let mut o = FetchOpts::new(12_000).method(method).header("Accept", "application/json").header("User-Agent", user_agent);
    if let Some(b) = body {
        o = o.header("Content-Type", "application/json").body(b.to_string());
    }
    net::fetch(url, o).await
}

/// `encodeURIComponent`
fn enc(s: &str) -> String {
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

/// What `getKickChannel` resolves to.
#[derive(Clone, Debug)]
pub struct KickChannel {
    pub channel_id: String,
    pub user_id: String,
    /// "" when Kick sent none
    pub chatroom_id: String,
    pub slug: String,
    pub live: bool,
    pub title: String,
    pub category: String,
    pub viewers: Value,
    pub sub_badges: Vec<SubBadge>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubBadge {
    pub months: i64,
    pub url: String,
}

/// Resolve a slug to what the socket and header need. None when the channel does not exist.
/// `user_id` is the broadcaster's user id (what the site calls userId); `channel_id` is the one `channel.<id>`
/// events and the history endpoint use; `chatroom_id` is the one `chatrooms.<id>.v2` uses. They differ.
pub async fn get_kick_channel(slug: &str, user_agent: &str) -> Result<Option<KickChannel>, String> {
    let r = kick_fetch(&format!("https://kick.com/api/v2/channels/{}", enc(slug)), user_agent, "", None).await.map_err(|e| e.message)?;
    if r.status().as_u16() == 404 {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(format!("Kick answered {}", r.status().as_u16()));
    }
    let ch = net::json(r).await.map_err(|e| e.message)?;
    if !truthy(&ch["id"]) {
        return Ok(None);
    }
    Ok(Some(channel_from_json(&ch, slug)))
}

/// What kick.com says about one user in one channel (`/api/v2/channels/<slug>/users/<user slug>`): the roles and badges
/// the site itself uses. None when Kick does not know the pair (404). Unofficial and may be blocked: callers fall back.
pub async fn get_channel_user(slug: &str, user_slug: &str, user_agent: &str) -> Result<Option<Value>, String> {
    let url = format!("https://kick.com/api/v2/channels/{}/users/{}", enc(slug), enc(user_slug));
    let r = kick_fetch(&url, user_agent, "", None).await.map_err(|e| e.message)?;
    if r.status().as_u16() == 404 {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(format!("Kick answered {}", r.status().as_u16()));
    }
    net::json(r).await.map(Some).map_err(|e| e.message)
}

/// Does a badge list (`[{type: "moderator"}, ...]`, or Kick's `badges_v2` shape `[{name: "moderator"}, ...]`) mark a
/// moderator or the broadcaster? Hidden badges count: a moderator who chose not to show the badge is still one.
pub fn badges_mark_mod(badges: &Value) -> bool {
    badges.as_array().map(|a| a.iter().any(|b| matches!(b["type"].as_str().or(b["name"].as_str()), Some("moderator" | "broadcaster")))).unwrap_or(false)
}

/// Is this user a moderator (or the owner) of the channel, per `get_channel_user`'s answer?
pub fn channel_user_is_mod(u: &Value) -> bool {
    u["is_moderator"] == json!(true) || u["is_channel_owner"] == json!(true) || u["is_broadcaster"] == json!(true) || badges_mark_mod(&u["badges"]) || badges_mark_mod(&u["badges_v2"])
}

/// The pure half of `getKickChannel`.
pub fn channel_from_json(ch: &Value, slug: &str) -> KickChannel {
    let ls = &ch["livestream"];
    let cat = ls["categories"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
    let own_slug = js_str_or_empty(Some(&ch["slug"]));
    KickChannel {
        channel_id: js_string(&ch["id"]),
        user_id: str_nullish(&ch["user_id"]),
        chatroom_id: if truthy(&ch["chatroom"]["id"]) { js_string(&ch["chatroom"]["id"]) } else { String::new() },
        slug: if own_slug.is_empty() { slug.to_string() } else { own_slug },
        live: truthy(ls) && ls["is_live"] != Value::Bool(false),
        title: js_str_or_empty(Some(&ls["session_title"])),
        category: js_str_or_empty(Some(&cat["name"])),
        viewers: num_or0(&ls["viewer_count"]),
        sub_badges: sub_badges_from(ch),
    }
}

/// Kick's own web client asks this endpoint which realtime provider and credentials to use for a channel's
/// chat (the Pusher app key is NOT hard-coded in its JS any more). Public, no auth. Returns (appKey, cluster).
pub async fn get_realtime_descriptor(channel_id: &str, user_agent: &str, client_id: &str) -> Result<(String, String), String> {
    let body = json!({"client": {"id": client_id, "type": "web"}, "capabilities": {"accepted_providers": [{"provider": "pusher"}]}});
    let url = format!("https://web.kick.com/api/v1/realtime/channels/{}/chat/connection", enc(channel_id));
    let r = kick_fetch(&url, user_agent, "POST", Some(body)).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("Kick realtime descriptor answered {}", r.status().as_u16()));
    }
    let j = net::json(r).await.map_err(|e| e.message)?;
    descriptor_from_json(&j).ok_or_else(|| "Kick offered no realtime provider this client speaks".to_string())
}

pub fn descriptor_from_json(j: &Value) -> Option<(String, String)> {
    for c in j["data"]["connections"].as_array().into_iter().flatten() {
        let cred = &c["credentials"];
        if c["provider"] == "pusher" && truthy(&cred["app_key"]) && truthy(&cred["cluster"]) {
            return Some((js_string(&cred["app_key"]), js_string(&cred["cluster"])));
        }
    }
    None
}

/// Recent chat for the backlog, oldest first. Public; best-effort (the caller tolerates failure).
pub async fn fetch_history(channel_id: &str, user_agent: &str) -> Result<Vec<Value>, String> {
    let r = kick_fetch(&format!("https://kick.com/api/v2/channels/{}/messages", enc(channel_id)), user_agent, "", None).await.map_err(|e| e.message)?;
    if !r.status().is_success() {
        return Err(format!("Kick history answered {}", r.status().as_u16()));
    }
    let j = net::json(r).await.map_err(|e| e.message)?;
    let mut list: Vec<Value> = j["data"]["messages"].as_array().cloned().unwrap_or_default();
    list.sort_by_key(|m| m["created_at"].as_str().and_then(parse_rfc3339_ms).unwrap_or(0));
    Ok(list)
}

// ---------------------------------------------------------------------------
// Row shape
// ---------------------------------------------------------------------------

/// A Kick chat message (Pusher App\Events\ChatMessageEvent, or one entry of the history endpoint) -> the row
/// the chat page renders, field for field what the site's toKickChatMsg produces from a webhook.
///
/// The emote markup (`[emote:1234:name]`) is left INSIDE `text` on purpose: it is self-delimiting, so the
/// renderer splits on it directly and it cannot drift out of sync with an offset table.
pub fn to_kick_chat_msg(ev: &Value) -> Value {
    let sender = &ev["sender"];
    let identity = &sender["identity"];
    let ts = ev["created_at"].as_str().and_then(parse_rfc3339_ms);
    // The history endpoint ships `metadata` as a JSON string; the socket ships an object.
    let meta: Value = match &ev["metadata"] {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        other => other.clone(),
    };
    let orig = &meta["original_message"];
    let orig_sender = &meta["original_sender"];
    let first = |a: &Value, b: &Value| js_str_or_empty(Some(if truthy(a) { a } else { b }));

    let user_id = if !sender["id"].is_null() { str_nullish(&sender["id"]) } else { str_nullish(&sender["user_id"]) };
    let slug = first(&sender["slug"], &sender["channel_slug"]);
    let username = js_str_or_empty(Some(&sender["username"]));
    let reply = if ev["type"] == "reply" && truthy(orig) {
        json!({"id": js_str_or_empty(Some(&orig["id"])), "text": js_str_or_empty(Some(&orig["content"])), "name": js_str_or_empty(Some(&orig_sender["username"]))})
    } else {
        Value::Null
    };
    json!({
        "platform": "kick",
        "id": first(&ev["id"], &ev["message_id"]),
        "ts": ts.unwrap_or_else(now_ms),
        "user": {
            // The numeric id is what every moderation call needs, and the one identifier Kick's API can act on.
            "id": user_id,
            "login": slug,
            "name": if username.is_empty() { slug.clone() } else { username },
            "color": first(&identity["color"], &identity["username_color"]),
            // The socket payload carries no avatar or verified flag; the row keeps the fields so the renderer needs no branch.
            "avatar": js_str_or_empty(Some(&sender["profile_picture"])),
            "verified": truthy(&sender["is_verified"]),
            "anonymous": truthy(&sender["is_anonymous"]),
        },
        "badges": kick_badges(identity),
        "text": js_str_or_empty(Some(&ev["content"])),
        "reply": reply,
        "action": false,
        "notice": if ev["type"] == "celebration" { json!("celebration") } else { Value::Null },
    })
}

// ---------------------------------------------------------------------------
// Badges
// ---------------------------------------------------------------------------

/// How many badges kick.com's chat shows in front of a name (MAX_BADGE_COUNT in its chat code).
pub const CHAT_BADGE_MAX: usize = 4;
/// A message never carries more than this many badges (a guard against a runaway list, not a Kick rule).
const BADGE_KEEP_MAX: usize = 24;

/// A sender's badges (`sender.identity`) -> the row's badge list, in the order kick.com draws them.
///
/// Kick ships two lists. `badges` holds the channel badges ({type, text, count, sort_order}: broadcaster, moderator,
/// vip, og, founder, subscriber, sub_gifter, verified, bot, ...) and still drew everything until November 2026.
/// `badges_v2` holds the global ones ({name, badge_type, image_url, metadata: {level}, selected, sort_order}: the
/// "Level 58" badge, "KICK Founder 22", event badges like "GOAT"). kick.com's chat joins the two (channel first),
/// drops a v2 badge the user did not select (and a channel badge marked `active: false`), sorts what is left by
/// `sort_order` (stable, missing last) and shows the first four.
///
/// Every badge is kept, in that order, so mod rights and highlights still see a moderator badge that does not fit;
/// `hidden` marks the ones kick.com's chat does not show. Each row badge is
/// `{type, text, count, label, url?, level?, sort?, hidden}`: `type` is the v1 type or the v2 name, `label`
/// kick.com's tooltip, `url` Kick's own picture when it has one.
pub fn kick_badges(identity: &Value) -> Vec<Value> {
    // (sort key, shown in chat, badge)
    let mut list: Vec<(f64, bool, Value)> = Vec::new();
    let sort_of = |b: &Value| b["sort_order"].as_f64().unwrap_or(f64::INFINITY);
    for b in identity["badges"].as_array().into_iter().flatten() {
        let kind = js_str_or_empty(Some(&b["type"]));
        if kind.is_empty() {
            continue;
        }
        let text = js_str_or_empty(Some(&b["text"]));
        let count = num_or0(&b["count"]);
        let label = crate::kick::badges::badge_label(&kind, &text, count.as_f64().unwrap_or(0.0));
        let mut row = json!({"type": kind, "text": text, "count": count, "label": label, "hidden": false});
        if let Some(o) = b["sort_order"].as_f64() {
            row["sort"] = json!(o);
        }
        list.push((sort_of(b), b["active"] != json!(false), row));
    }
    for b in identity["badges_v2"].as_array().into_iter().flatten() {
        let name = js_str_or_empty(Some(&b["name"]));
        if name.is_empty() {
            continue;
        }
        let level = b["metadata"]["level"].as_f64().map(|l| l as i64);
        let image = js_str_or_empty(Some(&b["image_url"]));
        let mut row = json!({"type": name, "text": "", "count": 0, "label": crate::kick::badges::badge_v2_label(&name, level), "hidden": false, "v2": true});
        if !image.is_empty() {
            row["url"] = json!(crate::kick::badges::cdn_badge_url(&image));
        }
        if let Some(l) = level {
            row["level"] = json!(l);
        }
        if let Some(o) = b["sort_order"].as_f64() {
            row["sort"] = json!(o);
        }
        list.push((sort_of(b), b["selected"] != json!(false), row));
    }
    // a stable sort, like Array.prototype.sort
    list.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut shown = 0;
    let mut out = Vec::with_capacity(list.len().min(BADGE_KEEP_MAX));
    for (_, show, mut row) in list.into_iter().take(BADGE_KEEP_MAX) {
        let visible = show && shown < CHAT_BADGE_MAX;
        if visible {
            shown += 1;
        }
        row["hidden"] = json!(!visible);
        out.push(row);
    }
    out
}

// ---------------------------------------------------------------------------
// Emotes for the picker (unchanged from kickApi.js apart from the UA)
// ---------------------------------------------------------------------------

const EMOTE_TTL_MS: i64 = 30 * 60_000;
static EMOTE_CACHE: LazyLock<Mutex<HashMap<String, (i64, Value)>>> = LazyLock::new(|| Mutex::new(HashMap::new())); // slug -> { at, emotes }

pub async fn fetch_kick_emotes(slug: &str, user_agent: &str, force: bool) -> Value {
    let key = slug.to_lowercase();
    if key.is_empty() {
        return json!({});
    }
    let hit = EMOTE_CACHE.lock().unwrap().get(&key).cloned();
    if !force {
        if let Some((at, emotes)) = &hit {
            if now_ms() - at < EMOTE_TTL_MS {
                return emotes.clone();
            }
        }
    }
    let attempt: Result<Value, String> = async {
        let r = kick_fetch(&format!("https://kick.com/emotes/{}", enc(&key)), user_agent, "", None).await.map_err(|e| e.message)?;
        if !r.status().is_success() {
            return Err(r.status().as_u16().to_string());
        }
        let groups = net::json(r).await.map_err(|e| e.message)?;
        Ok(emotes_from_groups(&groups))
    }
    .await;
    match attempt {
        Ok(emotes) => {
            EMOTE_CACHE.lock().unwrap().insert(key, (now_ms(), emotes.clone()));
            emotes
        }
        Err(_) => {
            // Serve a stale list rather than emptying a working picker over one blocked request. A blocked list costs
            // the picker and not the chat: an emote inside a message renders from files.kick.com by id alone.
            if let Some((_, emotes)) = hit {
                return emotes;
            }
            EMOTE_CACHE.lock().unwrap().insert(key, (now_ms(), json!({})));
            json!({})
        }
    }
}

/// The pure half of `fetchKickEmotes`: Kick's emote groups -> {name: {id, group, subsOnly}}.
pub fn emotes_from_groups(groups: &Value) -> Value {
    let mut emotes = Map::new();
    for g in groups.as_array().into_iter().flatten() {
        // The channel's own group has a numeric id and a slug; Kick's shared sets arrive as "Global" and "Emoji".
        let group = if truthy(&g["slug"]) {
            "channel".to_string()
        } else if truthy(&g["id"]) {
            js_string(&g["id"]).to_lowercase()
        } else {
            "global".to_string()
        };
        for e in g["emotes"].as_array().into_iter().flatten() {
            if !truthy(&e["name"]) {
                continue;
            }
            emotes.insert(js_string(&e["name"]), json!({"id": js_string(&e["id"]), "group": group, "subsOnly": truthy(&e["subscribers_only"])}));
        }
    }
    Value::Object(emotes)
}

fn to_i(v: &Value) -> i64 {
    v.as_i64().unwrap_or_else(|| v.as_f64().unwrap_or(0.0) as i64)
}

/// A channel's own subscriber badge artwork, [{months, url}] -- the only badges Kick serves as images.
fn sub_badges_from(ch: &Value) -> Vec<SubBadge> {
    let mut badges = Vec::new();
    for b in ch["subscriber_badges"].as_array().into_iter().flatten() {
        let url = js_str_or_empty(Some(&b["badge_image"]["src"]));
        if url.is_empty() {
            continue;
        }
        badges.push(SubBadge { months: to_i(&num_or0(&b["months"])), url });
    }
    badges.sort_by_key(|b| b.months);
    badges
}

/// The badge a subscriber of `months` months wears: the highest tier they have reached.
pub fn sub_badge_for(badges: &[SubBadge], months: &Value) -> String {
    if badges.is_empty() {
        return String::new();
    }
    let m = to_i(&num_or0(months));
    let mut best = &badges[0];
    for b in badges {
        if b.months <= m && b.months >= best.months {
            best = b;
        }
    }
    best.url.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_forms() {
        let p = |s: &str| parse_kick_source(s);
        assert_eq!(p("Foo"), Some("foo".into()));
        assert_eq!(p("  foo  "), Some("foo".into()));
        assert_eq!(p("some_user"), Some("some-user".into()));
        assert_eq!(p("@Foo"), Some("foo".into()));
        assert_eq!(p("kick.com/Foo"), Some("foo".into()));
        assert_eq!(p("www.kick.com/Foo"), Some("foo".into()));
        assert_eq!(p("https://kick.com/Foo"), Some("foo".into()));
        assert_eq!(p("http://www.kick.com/Foo/"), Some("foo".into()));
        assert_eq!(p("https://kick.com/some_user/videos/123?x=1#y"), Some("some-user".into()));
        assert_eq!(p("https://kick.com/@Foo"), Some("foo".into()));
        assert_eq!(p("https://sub.kick.com/foo"), Some("foo".into()));
        assert_eq!(p("https://evil.com/foo"), None);
        assert_eq!(p("https://notkick.com/foo"), None);
        assert_eq!(p("kick.company/foo"), None);
        assert_eq!(p("https://kick.com/"), None);
        assert_eq!(p("kick.com"), None);
        assert_eq!(p(""), None);
        assert_eq!(p("   "), None);
        assert_eq!(p(&"a".repeat(25)), Some("a".repeat(25)));
        assert_eq!(p(&"a".repeat(26)), None);
        assert_eq!(p("foo bar"), None);
        assert_eq!(p("foo.bar"), None);
    }

    #[test]
    fn subscriber_badge_is_the_highest_tier_not_above_the_months() {
        let subs = vec![SubBadge { months: 1, url: "b1".into() }, SubBadge { months: 3, url: "b3".into() }, SubBadge { months: 12, url: "b12".into() }];
        assert_eq!(sub_badge_for(&subs, &json!(1)), "b1");
        assert_eq!(sub_badge_for(&subs, &json!(2)), "b1");
        assert_eq!(sub_badge_for(&subs, &json!(3)), "b3");
        assert_eq!(sub_badge_for(&subs, &json!(11)), "b3");
        assert_eq!(sub_badge_for(&subs, &json!(12)), "b12");
        assert_eq!(sub_badge_for(&subs, &json!(400)), "b12");
        assert_eq!(sub_badge_for(&subs, &Value::Null), "b1");
    }

    #[test]
    fn every_badge_of_a_message_is_kept_with_its_type_text_and_count() {
        let ev = json!({"id": "x", "content": "hi", "sender": {"id": 1, "username": "A", "slug": "a", "identity": {"color": "#fff", "badges": [
            {"type": "broadcaster", "text": "Broadcaster"}, {"type": "moderator", "text": "Moderator"}, {"type": "vip", "text": "VIP"},
            {"type": "og", "text": "OG"}, {"type": "founder", "text": "Founder"}, {"type": "verified", "text": "Verified"},
            {"type": "subscriber", "text": "Subscriber", "count": 14}, {"type": "sub_gifter", "text": "Sub Gifter", "count": 30}]}}});
        let m = to_kick_chat_msg(&ev);
        let b = m["badges"].as_array().unwrap();
        assert_eq!(b.iter().map(|x| x["type"].as_str().unwrap()).collect::<Vec<_>>(), ["broadcaster", "moderator", "vip", "og", "founder", "verified", "subscriber", "sub_gifter"]);
        assert_eq!(b[6]["count"], 14);
        assert_eq!(b[7]["count"], 30);
        assert_eq!(b[7]["text"], "Sub Gifter");
        assert_eq!(b[6]["label"], "14-Month Subscriber");
        // without a sort order kick.com keeps the list as sent and shows the first four
        assert_eq!(b.iter().map(|x| x["hidden"] == json!(true)).collect::<Vec<_>>(), [false, false, false, false, true, true, true, true]);
        assert!(badges_mark_mod(&m["badges"]));
    }

    /// One row's badges as (type, label, hidden).
    fn badge_view(m: &Value) -> Vec<(String, String, bool)> {
        m["badges"].as_array().unwrap().iter().map(|b| (b["type"].as_str().unwrap().to_string(), b["label"].as_str().unwrap().to_string(), b["hidden"] == json!(true))).collect()
    }

    /// The badges kick.com's chat draws for a row, in order.
    fn shown(m: &Value) -> Vec<String> {
        badge_view(m).into_iter().filter(|(_, _, h)| !h).map(|(t, _, _)| t).collect()
    }

    /// Real chat of a busy channel (2026-11-18, trimmed, texts replaced): five history messages whose `badges` is
    /// empty and whose badges are all in `badges_v2`, and two live socket messages with channel badges in `badges`.
    #[test]
    fn badges_v2_from_kick_history() {
        let j: Value = serde_json::from_str(include_str!("fixtures/kick_history_badges_v2.json")).unwrap();
        let rows: Vec<Value> = j["data"]["messages"].as_array().unwrap().iter().map(to_kick_chat_msg).collect();
        let by = |name: &str| rows.iter().find(|r| r["user"]["name"] == name).unwrap_or_else(|| panic!("{name}"));

        // the level badge: Kick's own picture (fetched small), "Level 57", its number kept
        let lvl = &by("Ragger")["badges"][0];
        assert_eq!(lvl["type"], "level");
        assert_eq!(lvl["label"], "Level 57");
        assert_eq!(lvl["level"], 57);
        assert_eq!(lvl["hidden"], false);
        assert_eq!(lvl["v2"], true);
        assert_eq!(lvl["url"], "https://ext.cdn.kick.com/chat/badges/57_ddc069ae-a702-456d-8f1b-7c737c6abf8a.png?width=72,format=webp,quality=50");

        // sorted by sort_order whatever order Kick sent them in
        assert_eq!(shown(by("Milopaks")), ["level", "GOAT"]);
        assert_eq!(
            badge_view(by("khanspiracy")),
            [
                ("level".to_string(), "Level 45".to_string(), false),
                ("kick_founder22".to_string(), "KICK Founder 22".to_string(), false),
                ("WorldofWarcraftForeverBeta".to_string(), "WorldofWarcraftForeverBeta".to_string(), false)
            ]
        );
        // an unselected badge is kept but hidden, and does not use up one of the four places
        let uno = badge_view(by("unohaze"));
        assert_eq!(uno.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(), ["level", "DEDsafioNutria", "WorldofWarcraftForeverBeta", "Ghost"]);
        assert_eq!(uno.iter().map(|b| b.2).collect::<Vec<_>>(), [false, true, false, false]);
        // a level badge the user did not select is not drawn on kick.com either
        assert!(shown(by("flop_e")).is_empty());
        assert_eq!(by("flop_e")["badges"][0]["level"], 18);

        // channel badges (old list) and global ones together, in kick.com's order
        assert_eq!(shown(by("a93c")), ["GoldenK", "subscriber"]);
        assert_eq!(by("a93c")["badges"][2]["label"], "3-Month Subscriber");
        let fossa = by("Fossabot");
        assert_eq!(shown(fossa), ["moderator", "verified"]);
        assert_eq!(badge_view(fossa)[0], ("level".to_string(), "Level 1".to_string(), true));
        assert_eq!(badge_view(fossa)[1].1, "Moderator");
        assert_eq!(badge_view(fossa)[2].1, "Verified");
        assert!(badges_mark_mod(&fossa["badges"]));
        assert!(!badges_mark_mod(&by("khanspiracy")["badges"]));
    }

    #[test]
    fn only_four_badges_show_and_hidden_ones_still_count_for_mod_rights() {
        let v2 = |name: &str, sort: i64| json!({"name": name, "badge_type": "global", "image_url": format!("https://ext.cdn.kick.com/chat/badges/{name}.png"), "selected": true, "sort_order": sort});
        let identity = json!({
            "badges": [{"type": "moderator", "text": "Moderator", "sort_order": 12}, {"type": "subscriber", "text": "Subscriber", "count": 2, "sort_order": 9}],
            "badges_v2": [{"name": "level", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/9_x.png", "metadata": {"level": 9}, "selected": true, "sort_order": 1}, v2("kick_founder22", 2), v2("GOAT", 4), v2("Ghost", 4)]
        });
        let m = to_kick_chat_msg(&json!({"id": "1", "content": "x", "sender": {"id": 5, "username": "M", "slug": "m", "identity": identity}}));
        assert_eq!(shown(&m), ["level", "kick_founder22", "GOAT", "Ghost"]);
        let all = badge_view(&m);
        assert_eq!(all.iter().map(|b| b.0.as_str()).collect::<Vec<_>>(), ["level", "kick_founder22", "GOAT", "Ghost", "subscriber", "moderator"]);
        // the moderator badge does not fit, but the row still says it is a moderator
        assert!(badges_mark_mod(&m["badges"]));
        // a channel badge Kick marks inactive is hidden too
        let off = to_kick_chat_msg(&json!({"id": "2", "sender": {"id": 6, "identity": {"badges": [{"type": "vip", "active": false}, {"type": "og"}]}}}));
        assert_eq!(shown(&off), ["og"]);
    }

    #[test]
    fn the_old_badge_list_alone_still_works() {
        // a sender with only the old list (all Kick sent before badges_v2)
        let m = to_kick_chat_msg(&json!({"id": "1", "sender": {"id": 5, "identity": {"badges": [{"type": "subscriber", "text": "Subscriber", "count": 5}, {"type": "", "text": "nameless"}]}}}));
        assert_eq!(badge_view(&m), [("subscriber".to_string(), "5-Month Subscriber".to_string(), false)]);
        assert!(m["badges"][0].get("url").is_none());
        assert_eq!(to_kick_chat_msg(&json!({"id": "1", "sender": {"id": 5}}))["badges"], json!([]));
        assert_eq!(to_kick_chat_msg(&json!({"id": "1", "sender": {"id": 5, "identity": {"badges": null, "badges_v2": null}}}))["badges"], json!([]));
    }

    #[test]
    fn mod_badges_in_either_list_mark_a_moderator() {
        assert!(badges_mark_mod(&json!([{"type": "moderator"}])));
        assert!(badges_mark_mod(&json!([{"name": "broadcaster", "badge_type": "channel"}])));
        assert!(!badges_mark_mod(&json!([{"name": "level"}, {"type": "vip"}])));
        assert!(channel_user_is_mod(&json!({"badges": [], "badges_v2": [{"name": "moderator", "badge_type": "channel", "selected": false}]})));
        assert!(!channel_user_is_mod(&json!({"badges": [], "badges_v2": [{"name": "level", "selected": true}]})));
    }

    #[test]
    fn chat_row_from_socket_and_history_shapes() {
        let ev = json!({
            "id": "abc", "content": "hi [emote:1:Kappa]", "type": "reply", "created_at": "2026-11-05T12:00:00.000000Z",
            "sender": {"id": 42, "username": "Bob", "slug": "bob", "identity": {"color": "#fff", "badges": [{"type": "subscriber", "text": "Subscriber", "count": 5}]}},
            "metadata": "{\"original_message\":{\"id\":\"o1\",\"content\":\"yo\"},\"original_sender\":{\"username\":\"Al\"}}"
        });
        let m = to_kick_chat_msg(&ev);
        assert_eq!(m["platform"], "kick");
        assert_eq!(m["id"], "abc");
        assert_eq!(m["ts"], parse_rfc3339_ms("2026-11-05T12:00:00Z").unwrap());
        assert_eq!(m["user"]["id"], "42");
        assert_eq!(m["user"]["name"], "Bob");
        assert_eq!(m["badges"][0]["count"], 5);
        assert_eq!(m["reply"], json!({"id": "o1", "text": "yo", "name": "Al"}));
        assert_eq!(m["notice"], Value::Null);
        let c = to_kick_chat_msg(&json!({"message_id": "m", "type": "celebration", "sender": {"user_id": 7, "channel_slug": "x"}}));
        assert_eq!(c["id"], "m");
        assert_eq!(c["user"]["id"], "7");
        assert_eq!(c["user"]["name"], "x");
        assert_eq!(c["notice"], "celebration");
    }

    #[test]
    fn channel_descriptor_emotes_and_badges() {
        let ch = json!({"id": 9, "user_id": 3, "slug": "Foo", "chatroom": {"id": 11},
            "livestream": {"is_live": true, "session_title": "t", "viewer_count": "12", "categories": [{"name": "Just Chatting"}]},
            "subscriber_badges": [{"months": 6, "badge_image": {"src": "b6"}}, {"months": 1, "badge_image": {"src": "b1"}}, {"months": 3}]});
        let c = channel_from_json(&ch, "foo");
        assert_eq!((c.channel_id.as_str(), c.user_id.as_str(), c.chatroom_id.as_str(), c.slug.as_str()), ("9", "3", "11", "Foo"));
        assert!(c.live);
        assert_eq!((c.title.as_str(), c.category.as_str(), c.viewers.clone()), ("t", "Just Chatting", json!(12)));
        assert_eq!(c.sub_badges.iter().map(|b| b.months).collect::<Vec<_>>(), vec![1, 6]);
        assert_eq!(sub_badge_for(&c.sub_badges, &json!(0)), "b1");
        assert_eq!(sub_badge_for(&c.sub_badges, &json!(5)), "b1");
        assert_eq!(sub_badge_for(&c.sub_badges, &json!(7)), "b6");
        assert_eq!(sub_badge_for(&[], &json!(7)), "");
        assert!(!channel_from_json(&json!({"id": 1, "livestream": {"is_live": false}}), "x").live);
        assert!(!channel_from_json(&json!({"id": 1}), "x").live);

        let d = json!({"data": {"connections": [{"provider": "other"}, {"provider": "pusher", "credentials": {"app_key": "k", "cluster": "us2"}}]}});
        assert_eq!(descriptor_from_json(&d), Some(("k".into(), "us2".into())));
        assert_eq!(descriptor_from_json(&json!({"data": {}})), None);

        let g = json!([{"id": 5, "slug": "foo", "emotes": [{"id": 1, "name": "A", "subscribers_only": true}, {"name": ""}]}, {"id": "Global", "emotes": [{"id": 2, "name": "B"}]}]);
        assert_eq!(emotes_from_groups(&g), json!({"A": {"id": "1", "group": "channel", "subsOnly": true}, "B": {"id": "2", "group": "global", "subsOnly": false}}));
    }
}
