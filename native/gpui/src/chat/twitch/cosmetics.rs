//! Port of src/renderer/chat/twitch/cosmetics.ts: 7TV cosmetic definitions (paints, badges) and each chatter's
//! style. `use7TvStyle` (a React hook) becomes `Cosmetics::style_of`, which answers from the cache and starts a
//! background fetch (at most 3 in flight) for an unknown chatter; `on_style` listeners then refresh the rows.
//!
//! Differences: a painted name is drawn whole by `chat/paint_text.rs` (DirectWrite/Direct2D mask filled with the CSS
//! gradient / image and the paint's drop-shadows); `paint_color_at` (per character colours) remains as the fallback
//! and for the /me message body.
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use serde_json::{json, Value};

use super::emotes::{read_cache, write_cache};
use crate::chat::net::Net;

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
}

const DEFS_KEY: &str = "emotes:7tv:cosmetics:global:v2";
const DEFS_TTL_MS: i64 = 6 * 3_600_000;
const GQL_QUERY: &str = "{cosmetics{paints{id name function color angle shape repeat image_url stops{at color}shadows{x_offset y_offset radius color}}badges{id name tooltip}}}";

/// Paint colours arrive as signed 32-bit RGBA ints; reinterpret as unsigned.
fn to_u32(v: &Value) -> Option<u32> {
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

/// `None`: the lookup failed (network, 5xx, rate limit) and is worth retrying; `Some(None)`: 7TV answered,
/// and the chatter has no style (or no 7TV account).
async fn fetch_chatter_style(client: &reqwest::Client, user_id: &str) -> Option<Option<ChatterStyle>> {
    let r = client.get(format!("https://7tv.io/v3/users/twitch/{user_id}")).send().await.ok()?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Some(None);
    }
    if !r.status().is_success() {
        return None;
    }
    let d: Value = r.json().await.ok()?;
    let Some(st) = d.pointer("/user/style").filter(|s| !s.is_null()) else { return Some(None) };
    Some(Some(ChatterStyle {
        color: st.get("color").and_then(|c| c.as_i64()),
        paint_id: st.get("paint_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
        badge_id: st.get("badge_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
    }))
}

/// Wait before retrying a failed lookup: 5 s, 15 s, 60 s, then every 5 minutes.
fn retry_delay(failures: u32) -> std::time::Duration {
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

type StyleListener = Box<dyn Fn(&str)>;

/// UI-thread state of all 7TV cosmetics lookups.
pub struct CosmeticsState {
    pub defs: Option<Arc<SevenTvDefs>>,
    styles: HashMap<String, Option<ChatterStyle>>,
    pending: HashSet<String>,
    queue: VecDeque<String>,
    active: usize,
    listeners: Vec<StyleListener>,
    defs_requested: bool,
    /// failed lookups so far, per chatter (cleared on an answer); `defs_failures` for the definitions
    failures: HashMap<String, u32>,
    defs_failures: u32,
}

#[derive(Clone)]
pub struct Cosmetics(pub Rc<RefCell<CosmeticsState>>);

pub enum StyleLookup {
    Unknown,
    Known(Option<ChatterStyle>),
}

impl Cosmetics {
    pub fn new() -> Cosmetics {
        // dev: `AGU_PAINT_FIXTURE=file.json` ({"defs": {paints}, "users": [{id, name, paint_id}]}) stands in for 7TV
        let fixture = paint_fixture();
        let mut styles = HashMap::new();
        if let Some(f) = &fixture {
            for u in f.get("users").and_then(|u| u.as_array()).into_iter().flatten() {
                let id = u.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let paint_id = u.get("paint_id").and_then(|v| v.as_str()).map(|s| s.to_string());
                styles.insert(id, Some(ChatterStyle { color: None, paint_id, badge_id: None }));
            }
        }
        Cosmetics(Rc::new(RefCell::new(CosmeticsState {
            defs: fixture.as_ref().and_then(|f| f.get("defs")).map(|d| Arc::new(defs_from_raw(d))),
            styles,
            pending: HashSet::new(),
            queue: VecDeque::new(),
            active: 0,
            listeners: Vec::new(),
            defs_requested: false,
            failures: HashMap::new(),
            defs_failures: 0,
        })))
    }

    pub fn on_style(&self, f: impl Fn(&str) + 'static) {
        self.0.borrow_mut().listeners.push(Box::new(f));
    }

    pub fn defs(&self) -> Option<Arc<SevenTvDefs>> {
        self.0.borrow().defs.clone()
    }

    /// Cached style of a chatter; an unknown one is queued for fetching (the listeners hear about the answer).
    pub fn style_of(&self, user_id: &str, net: &Arc<Net>, cx: &mut gpui::App) -> StyleLookup {
        if user_id.is_empty() {
            return StyleLookup::Known(None);
        }
        {
            let s = self.0.borrow();
            if let Some(v) = s.styles.get(user_id) {
                return StyleLookup::Known(v.clone());
            }
            if s.pending.contains(user_id) {
                return StyleLookup::Unknown;
            }
        }
        self.0.borrow_mut().pending.insert(user_id.to_string());
        self.0.borrow_mut().queue.push_back(user_id.to_string());
        self.pump(net, cx);
        StyleLookup::Unknown
    }

    /// The listeners hear this id when the definitions arrive: every painted row has to be redrawn.
    pub const DEFS_ARRIVED: &'static str = "";

    /// Start the fetch of the paint / badge definitions; a failed fetch is retried (`retry_delay`).
    pub fn ensure_defs(&self, net: &Arc<Net>, cx: &mut gpui::App) {
        if std::mem::replace(&mut self.0.borrow_mut().defs_requested, true) || self.0.borrow().defs.is_some() {
            return;
        }
        let this = self.clone();
        let netc = net.clone();
        let client = net.client.clone();
        let task = net.run(async move { fetch_seven_tv_defs(&client).await.map(|d| defs_from_raw(&d)) });
        cx.spawn(async move |cx| {
            if let Some(Some(defs)) = task.await {
                let _ = cx.update(|cx| {
                    this.0.borrow_mut().defs = Some(Arc::new(defs));
                    this.notify(Self::DEFS_ARRIVED);
                    cx.refresh_windows();
                });
                return;
            }
            let wait = {
                let mut s = this.0.borrow_mut();
                s.defs_failures += 1;
                retry_delay(s.defs_failures)
            };
            cx.background_executor().timer(wait).await;
            let _ = cx.update(|cx| {
                this.0.borrow_mut().defs_requested = false;
                this.ensure_defs(&netc, cx);
            });
        })
        .detach();
    }

    fn notify(&self, uid: &str) {
        let st = self.0.borrow();
        for l in st.listeners.iter() {
            l(uid);
        }
    }

    fn pump(&self, net: &Arc<Net>, cx: &mut gpui::App) {
        while self.0.borrow().active < 3 {
            let Some(uid) = self.0.borrow_mut().queue.pop_front() else { break };
            self.0.borrow_mut().active += 1;
            let this = self.clone();
            let netc = net.clone();
            let client = net.client.clone();
            let id = uid.clone();
            let task = net.run(async move { fetch_chatter_style(&client, &id).await });
            cx.spawn(async move |cx| {
                // An answer (a style, or none: those are cached so repeat lookups don't refire) is kept; a
                // failed lookup stays pending and is queued again after a wait, so a network blip doesn't
                // take a chatter's paint and badge away for the whole session.
                match task.await.flatten() {
                    Some(style) => {
                        let _ = cx.update(|cx| {
                            {
                                let mut s = this.0.borrow_mut();
                                s.active -= 1;
                                s.pending.remove(&uid);
                                s.failures.remove(&uid);
                                s.styles.insert(uid.clone(), style);
                            }
                            this.notify(&uid);
                            this.pump(&netc, cx);
                            cx.refresh_windows();
                        });
                    }
                    None => {
                        let wait = {
                            let mut s = this.0.borrow_mut();
                            s.active -= 1;
                            let n = s.failures.entry(uid.clone()).or_insert(0);
                            *n += 1;
                            retry_delay(*n)
                        };
                        let _ = cx.update(|cx| this.pump(&netc, cx));
                        cx.background_executor().timer(wait).await;
                        let _ = cx.update(|cx| {
                            this.0.borrow_mut().queue.push_back(uid.clone());
                            this.pump(&netc, cx);
                        });
                    }
                }
            })
            .detach();
        }
    }
}

impl Default for Cosmetics {
    fn default() -> Self {
        Self::new()
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
    fn defs_parse() {
        let d = defs_from_raw(&json!({"paints": [{"id": "p", "name": "n", "function": "LINEAR_GRADIENT", "color": null, "angle": 90, "repeat": false, "image_url": null, "stops": [{"at": 0, "color": -1}]}], "badges": [{"id": "b", "name": "B", "tooltip": "tip"}]}));
        assert_eq!(d.paints["p"].stops.len(), 1);
        assert_eq!(d.badges["b"].tooltip, "tip");
    }
}
