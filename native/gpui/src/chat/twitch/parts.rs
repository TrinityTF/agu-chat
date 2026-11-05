//! Port of src/renderer/chat/twitch/parts.ts: Twitch emote/GIF tag parsing and message -> renderable parts.
use std::collections::HashMap;

use super::emotes::{EmoteProvider, EmoteSets};
use crate::chat::chat_text::{as_link, mention_word};

#[derive(Clone, Debug, PartialEq)]
pub struct TwitchEmoteRange {
    pub id: String,
    pub start: usize,
    pub end: usize,
}

pub fn parse_twitch_emote_tag(tag: &str) -> Vec<TwitchEmoteRange> {
    let mut out = Vec::new();
    if tag.is_empty() {
        return out;
    }
    for part in tag.split('/') {
        let mut it = part.splitn(2, ':');
        let (id, ranges) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if id.is_empty() || ranges.is_empty() {
            continue;
        }
        for r in ranges.split(',') {
            let mut se = r.split('-');
            if let (Some(Ok(s)), Some(Ok(e))) = (se.next().map(|x| x.parse::<usize>()), se.next().map(|x| x.parse::<usize>())) {
                out.push(TwitchEmoteRange { id: id.to_string(), start: s, end: e });
            }
        }
    }
    out.sort_by_key(|r| r.start);
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct GifRange {
    pub start: usize,
    pub end: usize,
    pub id: String,
    pub url: String,
}

fn starts_with_range(frag: &str) -> bool {
    // ^\d+-\d+\|
    let a = frag.chars().take_while(|c| c.is_ascii_digit()).count();
    if a == 0 || !frag[a..].starts_with('-') {
        return false;
    }
    let rest = &frag[a + 1..];
    let b = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    b > 0 && rest[b..].starts_with('|')
}

/// Split a `gifs` tag into entries: a URL may carry commas, so a fragment that doesn't begin a fresh
/// `<digits>-<digits>|` entry is re-joined to the previous one.
pub fn split_gif_entries(tag: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    for frag in tag.split(',') {
        if starts_with_range(frag) || entries.is_empty() {
            entries.push(frag.to_string());
        } else {
            let last = entries.last_mut().unwrap();
            last.push(',');
            last.push_str(frag);
        }
    }
    entries
}

/// Parse Twitch's `gifs` tag: `<start>-<end>|<gifID>|<gifURL>`, comma-separated.
pub fn parse_twitch_gif_tag(tag: &str) -> Vec<GifRange> {
    let mut out = Vec::new();
    if tag.is_empty() {
        return out;
    }
    for entry in split_gif_entries(tag) {
        let Some(first) = entry.find('|') else { continue };
        let Some(second) = entry[first + 1..].find('|').map(|i| i + first + 1) else { continue };
        let mut se = entry[..first].split('-');
        let (Some(Ok(s)), Some(Ok(e))) = (se.next().map(|x| x.parse::<usize>()), se.next().map(|x| x.parse::<usize>())) else { continue };
        let url = &entry[second + 1..];
        let l = url.to_lowercase();
        if !(l.starts_with("http://") || l.starts_with("https://")) {
            continue;
        }
        out.push(GifRange { start: s, end: e, id: entry[first + 1..second].to_string(), url: url.to_string() });
    }
    out.sort_by_key(|r| r.start);
    out
}

/// "twitch" is positional (IRC ranges); the rest are name-matched provider emotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmoteSource {
    Twitch,
    Provider(EmoteProvider),
}

#[derive(Clone, Debug, PartialEq)]
pub struct KnownChatter {
    pub name: String,
    pub color: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Overlay {
    pub source: EmoteSource,
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MsgPart {
    Text(String),
    Emote { source: EmoteSource, id: String, name: String, overlays: Vec<Overlay> },
    Mention { login: String, name: String, color: String, user_id: String },
    Link { href: String, text: String },
    Gif { id: String, url: String, alt: String },
}

enum Raw {
    Text(String),
    Emote(String, String),
    Gif(String, String, String),
}

enum Mid {
    Text(String),
    Emote { source: EmoteSource, id: String, name: String, zero_width: bool },
    Mention { login: String, name: String, color: String, user_id: String },
    Link { href: String, text: String },
    Gif(String, String, String),
}

/// Split keeping the whitespace runs, like JS `text.split(/(\s+)/)` (empty strings dropped).
pub fn split_keep_ws(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut in_ws: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let ws = c.is_whitespace();
        match in_ws {
            Some(prev) if prev != ws => {
                out.push(&text[start..i]);
                start = i;
            }
            _ => {}
        }
        in_ws = Some(ws);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

pub fn build_parts(text: &str, emotes_tag: &str, emotes: Option<&EmoteSets>, known: Option<&HashMap<String, KnownChatter>>, gifs_tag: &str) -> Vec<MsgPart> {
    let chars: Vec<char> = text.chars().collect();
    enum R {
        Emote(String),
        Gif(String, String),
    }
    let mut ranges: Vec<(usize, usize, R)> = Vec::new();
    for r in parse_twitch_emote_tag(emotes_tag) {
        ranges.push((r.start, r.end, R::Emote(r.id)));
    }
    for r in parse_twitch_gif_tag(gifs_tag) {
        ranges.push((r.start, r.end, R::Gif(r.id, r.url)));
    }
    ranges.sort_by_key(|r| r.0);

    let slice = |a: usize, b: usize| -> String { chars[a.min(chars.len())..b.min(chars.len())].iter().collect() };
    let mut raw: Vec<Raw> = Vec::new();
    let mut i = 0;
    for (start, end, r) in ranges {
        if start < i {
            continue; // guard against overlapping/out-of-order tag data
        }
        if start > i {
            raw.push(Raw::Text(slice(i, start)));
        }
        let span = slice(start, end + 1);
        match r {
            R::Gif(id, url) => raw.push(Raw::Gif(id, url, span)),
            R::Emote(id) => raw.push(Raw::Emote(id, span)),
        }
        i = end + 1;
    }
    if i < chars.len() {
        raw.push(Raw::Text(slice(i, chars.len())));
    }

    let mut mid: Vec<Mid> = Vec::new();
    for tok in raw {
        match tok {
            Raw::Gif(a, b, c) => mid.push(Mid::Gif(a, b, c)),
            Raw::Emote(id, name) => mid.push(Mid::Emote { source: EmoteSource::Twitch, id, name, zero_width: false }),
            Raw::Text(t) => {
                for w in split_keep_ws(&t) {
                    if w.chars().all(|c| c.is_whitespace()) {
                        mid.push(Mid::Text(w.to_string()));
                        continue;
                    }
                    if let Some(link) = as_link(w) {
                        if !link.lead.is_empty() {
                            mid.push(Mid::Text(link.lead));
                        }
                        mid.push(Mid::Link { href: link.href, text: link.text });
                        if !link.trail.is_empty() {
                            mid.push(Mid::Text(link.trail));
                        }
                        continue;
                    }
                    if let Some(e) = emotes.and_then(|s| s.all.get(w)) {
                        mid.push(Mid::Emote { source: EmoteSource::Provider(e.provider), id: e.id.clone(), name: e.name.clone(), zero_width: e.zero_width });
                        continue;
                    }
                    // mention chip: the word matches a chatter seen in the buffer (with or without "@")
                    let m = mention_word(w);
                    let k = m.and_then(|(_, name, _)| known.and_then(|k| k.get(&name.to_lowercase())));
                    if let (Some((lead, name, trail)), Some(k)) = (m, k) {
                        let lead = lead.trim_end_matches('@');
                        if !lead.is_empty() {
                            mid.push(Mid::Text(lead.to_string()));
                        }
                        mid.push(Mid::Mention { login: name.to_lowercase(), name: k.name.clone(), color: k.color.clone(), user_id: k.id.clone() });
                        if !trail.is_empty() {
                            mid.push(Mid::Text(trail.to_string()));
                        }
                    } else {
                        mid.push(Mid::Text(w.to_string()));
                    }
                }
            }
        }
    }

    let mut out: Vec<MsgPart> = Vec::new();
    for tok in mid {
        match tok {
            Mid::Gif(id, url, alt) => out.push(MsgPart::Gif { id, url, alt }),
            Mid::Text(t) => out.push(MsgPart::Text(t)),
            Mid::Link { href, text } => out.push(MsgPart::Link { href, text }),
            Mid::Mention { login, name, color, user_id } => out.push(MsgPart::Mention { login, name, color, user_id }),
            Mid::Emote { source, id, name, zero_width } => {
                if source != EmoteSource::Twitch && zero_width {
                    // A zero-width emote overlays the PRECEDING emote; walk back past exactly one connecting
                    // whitespace-only text token and drop it.
                    let mut prev = out.len() as isize - 1;
                    if prev >= 1 {
                        if let (MsgPart::Text(t), MsgPart::Emote { .. }) = (&out[prev as usize], &out[prev as usize - 1]) {
                            if t.chars().all(|c| c.is_whitespace()) {
                                out.remove(prev as usize);
                                prev -= 1;
                            }
                        }
                    }
                    if prev >= 0 {
                        if let MsgPart::Emote { overlays, .. } = &mut out[prev as usize] {
                            overlays.push(Overlay { source, id, name });
                            continue;
                        }
                    }
                }
                out.push(MsgPart::Emote { source, id, name, overlays: Vec::new() });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emote_tag() {
        let r = parse_twitch_emote_tag("25:0-4,11-15/354:6-10");
        assert_eq!(r.iter().map(|x| (x.id.as_str(), x.start, x.end)).collect::<Vec<_>>(), vec![("25", 0, 4), ("354", 6, 10), ("25", 11, 15)]);
    }

    #[test]
    fn gif_tag_with_commas_in_url() {
        let g = parse_twitch_gif_tag("0-5|id1|https://x.com/a?b=1,2,6-9|id2|https://y.com/z");
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].url, "https://x.com/a?b=1,2");
        assert!(parse_twitch_gif_tag("0-5|id|javascript:x").is_empty());
    }

    #[test]
    fn parts_text_emote_link() {
        let p = build_parts("Kappa hi twitch.tv/a", "25:0-4", None, None, "");
        assert!(matches!(&p[0], MsgPart::Emote { name, .. } if name == "Kappa"));
        assert!(p.iter().any(|x| matches!(x, MsgPart::Link { .. })));
        assert_eq!(p[1], MsgPart::Text(" ".into()));
    }

    #[test]
    fn mention_chip_and_punctuation() {
        let mut k = HashMap::new();
        k.insert("bob".to_string(), KnownChatter { name: "Bob".into(), color: "#fff".into(), id: "1".into() });
        let p = build_parts("@bob, hi", "", None, Some(&k), "");
        assert_eq!(p[0], MsgPart::Mention { login: "bob".into(), name: "Bob".into(), color: "#fff".into(), user_id: "1".into() });
        assert_eq!(p[1], MsgPart::Text(",".into()));
    }

    #[test]
    fn emote_ranges_count_code_points() {
        let p = build_parts("😀 Kappa", "25:2-6", None, None, "");
        assert!(matches!(&p[2], MsgPart::Emote { name, .. } if name == "Kappa"));
    }
}
