//! Port of src/renderer/chat/twitch/parts.ts: Twitch emote/GIF tag parsing and message -> renderable parts.
use std::collections::HashMap;

use super::emoji::{split_emoji, Seg};
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

/// "twitch" is positional (IRC ranges) and "kick" is Kick's own `[emote:<id>:<name>]` markup; the rest are
/// name-matched provider emotes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmoteSource {
    Twitch,
    Kick,
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
    /// One Unicode emoji (a whole grapheme), drawn as text with the emote popup; zero-width emotes stack on it.
    Emoji { text: String, overlays: Vec<Overlay> },
    Mention { login: String, name: String, color: String, user_id: String },
    Link { href: String, text: String },
    Gif { id: String, url: String, alt: String },
    /// A cheermote word of a bits message (`cheermotes::apply`): tier image plus the amount in the tier colour.
    Cheer { name: String, prefix: String, amount: u64, color: String, url: String },
}

enum Raw {
    Text(String),
    Emote(String, String),
    Gif(String, String, String),
}

enum Mid {
    Text(String),
    Emote { source: EmoteSource, id: String, name: String, zero_width: bool },
    Emoji(String),
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
                        for seg in split_emoji(w) {
                            mid.push(match seg {
                                Seg::Text(t) => Mid::Text(t.to_string()),
                                Seg::Emoji(e) => Mid::Emoji(e.to_string()),
                            });
                        }
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
            Mid::Emoji(e) => out.push(MsgPart::Emoji { text: e, overlays: Vec::new() }),
            Mid::Link { href, text } => out.push(MsgPart::Link { href, text }),
            Mid::Mention { login, name, color, user_id } => out.push(MsgPart::Mention { login, name, color, user_id }),
            Mid::Emote { source, id, name, zero_width } => {
                if matches!(source, EmoteSource::Provider(_)) && zero_width {
                    // A zero-width emote overlays the PRECEDING emote or emoji; walk back past exactly one
                    // connecting whitespace-only text token and drop it.
                    let mut prev = out.len() as isize - 1;
                    if prev >= 1 {
                        if let (MsgPart::Text(t), MsgPart::Emote { .. } | MsgPart::Emoji { .. }) = (&out[prev as usize], &out[prev as usize - 1]) {
                            if t.chars().all(|c| c.is_whitespace()) {
                                out.remove(prev as usize);
                                prev -= 1;
                            }
                        }
                    }
                    if prev >= 0 {
                        if let MsgPart::Emote { overlays, .. } | MsgPart::Emoji { overlays, .. } = &mut out[prev as usize] {
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

/// Kick's emote markup split out of a message: (is emote, emote id or the text, emote name), in order.
pub fn split_kick(text: &str) -> Vec<(bool, String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[emote:") {
        let tail = &rest[i + 7..];
        let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && tail[digits..].starts_with(':') {
            let after = &tail[digits + 1..];
            if let Some(close) = after.find(']') {
                let name = &after[..close];
                if (1..=80).contains(&name.chars().count()) {
                    if i > 0 {
                        out.push((false, rest[..i].to_string(), String::new()));
                    }
                    out.push((true, tail[..digits].to_string(), name.to_string()));
                    rest = &after[close + 1..];
                    continue;
                }
            }
        }
        out.push((false, rest[..i + 7].to_string(), String::new()));
        rest = tail;
    }
    if !rest.is_empty() {
        out.push((false, rest.to_string(), String::new()));
    }
    out
}

/// A Kick message as parts. Kick's own `[emote:<id>:<name>]` markup becomes a Kick emote, and in the text around it
/// every word that names an emote of `emotes` (the channel's Kick-linked 7TV set and 7TV global) becomes that emote,
/// like the 7TV extension does on kick.com; a zero-width one stacks on the emote before it, as on Twitch. The rest
/// stays text, one part per run between emotes (the row draws links and mentions in it).
///
/// Kick's own emotes come from markup and 7TV's from plain words, so a Kick emote always wins over a 7TV emote that
/// shares its name.
pub fn build_kick_parts(text: &str, emotes: Option<&EmoteSets>) -> Vec<MsgPart> {
    // the markup splitter leaves a broken "[emote:" as its own text piece: join neighbouring text back together
    let mut segs: Vec<(bool, String, String)> = Vec::new();
    for seg in split_kick(text) {
        match segs.last_mut() {
            Some((false, prev, _)) if !seg.0 => prev.push_str(&seg.1),
            _ => segs.push(seg),
        }
    }
    let mut out: Vec<MsgPart> = Vec::new();
    let push_text = |out: &mut Vec<MsgPart>, t: &str| {
        if t.is_empty() {
            return;
        }
        match out.last_mut() {
            Some(MsgPart::Text(prev)) => prev.push_str(t),
            _ => out.push(MsgPart::Text(t.to_string())),
        }
    };
    for (is_emote, a, name) in segs {
        if is_emote {
            out.push(MsgPart::Emote { source: EmoteSource::Kick, id: a, name, overlays: Vec::new() });
            continue;
        }
        let Some(sets) = emotes.filter(|e| !e.all.is_empty()) else {
            push_text(&mut out, &a);
            continue;
        };
        for w in split_keep_ws(&a) {
            let Some(e) = sets.all.get(w) else {
                push_text(&mut out, w);
                continue;
            };
            let source = EmoteSource::Provider(e.provider);
            if e.zero_width {
                // on the emote right before it, across one run of whitespace (which is dropped), like build_parts
                let ws_between = matches!(out.last(), Some(MsgPart::Text(t)) if t.chars().all(|c| c.is_whitespace()));
                let target = if ws_between { out.len().checked_sub(2) } else { out.len().checked_sub(1) };
                if let Some(i) = target.filter(|i| matches!(out[*i], MsgPart::Emote { .. })) {
                    if ws_between {
                        out.pop();
                    }
                    if let MsgPart::Emote { overlays, .. } = &mut out[i] {
                        overlays.push(Overlay { source, id: e.id.clone(), name: e.name.clone() });
                    }
                    continue;
                }
            }
            out.push(MsgPart::Emote { source, id: e.id.clone(), name: e.name.clone(), overlays: Vec::new() });
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
    fn emoji_become_their_own_parts() {
        let p = build_parts("hi🍟! 🍟", "", None, None, "");
        assert_eq!(
            p,
            vec![
                MsgPart::Text("hi".into()),
                MsgPart::Emoji { text: "🍟".into(), overlays: vec![] },
                MsgPart::Text("!".into()),
                MsgPart::Text(" ".into()),
                MsgPart::Emoji { text: "🍟".into(), overlays: vec![] },
            ]
        );
        // a Twitch emote range after an emoji still counts code points
        let p = build_parts("😀 Kappa", "25:2-6", None, None, "");
        assert_eq!(p[0], MsgPart::Emoji { text: "😀".into(), overlays: vec![] });
    }

    fn seven(entries: &[(&str, &str, bool)]) -> EmoteSets {
        use super::super::emotes::ProviderEmote;
        let mut sets = EmoteSets::default();
        for (id, name, zw) in entries {
            let e = std::sync::Arc::new(ProviderEmote { provider: EmoteProvider::SevenTv, id: id.to_string(), name: name.to_string(), zero_width: *zw, animated: false, urls: Default::default() });
            sets.all.insert(name.to_string(), e);
        }
        sets
    }

    #[test]
    fn kick_messages_match_7tv_words_and_keep_kick_emotes() {
        let sets = seven(&[("lo", "LO", false), ("zw", "notL", true), ("k7", "KEKW", false)]);
        let s7 = EmoteSource::Provider(EmoteProvider::SevenTv);
        let p = build_kick_parts("LO chat [emote:37226:KEKW] KEKW", Some(&sets));
        assert_eq!(
            p,
            vec![
                MsgPart::Emote { source: s7, id: "lo".into(), name: "LO".into(), overlays: vec![] },
                MsgPart::Text(" chat ".into()),
                // Kick's own markup is Kick's emote even where 7TV has one of that name
                MsgPart::Emote { source: EmoteSource::Kick, id: "37226".into(), name: "KEKW".into(), overlays: vec![] },
                MsgPart::Text(" ".into()),
                // the plain word is 7TV's
                MsgPart::Emote { source: s7, id: "k7".into(), name: "KEKW".into(), overlays: vec![] },
            ]
        );
        // case matters, and a word inside other text is not an emote
        assert_eq!(build_kick_parts("lo LOL xLO", Some(&sets)), vec![MsgPart::Text("lo LOL xLO".into())]);
        // without 7TV (off, or not loaded) the text stays text
        assert_eq!(build_kick_parts("LO", None), vec![MsgPart::Text("LO".into())]);
        assert_eq!(build_kick_parts("LO", Some(&EmoteSets::default())), vec![MsgPart::Text("LO".into())]);
        // a broken markup is plain text, one run
        assert_eq!(build_kick_parts("[emote:x:y] LO", None), vec![MsgPart::Text("[emote:x:y] LO".into())]);
        assert_eq!(build_kick_parts("", Some(&sets)), vec![]);
    }

    #[test]
    fn kick_zero_width_7tv_emotes_stack_on_the_emote_before() {
        let sets = seven(&[("lo", "LO", false), ("zw", "notL", true)]);
        let s7 = EmoteSource::Provider(EmoteProvider::SevenTv);
        let ov = vec![Overlay { source: s7, id: "zw".into(), name: "notL".into() }];
        // on a 7TV emote, and on a Kick emote
        assert_eq!(build_kick_parts("LO notL", Some(&sets)), vec![MsgPart::Emote { source: s7, id: "lo".into(), name: "LO".into(), overlays: ov.clone() }]);
        assert_eq!(build_kick_parts("[emote:1:Kappa] notL hi", Some(&sets)), vec![MsgPart::Emote { source: EmoteSource::Kick, id: "1".into(), name: "Kappa".into(), overlays: ov.clone() }, MsgPart::Text(" hi".into())]);
        // after a word, or alone, it is drawn as an emote of its own
        assert_eq!(build_kick_parts("hi notL", Some(&sets)), vec![MsgPart::Text("hi ".into()), MsgPart::Emote { source: s7, id: "zw".into(), name: "notL".into(), overlays: vec![] }]);
        assert_eq!(build_kick_parts("notL", Some(&sets)), vec![MsgPart::Emote { source: s7, id: "zw".into(), name: "notL".into(), overlays: vec![] }]);
    }

    #[test]
    fn zero_width_stacks_on_emotes_and_emoji() {
        let sets = seven(&[("c", "RedCircle", true), ("k", "KEKW", false)]);
        let ov = vec![Overlay { source: EmoteSource::Provider(EmoteProvider::SevenTv), id: "c".into(), name: "RedCircle".into() }];
        // the reported case: "🍟 RedCircle" repeated, each circle on its fries
        let p = build_parts("🍟 RedCircle 🍟 RedCircle", "", Some(&sets), None, "");
        assert_eq!(p, vec![MsgPart::Emoji { text: "🍟".into(), overlays: ov.clone() }, MsgPart::Text(" ".into()), MsgPart::Emoji { text: "🍟".into(), overlays: ov.clone() }]);
        // an emoji glued to text still takes it
        let p = build_parts("lol🍟 RedCircle", "", Some(&sets), None, "");
        assert_eq!(p, vec![MsgPart::Text("lol".into()), MsgPart::Emoji { text: "🍟".into(), overlays: ov.clone() }]);
        // emotes as before
        let p = build_parts("KEKW RedCircle", "", Some(&sets), None, "");
        assert!(matches!(&p[..], [MsgPart::Emote { name, overlays, .. }] if name == "KEKW" && *overlays == ov));
        // after plain text it stays an emote of its own
        let p = build_parts("hi RedCircle", "", Some(&sets), None, "");
        assert!(matches!(&p[..], [MsgPart::Text(_), MsgPart::Text(_), MsgPart::Emote { name, overlays, .. }] if name == "RedCircle" && overlays.is_empty()));
    }

    #[test]
    fn emote_ranges_count_code_points() {
        let p = build_parts("😀 Kappa", "25:2-6", None, None, "");
        assert!(matches!(&p[2], MsgPart::Emote { name, .. } if name == "Kappa"));
    }
}
