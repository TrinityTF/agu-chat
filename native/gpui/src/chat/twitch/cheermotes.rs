//! Twitch cheermotes (`GET helix/bits/cheermotes`, relayed as `/api/chat/cheermotes`). Only a message that carries
//! a `bits` tag gets them: a word that is a cheermote prefix followed by digits ("Cheer100", case-insensitive)
//! becomes the tier's image plus the amount in the tier's colour, like twitch.tv draws it.
//! The Electron app never had this.
use std::sync::Arc;

use agu_chat_core::protocol::{AguBridge, SourceRef};
use serde_json::Value;

use super::emotes::{flush_cache, read_cache, write_cache, EMOTE_CHANNEL_TTL_MS};
use super::parts::{split_keep_ws, MsgPart};
use crate::chat::mod_caps::relay_api;

#[derive(Clone, Debug, PartialEq)]
pub struct Tier {
    pub min_bits: u64,
    /// `#rrggbb`
    pub color: String,
    /// Dark-theme image URLs (any scale; `url` swaps in the wanted one).
    pub animated: String,
    pub still: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Prefix {
    /// Lower-cased for matching.
    pub key: String,
    pub name: String,
    /// Ascending by `min_bits`.
    pub tiers: Vec<Tier>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cheermotes {
    pub prefixes: Vec<Prefix>,
}

/// Image scale for an emote box of `px` height (the API offers 1, 1.5, 2, 3, 4).
fn scale_for(px: f32) -> &'static str {
    if px >= 30.0 {
        "4"
    } else if px >= 20.0 {
        "3"
    } else {
        "2"
    }
}

/// Helix answers `{ data: [{ prefix, tiers: [{ min_bits, color, images: { dark: { animated: {"1": url, ..}, static: {..} } } }] }] }`.
pub fn parse_cheermotes(v: &Value) -> Cheermotes {
    let list = v.get("data").and_then(|d| d.as_array()).or_else(|| v.as_array());
    let mut prefixes = Vec::new();
    for g in list.into_iter().flatten() {
        let Some(name) = g.get("prefix").and_then(|p| p.as_str()).filter(|p| !p.is_empty()) else { continue };
        let mut tiers: Vec<Tier> = Vec::new();
        for t in g.get("tiers").and_then(|t| t.as_array()).into_iter().flatten() {
            let Some(min_bits) = t.get("min_bits").and_then(|m| m.as_u64()) else { continue };
            let dark = t.get("images").and_then(|i| i.get("dark"));
            let pick = |kind: &str| dark.and_then(|d| d.get(kind)).and_then(|k| k.get("2").or_else(|| k.get("1"))).and_then(|u| u.as_str()).unwrap_or("").to_string();
            let (animated, still) = (pick("animated"), pick("static"));
            if animated.is_empty() && still.is_empty() {
                continue;
            }
            tiers.push(Tier { min_bits, color: t.get("color").and_then(|c| c.as_str()).unwrap_or("#979797").to_string(), animated, still });
        }
        if tiers.is_empty() {
            continue;
        }
        tiers.sort_by_key(|t| t.min_bits);
        prefixes.push(Prefix { key: name.to_lowercase(), name: name.to_string(), tiers });
    }
    Cheermotes { prefixes }
}

impl Tier {
    /// The tier's image at `px` emote height; the static one when animations are off. The Helix URLs end in the
    /// scale (`.../animated/100/2`), so the wanted scale replaces it.
    pub fn url(&self, animate: bool, px: f32) -> String {
        let base = if animate && !self.animated.is_empty() || self.still.is_empty() { &self.animated } else { &self.still };
        let Some((head, tail)) = base.rsplit_once('/') else { return base.clone() };
        // `2.gif` / `2.png` (or a bare `2`): swap the scale, keep the extension
        let (stem, ext) = match tail.rsplit_once('.') {
            Some((st, ex)) if ex.chars().all(|c| c.is_ascii_alphabetic()) && !ex.is_empty() => (st, format!(".{ex}")),
            _ => (tail, String::new()),
        };
        if stem.is_empty() || !stem.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return base.clone();
        }
        format!("{head}/{}{ext}", scale_for(px))
    }
}

/// The highest tier whose `min_bits` is at most `amount`.
pub fn pick_tier(tiers: &[Tier], amount: u64) -> Option<&Tier> {
    tiers.iter().filter(|t| t.min_bits <= amount).max_by_key(|t| t.min_bits)
}

/// `Cheer100` -> (prefix, 100): a known prefix (any case) immediately followed by digits, and nothing else in the word.
pub fn match_token<'a>(word: &str, cm: &'a Cheermotes) -> Option<(&'a Prefix, u64)> {
    let digits_at = word.find(|c: char| c.is_ascii_digit())?;
    let (head, num) = word.split_at(digits_at);
    if !num.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let head = head.to_lowercase();
    let p = cm.prefixes.iter().find(|p| p.key == head)?;
    let amount: u64 = num.parse().ok().filter(|a| *a > 0)?;
    Some((p, amount))
}

/// Replace cheermote words in the text parts of a bits message.
pub fn apply(parts: Vec<MsgPart>, cm: &Cheermotes, animate: bool, px: f32) -> Vec<MsgPart> {
    if cm.prefixes.is_empty() {
        return parts;
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        let MsgPart::Text(t) = &part else {
            out.push(part);
            continue;
        };
        if !t.chars().any(|c| c.is_ascii_digit()) {
            out.push(part);
            continue;
        }
        let mut pending = String::new();
        for w in split_keep_ws(t) {
            match match_token(w, cm).and_then(|(p, n)| pick_tier(&p.tiers, n).map(|t| (p, n, t))) {
                Some((p, n, tier)) => {
                    if !pending.is_empty() {
                        out.push(MsgPart::Text(std::mem::take(&mut pending)));
                    }
                    out.push(MsgPart::Cheer { name: w.to_string(), amount: n, color: tier.color.clone(), url: tier.url(animate, px), prefix: p.name.clone() });
                }
                None => pending.push_str(w),
            }
        }
        if !pending.is_empty() {
            out.push(MsgPart::Text(pending));
        }
    }
    out
}

/// Cheermotes for a source: the emotes cache first (an hour per channel), then the relay. Logged out the relay
/// has nothing, so the answer is None and Bits messages stay plain text.
pub async fn load_cheermotes(bridge: &Arc<dyn AguBridge>, source: &SourceRef, sk: &str) -> Option<Cheermotes> {
    let key = format!("cheermotes:{sk}");
    if let Some(c) = read_cache(&key, EMOTE_CHANNEL_TTL_MS) {
        let cm = parse_cheermotes(&c);
        if !cm.prefixes.is_empty() {
            return Some(cm);
        }
    }
    let r = relay_api(bridge, source, "/api/chat/cheermotes", None, None).await;
    if !r.ok {
        return None;
    }
    let cm = parse_cheermotes(&r.body);
    if cm.prefixes.is_empty() {
        return None;
    }
    write_cache(&key, &r.body);
    flush_cache();
    Some(cm)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Cheermotes {
        let img = |k: &str, n: &str| {
            json!({"dark": {
                "animated": {"1": format!("https://cdn/{k}/dark/animated/{n}/1.gif"), "2": format!("https://cdn/{k}/dark/animated/{n}/2.gif")},
                "static": {"1": format!("https://cdn/{k}/dark/static/{n}/1.png"), "2": format!("https://cdn/{k}/dark/static/{n}/2.png")}}})
        };
        let tiers = |k: &str| {
            json!([
                {"min_bits": 10000, "color": "#f43021", "images": img(k, "10000")},
                {"min_bits": 1, "color": "#979797", "images": img(k, "1")},
                {"min_bits": 100, "color": "#9c3ee8", "images": img(k, "100")},
                {"min_bits": 1000, "color": "#1db2a5", "images": img(k, "1000")},
                {"min_bits": 5000, "color": "#0099fe", "images": img(k, "5000")},
            ])
        };
        parse_cheermotes(&json!({"data": [{"prefix": "Cheer", "tiers": tiers("cheer")}, {"prefix": "BibleThump", "tiers": tiers("bt")}, {"prefix": "MyCustom", "tiers": tiers("mc")}]}))
    }

    #[test]
    fn tier_pick() {
        let cm = sample();
        let t = &cm.prefixes[0].tiers;
        let at = |n| pick_tier(t, n).map(|t| t.min_bits);
        assert_eq!(at(0), None);
        assert_eq!(at(1), Some(1));
        assert_eq!(at(99), Some(1));
        assert_eq!(at(100), Some(100));
        assert_eq!(at(999), Some(100));
        assert_eq!(at(1000), Some(1000));
        assert_eq!(at(4999), Some(1000));
        assert_eq!(at(5000), Some(5000));
        assert_eq!(at(10000), Some(10000));
        assert_eq!(at(123456), Some(10000));
    }

    #[test]
    fn token_matching() {
        let cm = sample();
        let m = |w| match_token(w, &cm).map(|(p, n)| (p.name.clone(), n));
        assert_eq!(m("Cheer100"), Some(("Cheer".into(), 100)));
        assert_eq!(m("cHeEr5"), Some(("Cheer".into(), 5)));
        assert_eq!(m("biblethump25"), Some(("BibleThump".into(), 25)));
        assert_eq!(m("MYCUSTOM1000"), Some(("MyCustom".into(), 1000)));
        for bad in ["Cheer", "Cheer100x", "Cheer10!", "xCheer100", "Cheers100", "Cheer0", "100", "Cheer-5", "Cheer100,", "Kappa100", "Cheer1e3"] {
            assert!(m(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn replaces_words_in_text_parts() {
        let cm = sample();
        let p = apply(vec![MsgPart::Text("Cheer100 told you, ogre mound".into())], &cm, true, 25.0);
        assert_eq!(p.len(), 2);
        assert!(matches!(&p[0], MsgPart::Cheer { amount: 100, color, .. } if color == "#9c3ee8"));
        assert_eq!(p[1], MsgPart::Text(" told you, ogre mound".into()));
        let p = apply(vec![MsgPart::Text("a Cheer1 b Kappa2 Cheer5000".into())], &cm, true, 25.0);
        assert_eq!(p.len(), 4);
        assert_eq!(p[0], MsgPart::Text("a ".into()));
        assert!(matches!(&p[1], MsgPart::Cheer { amount: 1, .. }));
        assert_eq!(p[2], MsgPart::Text(" b Kappa2 ".into()));
        assert!(matches!(&p[3], MsgPart::Cheer { amount: 5000, .. }));
        // no prefix list -> untouched
        let t = vec![MsgPart::Text("Cheer100".into())];
        assert_eq!(apply(t.clone(), &Cheermotes::default(), true, 25.0), t);
    }

    #[test]
    fn animation_setting_and_scale() {
        let cm = sample();
        let t = &cm.prefixes[0].tiers[1];
        assert_eq!(t.min_bits, 100);
        assert_eq!(t.url(true, 25.0), "https://cdn/cheer/dark/animated/100/3.gif");
        assert_eq!(t.url(false, 25.0), "https://cdn/cheer/dark/static/100/3.png");
        assert_eq!(t.url(true, 19.0), "https://cdn/cheer/dark/animated/100/2.gif");
        assert_eq!(t.url(true, 40.0), "https://cdn/cheer/dark/animated/100/4.gif");
    }
}
