//! Port of src/renderer/chat/twitch/cosmetics.ts: 7TV cosmetic definitions (paints, badges) and each chatter's
//! style. `use7TvStyle` (a React hook) becomes `Cosmetics::style_of`, which answers from the cache and starts a
//! background fetch (at most 3 in flight) for an unknown chatter; `on_style` listeners then refresh the rows.
//!
//! Differences: CSS `background-clip: text` gradients cannot be painted per glyph here, so a paint is
//! evaluated per character along the name (`paint_color_at`): linear/radial/repeating gradients and solid
//! colours work, image-textured paints fall back to their gradient underlay (or the plain name colour), and
//! the paints' drop-shadows are not drawn.
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
    pub image_url: Option<String>,
    pub stops: Vec<PaintStop>,
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

const DEFS_KEY: &str = "emotes:7tv:cosmetics:global";
const DEFS_TTL_MS: i64 = 6 * 3_600_000;
const GQL_QUERY: &str = "{cosmetics{paints{id name function color angle repeat image_url stops{at color}shadows{x_offset y_offset radius color}}badges{id name tooltip}}}";

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
            image_url: p.get("image_url").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()),
            stops: p
                .get("stops")
                .and_then(|a| a.as_array())
                .into_iter()
                .flatten()
                .map(|st| PaintStop { at: st.get("at").and_then(|a| a.as_f64()).unwrap_or(0.0) as f32, color: st.get("color").and_then(to_u32).unwrap_or(0) })
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

async fn fetch_chatter_style(client: &reqwest::Client, user_id: &str) -> Option<ChatterStyle> {
    let r = client.get(format!("https://7tv.io/v3/users/twitch/{user_id}")).send().await.ok()?;
    if !r.status().is_success() {
        return None;
    }
    let d: Value = r.json().await.ok()?;
    let st = d.pointer("/user/style").filter(|s| !s.is_null())?;
    Some(ChatterStyle {
        color: st.get("color").and_then(|c| c.as_i64()),
        paint_id: st.get("paint_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
        badge_id: st.get("badge_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
    })
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
}

#[derive(Clone)]
pub struct Cosmetics(pub Rc<RefCell<CosmeticsState>>);

pub enum StyleLookup {
    Unknown,
    Known(Option<ChatterStyle>),
}

impl Cosmetics {
    pub fn new() -> Cosmetics {
        Cosmetics(Rc::new(RefCell::new(CosmeticsState {
            defs: None,
            styles: HashMap::new(),
            pending: HashSet::new(),
            queue: VecDeque::new(),
            active: 0,
            listeners: Vec::new(),
            defs_requested: false,
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

    /// Start the one-off fetch of the paint / badge definitions.
    pub fn ensure_defs(&self, net: &Arc<Net>, cx: &mut gpui::App) {
        if std::mem::replace(&mut self.0.borrow_mut().defs_requested, true) {
            return;
        }
        let this = self.clone();
        let client = net.client.clone();
        let task = net.run(async move { fetch_seven_tv_defs(&client).await.map(|d| defs_from_raw(&d)) });
        cx.spawn(async move |cx| {
            if let Some(Some(defs)) = task.await {
                let _ = cx.update(|_| this.0.borrow_mut().defs = Some(Arc::new(defs)));
                let _ = cx.update(|cx| cx.refresh_windows());
            }
        })
        .detach();
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
                // `None` = no style / 404 / failure; negatives are cached so repeat lookups don't refire.
                let style = task.await.flatten();
                let _ = cx.update(|cx| {
                    {
                        let mut s = this.0.borrow_mut();
                        s.active -= 1;
                        s.pending.remove(&uid);
                        s.styles.insert(uid.clone(), style);
                    }
                    let st = this.0.borrow();
                    for l in st.listeners.iter() {
                        l(&uid);
                    }
                    drop(st);
                    this.pump(&netc, cx);
                    cx.refresh_windows();
                });
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
