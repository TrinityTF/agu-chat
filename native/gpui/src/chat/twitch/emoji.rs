//! Unicode emoji inside message text: found per grapheme so each one becomes its own part. It is still drawn
//! as text (the system's colour emoji font), but as a part of its own it gets the emote hover popup and can be
//! the base a 7TV zero-width emote stacks on. Names come from the `emojis` data (CLDR names, gemoji shortcodes).
use unicode_segmentation::UnicodeSegmentation;

/// One piece of a word: plain text, or a single emoji (a whole grapheme: skin tones, ZWJ sequences and flags
/// stay in one piece).
#[derive(Clone, Debug, PartialEq)]
pub enum Seg<'a> {
    Text(&'a str),
    Emoji(&'a str),
}

/// The emoji data entry of a grapheme. A text-style symbol written without its emoji variation selector (`❤`,
/// `☺`) is looked up with U+FE0F added, so it is named too.
fn lookup(g: &str) -> Option<&'static emojis::Emoji> {
    emojis::get(g).or_else(|| if g.ends_with('\u{fe0f}') { None } else { emojis::get(&format!("{g}\u{fe0f}")) })
}

/// Whether one grapheme is an emoji. Bare digits, `#` and `*` are in the emoji tables (keycap bases) but are
/// ordinary text.
pub fn is_emoji(g: &str) -> bool {
    !g.is_ascii() && lookup(g).is_some()
}

/// `text` cut into text and emoji pieces, in order. A word without emoji comes back as one `Text`.
pub fn split_emoji(text: &str) -> Vec<Seg<'_>> {
    let mut out: Vec<Seg> = Vec::new();
    // fast path: every emoji is outside ASCII
    if text.is_ascii() {
        if !text.is_empty() {
            out.push(Seg::Text(text));
        }
        return out;
    }
    let mut run_start: Option<usize> = None;
    for (i, g) in text.grapheme_indices(true) {
        if is_emoji(g) {
            if let Some(s) = run_start.take() {
                out.push(Seg::Text(&text[s..i]));
            }
            out.push(Seg::Emoji(g));
        } else if run_start.is_none() {
            run_start = Some(i);
        }
    }
    if let Some(s) = run_start {
        out.push(Seg::Text(&text[s..]));
    }
    out
}

/// "french fries" -> "French Fries".
fn title_case(s: &str) -> String {
    s.split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// (display name, shortcode) of an emoji: ("French Fries", Some("fries")).
pub fn emoji_names(g: &str) -> (String, Option<String>) {
    match lookup(g) {
        Some(e) => (title_case(e.name()), e.shortcode().map(|s| s.to_string())),
        None => (g.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_text_and_emoji() {
        assert_eq!(split_emoji("hi🍟!"), vec![Seg::Text("hi"), Seg::Emoji("🍟"), Seg::Text("!")]);
        assert_eq!(split_emoji("🍟🍟"), vec![Seg::Emoji("🍟"), Seg::Emoji("🍟")]);
        assert_eq!(split_emoji("plain 123 #1"), vec![Seg::Text("plain 123 #1")]);
        assert_eq!(split_emoji("héllo"), vec![Seg::Text("héllo")]);
        assert!(split_emoji("").is_empty());
    }

    #[test]
    fn sequences_stay_whole() {
        assert_eq!(split_emoji("👍🏽"), vec![Seg::Emoji("👍🏽")]);
        assert_eq!(split_emoji("🏳️‍🌈"), vec![Seg::Emoji("🏳️‍🌈")]);
        assert_eq!(split_emoji("🇸🇪"), vec![Seg::Emoji("🇸🇪")]);
        assert_eq!(split_emoji("❤️x"), vec![Seg::Emoji("❤️"), Seg::Text("x")]);
    }

    #[test]
    fn names() {
        assert_eq!(emoji_names("🍟"), ("French Fries".to_string(), Some("fries".to_string())));
        assert_eq!(emoji_names("❤").1.as_deref(), Some("heart"));
    }
}
