//! Port of src/renderer/chat/twitch/cosmetics.ts, the UI-free part: 7TV cosmetic definitions (paints, badges), what a
//! chatter's 7TV lookup answers (`fetch_chatter_style`) and a paint's colour at a point of the name. The desktop
//! (`gpui/chat/twitch/cosmetics.rs`) keeps its own cache and queue on the UI thread; Android's is `StyleBook` below.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use serde_json::{json, Value};

use super::emotes::{fetch_seven_tv_personal_set, personal_set_id, read_cache, seven_tv_subscriber, write_cache, EmoteMap};

#[derive(Clone, Debug, Default)]
pub struct PaintStop {
    pub at: f32,
    pub color: u32,
}

#[derive(Clone, Debug, Default)]
pub struct SevenTvPaint {
    pub id: String,
    pub name: String,
    pub function: String,
    pub color: Option<u32>,
    pub angle: f32,
    pub repeat: bool,
    /// radial gradients: "circle" or "ellipse" (7TV's `shape`, default circle)
    pub shape: String,
    pub image_url: Option<String>,
    pub stops: Vec<PaintStop>,
    pub shadows: Vec<PaintShadow>,
}

/// One `drop-shadow(x y radius color)` of a paint's CSS `filter`.
#[derive(Clone, Debug, Default)]
pub struct PaintShadow {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub color: u32,
}

#[derive(Clone, Debug, Default)]
pub struct SevenTvBadgeDef {
    pub id: String,
    pub name: String,
    pub tooltip: String,
}

#[derive(Default)]
pub struct SevenTvDefs {
    pub paints: HashMap<String, SevenTvPaint>,
    pub badges: HashMap<String, SevenTvBadgeDef>,
}

#[derive(Clone, Debug, Default)]
pub struct ChatterStyle {
    pub color: Option<i64>,
    pub paint_id: Option<String>,
    pub badge_id: Option<String>,
    /// the chatter's 7TV personal emotes (a subscriber's own set, drawn in every channel); None without one
    pub personal: Option<Arc<EmoteMap>>,
}

pub const DEFS_KEY: &str = "emotes:7tv:cosmetics:global:v2";
const DEFS_TTL_MS: i64 = 6 * 3_600_000;
pub const GQL_QUERY: &str = "{cosmetics{paints{id name function color angle shape repeat image_url stops{at color}shadows{x_offset y_offset radius color}}badges{id name tooltip}}}";

/// Paint colours arrive as signed 32-bit RGBA ints; reinterpret as unsigned.
pub fn to_u32(v: &Value) -> Option<u32> {
    v.as_i64().map(|n| n as u32)
}

pub fn defs_from_raw(data: &Value) -> SevenTvDefs {
    let mut d = SevenTvDefs::default();
    for p in data.get("paints").and_then(|p| p.as_array()).into_iter().flatten() {
        let s = |k: &str| p.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let paint = SevenTvPaint {
            id: s("id"),
            name: s("name"),
            function: s("function"),
            color: p.get("color").and_then(to_u32),
            angle: p.get("angle").and_then(|a| a.as_f64()).unwrap_or(0.0) as f32,
            repeat: p.get("repeat").and_then(|a| a.as_bool()).unwrap_or(false),
            shape: s("shape"),
            image_url: p.get("image_url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()),
            stops: p
                .get("stops")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .map(|st| PaintStop { at: st.get("at").and_then(|a| a.as_f64()).unwrap_or(0.0) as f32, color: st.get("color").and_then(to_u32).unwrap_or(0) })
                .collect(),
            shadows: p
                .get("shadows")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .map(|sh| {
                    let f = |k: &str| sh.get(k).and_then(|a| a.as_f64()).unwrap_or(0.0) as f32;
                    PaintShadow { x: f("x_offset"), y: f("y_offset"), radius: f("radius"), color: sh.get("color").and_then(to_u32).unwrap_or(0) }
                })
                .collect(),
        };
        d.paints.insert(paint.id.clone(), paint);
    }
    for b in data.get("badges").and_then(|p| p.as_array()).into_iter().flatten() {
        let s = |k: &str| b.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let def = SevenTvBadgeDef { id: s("id"), name: s("name"), tooltip: s("tooltip") };
        d.badges.insert(def.id.clone(), def);
    }
    d
}

pub async fn fetch_seven_tv_defs(client: &reqwest::Client) -> Option<Value> {
    super::emotes::cache_ready().await;
    if let Some(c) = read_cache(DEFS_KEY, DEFS_TTL_MS) {
        return Some(c);
    }
    let r = client.post("https://7tv.io/v3/gql").json(&json!({"query": GQL_QUERY})).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let j: Value = r.json().await.ok()?;
    let data = j.pointer("/data/cosmetics")?.clone();
    write_cache(DEFS_KEY, &data);
    Some(data)
}

/// Cache key of a Kick chatter: Twitch and Kick ids are both plain numbers and must not share an entry.
pub fn kick_key(user_id: &str) -> String {
    if user_id.is_empty() {
        String::new()
    } else {
        format!("kick:{user_id}")
    }
}

/// (7TV platform, platform user id) of a cache key: a bare id is a Twitch user, `kick:<id>` a Kick one.
pub fn split_key(key: &str) -> (&'static str, &str) {
    match key.strip_prefix("kick:") {
        Some(id) => ("kick", id),
        None => ("twitch", key),
    }
}

/// What a chatter's 7TV user answer says: their style (paint, badge, colour) and, for a subscriber, the id of
/// their personal emote set. `None` when there is neither.
pub fn style_from_user(d: &Value) -> Option<(ChatterStyle, Option<String>)> {
    let user = d.get("user").filter(|u| !u.is_null())?;
    let personal_set = if seven_tv_subscriber(user) { personal_set_id(user) } else { None };
    let st = user.get("style").filter(|s| !s.is_null());
    if st.is_none() && personal_set.is_none() {
        return None;
    }
    let field = |k: &str| st.and_then(|s| s.get(k)).and_then(|v| v.as_str()).map(|s| s.to_string());
    let style = ChatterStyle { color: st.and_then(|s| s.get("color")).and_then(|c| c.as_i64()), paint_id: field("paint_id"), badge_id: field("badge_id"), personal: None };
    Some((style, personal_set))
}

/// `None`: the lookup failed (network, 5xx, rate limit) and is worth retrying; `Some(None)`: 7TV answered,
/// and the chatter has no style (or no 7TV account). A personal set is fetched in the same lookup (so it counts
/// against the same in-flight limit) and comes from the emote cache when it was seen within the hour.
pub async fn fetch_chatter_style(client: &reqwest::Client, key: &str) -> Option<Option<ChatterStyle>> {
    let (platform, user_id) = split_key(key);
    let r = client.get(format!("https://7tv.io/v3/users/{platform}/{user_id}")).send().await.ok()?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Some(None);
    }
    if !r.status().is_success() {
        return None;
    }
    let d: Value = r.json().await.ok()?;
    let Some((mut style, personal_set)) = style_from_user(&d) else { return Some(None) };
    if let Some(set_id) = personal_set {
        let emotes = fetch_seven_tv_personal_set(client, &set_id, false).await?;
        style.personal = (!emotes.is_empty()).then(|| Arc::new(emotes));
    }
    Some(Some(style))
}

/// Wait before retrying a failed lookup: 5 s, 15 s, 60 s, then every 5 minutes.
pub fn retry_delay(failures: u32) -> std::time::Duration {
    std::time::Duration::from_secs(match failures {
        0 | 1 => 5,
        2 => 15,
        3 => 60,
        _ => 300,
    })
}

/// The dev paint fixture (env `AGU_PAINT_FIXTURE`), if any.
pub fn paint_fixture() -> Option<Value> {
    let path = std::env::var_os("AGU_PAINT_FIXTURE")?;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Decoded paint colour: (r, g, b, a 0..1).
pub fn decode_rgba(c: u32) -> (f32, f32, f32, f32) {
    (((c >> 24) & 255) as f32 / 255.0, ((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0)
}

fn lerp(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32), t: f32) -> (f32, f32, f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, a.3 + (b.3 - a.3) * t)
}

/// The paint's colour at `t` (0..1) across the name's width.
pub fn paint_color_at(paint: &SevenTvPaint, t: f32) -> Option<(f32, f32, f32, f32)> {
    let gradient = (paint.function == "LINEAR_GRADIENT" || paint.function == "RADIAL_GRADIENT") && !paint.stops.is_empty();
    if gradient {
        let mut pos = if paint.function == "RADIAL_GRADIENT" { (t - 0.5).abs() * 2.0 } else { t };
        let last = paint.stops.last().map(|s| s.at).unwrap_or(1.0);
        if paint.repeat && last > 0.0 {
            pos %= last;
        }
        let stops = &paint.stops;
        if pos <= stops[0].at {
            return Some(decode_rgba(stops[0].color));
        }
        for w in stops.windows(2) {
            if pos <= w[1].at {
                let span = (w[1].at - w[0].at).max(1e-6);
                return Some(lerp(decode_rgba(w[0].color), decode_rgba(w[1].color), (pos - w[0].at) / span));
            }
        }
        return Some(decode_rgba(stops[stops.len() - 1].color));
    }
    paint.color.map(decode_rgba)
}

/// Every chatter's 7TV style, looked up in the background, shared between threads (Android; the desktop keeps its
/// own on the UI thread). `want` asks for a chatter: a known one answers at once, an unknown one is queued once. The
/// caller starts the lookups: `next_job` hands out a key while fewer than `MAX_ACTIVE` are running, `finish` takes
/// the answer (`None` = the lookup failed, worth retrying: the caller waits `retry_delay` and calls `requeue`).
#[derive(Default)]
pub struct StyleBook {
    st: std::sync::Mutex<BookState>,
}

#[derive(Default)]
struct BookState {
    styles: HashMap<String, Option<ChatterStyle>>,
    pending: HashSet<String>,
    queue: VecDeque<String>,
    active: usize,
    failures: HashMap<String, u32>,
}

pub const MAX_ACTIVE: usize = 3;

pub enum Want {
    /// answered (a chatter without a 7TV style answers `None` and is not asked again)
    Known(Option<ChatterStyle>),
    /// queued now: the caller should pump
    Queued,
    /// already queued or running
    Waiting,
}

impl StyleBook {
    pub fn want(&self, key: &str) -> Want {
        if key.is_empty() {
            return Want::Known(None);
        }
        let mut s = self.st.lock().unwrap();
        if let Some(v) = s.styles.get(key) {
            return Want::Known(v.clone());
        }
        if !s.pending.insert(key.to_string()) {
            return Want::Waiting;
        }
        s.queue.push_back(key.to_string());
        Want::Queued
    }

    pub fn known(&self, key: &str) -> Option<Option<ChatterStyle>> {
        self.st.lock().unwrap().styles.get(key).cloned()
    }

    pub fn next_job(&self) -> Option<String> {
        let mut s = self.st.lock().unwrap();
        if s.active >= MAX_ACTIVE {
            return None;
        }
        let k = s.queue.pop_front()?;
        s.active += 1;
        Some(k)
    }

    /// A lookup ended. `Some(delay)` when it failed: wait, then `requeue`.
    pub fn finish(&self, key: &str, res: Option<Option<ChatterStyle>>) -> Option<std::time::Duration> {
        let mut s = self.st.lock().unwrap();
        s.active = s.active.saturating_sub(1);
        match res {
            Some(style) => {
                s.pending.remove(key);
                s.failures.remove(key);
                s.styles.insert(key.to_string(), style);
                None
            }
            None => {
                let n = s.failures.entry(key.to_string()).or_insert(0);
                *n += 1;
                Some(retry_delay(*n))
            }
        }
    }

    pub fn requeue(&self, key: &str) {
        self.st.lock().unwrap().queue.push_back(key.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_colours_decode() {
        // -1 as i32 = 0xFFFFFFFF
        assert_eq!(decode_rgba(to_u32(&json!(-1)).unwrap()), (1.0, 1.0, 1.0, 1.0));
        assert_eq!(decode_rgba(0xFF000080), (1.0, 0.0, 0.0, 128.0 / 255.0));
    }

    #[test]
    fn gradient_interpolates_and_repeats() {
        let p = SevenTvPaint { function: "LINEAR_GRADIENT".into(), stops: vec![PaintStop { at: 0.0, color: 0xFF0000FF }, PaintStop { at: 1.0, color: 0x0000FFFF }], ..Default::default() };
        let mid = paint_color_at(&p, 0.5).unwrap();
        assert!((mid.0 - 0.5).abs() < 0.01 && (mid.2 - 0.5).abs() < 0.01);
        let solid = SevenTvPaint { function: "URL".into(), color: Some(0x00FF00FF), ..Default::default() };
        assert_eq!(paint_color_at(&solid, 0.3), Some((0.0, 1.0, 0.0, 1.0)));
    }

    #[test]
    fn style_keys_keep_the_platforms_apart() {
        assert_eq!(kick_key("42"), "kick:42");
        assert_eq!(kick_key(""), "");
        assert_eq!(split_key("kick:42"), ("kick", "42"));
        assert_eq!(split_key("42"), ("twitch", "42"));
    }

    /// `/v3/users/twitch/<id>` as 7TV answers it (2026-11, trimmed: the channel set's emotes, editors and
    /// connections cut).
    fn seven_tv_user(roles: Value, style: Value) -> Value {
        json!({
            "id": "71092938", "platform": "TWITCH", "username": "xqc", "display_name": "xQc",
            "emote_set_id": "01FE9DRF000009TR6M9N941CYW",
            "emote_set": {"id": "01FE9DRF000009TR6M9N941CYW", "name": "xQc's Emotes", "flags": 0, "emotes": []},
            "user": {
                "id": "01FE9DRF000009TR6M9N941CYW", "username": "xqc", "display_name": "xQc",
                "style": style,
                "emote_sets": [
                    {"id": "01GN7SRYE8000EK2J7DFXT0WE5", "name": "Personal Emotes", "flags": 4, "tags": [], "capacity": 5},
                    {"id": "01FE9DRF000009TR6M9N941CYW", "name": "xQc's Emotes", "flags": 0, "tags": [], "capacity": 1000},
                    {"id": "01J9MMS2R800036CR40E1041ED", "name": "Halloween Emotes 2026", "flags": 0, "tags": ["spooky"], "capacity": 1000}
                ],
                "roles": roles
            }
        })
    }

    #[test]
    fn a_subscribers_lookup_carries_their_personal_set() {
        let style = json!({"color": -1857617921, "paint_id": "01JHXJFACX42RV996VE9933TB8", "badge_id": "01JJJ74CRHZBRMCM8F4Y2WBN6R"});
        let sub = json!(["01F37RCTH00000K96678WEQT02", "01F37R3RFR0000K96678WEQT01", "01G68MMQFR0007J6GNM9E2M0TM"]);
        let (st, set) = style_from_user(&seven_tv_user(sub.clone(), style.clone())).unwrap();
        assert_eq!(set.as_deref(), Some("01GN7SRYE8000EK2J7DFXT0WE5"));
        assert_eq!(st.paint_id.as_deref(), Some("01JHXJFACX42RV996VE9933TB8"));
        assert_eq!(st.badge_id.as_deref(), Some("01JJJ74CRHZBRMCM8F4Y2WBN6R"));
        assert_eq!(st.color, Some(-1857617921));
        // no paint or badge picked: the personal set alone is still worth an entry
        let (st, set) = style_from_user(&seven_tv_user(sub, Value::Null)).unwrap();
        assert!(set.is_some() && st.paint_id.is_none() && st.badge_id.is_none());
        // a lapsed subscriber keeps the set, but 7TV no longer draws it
        let lapsed = json!(["01G68MMQFR0007J6GNM9E2M0TM"]);
        let (st, set) = style_from_user(&seven_tv_user(lapsed.clone(), style)).unwrap();
        assert!(set.is_none() && st.paint_id.is_some());
        assert!(style_from_user(&seven_tv_user(lapsed, Value::Null)).is_none());
        // no 7TV user at all
        assert!(style_from_user(&json!({"id": "1", "user": null})).is_none());
    }

    #[test]
    fn defs_parse() {
        let d = defs_from_raw(&json!({"paints": [{"id": "p", "name": "n", "function": "LINEAR_GRADIENT", "color": null, "angle": 90, "repeat": false, "image_url": null, "stops": [{"at": 0, "color": -1}]}], "badges": [{"id": "b", "name": "B", "tooltip": "tip"}]}));
        assert_eq!(d.paints["p"].stops.len(), 1);
        assert_eq!(d.badges["b"].tooltip, "tip");
    }
}

#[cfg(test)]
mod book_tests {
    use super::*;

    #[test]
    fn a_chatter_is_asked_once_and_at_most_three_run_at_a_time() {
        let b = StyleBook::default();
        assert!(matches!(b.want("1"), Want::Queued));
        assert!(matches!(b.want("1"), Want::Waiting));
        for k in ["2", "3", "4"] {
            assert!(matches!(b.want(k), Want::Queued));
        }
        let jobs: Vec<String> = std::iter::from_fn(|| b.next_job()).collect();
        assert_eq!(jobs, vec!["1", "2", "3"], "the fourth waits for a free slot");
        assert!(b.finish("1", Some(Some(ChatterStyle { paint_id: Some("p".into()), ..Default::default() }))).is_none());
        assert!(matches!(b.want("1"), Want::Known(Some(s)) if s.paint_id.as_deref() == Some("p")));
        assert_eq!(b.next_job().as_deref(), Some("4"));
    }

    #[test]
    fn a_failed_lookup_is_retried_later_and_a_styleless_chatter_is_remembered() {
        let b = StyleBook::default();
        b.want("1");
        b.want("2");
        let (a, c) = (b.next_job().unwrap(), b.next_job().unwrap());
        assert_eq!(b.finish(&a, None), Some(std::time::Duration::from_secs(5)));
        assert!(matches!(b.want(&a), Want::Waiting), "still pending while it waits");
        b.requeue(&a);
        assert!(b.finish(&c, Some(None)).is_none());
        assert!(matches!(b.want(&c), Want::Known(None)));
        assert_eq!(b.next_job().as_deref(), Some(a.as_str()));
        assert_eq!(b.finish(&a, None), Some(std::time::Duration::from_secs(15)));
    }
}
