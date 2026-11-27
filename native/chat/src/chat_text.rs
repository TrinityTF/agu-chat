//! Port of src/renderer/chat/chatText.tsx, the pure half: what counts as a link, mention helpers. The React
//! components (`ChatLink`, `TextRun`) become inline items in `rows::inline_text`.
use std::sync::OnceLock;

use fancy_regex::Regex;

fn link_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)^(?P<lead>[(\[{"'<]*)(?P<url>(?:https?://|www\.)[^\s]+|[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|net|org|tv|gg|io|co|me|dev|app|live|xyz|link|to|be|ly|lol|gl|gd|cc|fm|ai|sh|im|uk|de|fr|nl|pl|se|no|fi|cz|ro|ch|ca|au|br|es|jp|ru)(?:/[^\s]*)?)$"#,
        )
        .unwrap()
    })
}

pub struct LinkWord {
    pub lead: String,
    pub href: String,
    pub text: String,
    pub trail: String,
}

/// The word as a link, or None. Splits off punctuation the URL does not own.
pub fn as_link(word: &str) -> Option<LinkWord> {
    let caps = link_word().captures(word).ok()??;
    let mut text = caps.name("url")?.as_str().to_string();
    let lead = caps.name("lead").map(|m| m.as_str().to_string()).unwrap_or_default();
    let mut trail = String::new();
    let cut_len = text.trim_end_matches(|c| ".,!?;:)]}'\"".contains(c)).len();
    if cut_len < text.len() {
        let t = text[cut_len..].to_string();
        let cut = &text[..cut_len];
        // A closing paren inside a real URL (Wikipedia does this) is kept when the URL also opened one.
        let opens = cut.matches('(').count();
        let closes = cut.matches(')').count();
        if !cut.is_empty() && !(t == ")" && opens > closes) {
            trail = t;
            text = cut.to_string();
        }
    }
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();
    let href = if lower.starts_with("http://") || lower.starts_with("https://") { text.clone() } else { format!("https://{text}") };
    Some(LinkWord { lead, href, text, trail })
}

/// One word split into leading punctuation, the name, and trailing punctuation (MENTION_WORD).
pub fn mention_word(w: &str) -> Option<(&str, &str, &str)> {
    let is_l = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let start = w.find(is_l)?;
    let rest = &w[start..];
    let len = rest.find(|c: char| !is_l(c)).unwrap_or(rest.len());
    let (name, trail) = rest.split_at(len);
    if trail.chars().any(is_l) {
        return None;
    }
    Some((&w[..start], name, trail))
}

/// Is this message addressed at `login_lower`? Every run of login-legal characters counts, so "@name," and
/// "name:" are mentions too.
pub fn text_mentions_login(text: &str, login_lower: &str) -> bool {
    if login_lower.is_empty() {
        return false;
    }
    let is_l = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.split(|c: char| !is_l(c)).any(|run| !run.is_empty() && run.to_lowercase() == login_lower)
}


use crate::types::{ChatUser, RumbleEmotes};

/// A sub / gift notice's sentence without the name it starts with, and the byte range to bold in it (7TV bolds
/// "Subscribed" and the gift's recipient): "Popo_Master subscribed with Prime." -> ("Subscribed with Prime.",
/// 0..10). None when the sentence doesn't start with the user's name (an anonymous gift).
pub fn sub_split(system: &str, user: &ChatUser) -> Option<(String, (usize, usize))> {
    let sys = system.trim();
    let lower = sys.to_lowercase();
    let name = [&user.name, &user.login].into_iter().map(|n| n.trim().to_lowercase()).find(|n| !n.is_empty() && lower.starts_with(n.as_str()))?;
    let rest = sys[name.len()..].trim_start();
    if rest.is_empty() {
        return None;
    }
    // "is gifting ..." reads on from the name; a past tense starts its own sentence
    let rest = if rest.starts_with("is ") || rest.starts_with("has ") {
        rest.to_string()
    } else {
        let mut c = rest.chars();
        c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
    };
    let bold = if rest.starts_with("Subscribed") {
        (0, "Subscribed".len())
    } else if let Some(i) = rest.find(" sub to ").map(|i| i + " sub to ".len()).or_else(|| rest.find(" Sub to ").map(|i| i + " Sub to ".len())) {
        let end = rest[i..].find(|c: char| c.is_whitespace() || c == '!').map_or(rest.len(), |e| i + e);
        (i, end)
    } else {
        (0, 0)
    };
    Some((rest, bold))
}

/// Port of `splitRumbleEmotes` (CoChatRows.tsx): `:name:` tokens (RUMBLE_TOKEN = /:([A-Za-z0-9_+.-]{1,60}):/g)
/// resolved against the chat's emote list; a token with no emote stays text, and like the global regex the
/// scan then resumes AFTER its closing colon (so in "a:b:c:" a missing ":b:" never makes ":c:" a token).
#[derive(Debug, PartialEq)]
pub enum RumblePart<'a> {
    Text(&'a str),
    Emote { name: &'a str, file: String },
}

pub fn split_rumble_emotes<'a>(text: &'a str, emotes: &RumbleEmotes) -> Vec<RumblePart<'a>> {
    if text.is_empty() {
        return vec![];
    }
    if emotes.is_empty() {
        return vec![RumblePart::Text(text)];
    }
    let is_tok = |c: char| c.is_ascii_alphanumeric() || "_+.-".contains(c);
    let mut parts = Vec::new();
    let mut last = 0;
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b':' {
            let rest = &text[i + 1..];
            let len = rest.find(|c: char| !is_tok(c)).unwrap_or(rest.len());
            if (1..=60).contains(&len) && rest[len..].starts_with(':') {
                let name = &rest[..len];
                let end = i + 1 + len + 1;
                if let Some(file) = emotes.get(name).and_then(|e| e.get("file")).and_then(|v| v.as_str()) {
                    if i > last {
                        parts.push(RumblePart::Text(&text[last..i]));
                    }
                    parts.push(RumblePart::Emote { name, file: file.to_string() });
                    last = end;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    if parts.is_empty() {
        return vec![RumblePart::Text(text)];
    }
    if last < text.len() {
        parts.push(RumblePart::Text(&text[last..]));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links() {
        let l = as_link("https://example.com/x.").unwrap();
        assert_eq!((l.text.as_str(), l.trail.as_str(), l.href.as_str()), ("https://example.com/x", ".", "https://example.com/x"));
        let l = as_link("twitch.tv/agu").unwrap();
        assert_eq!(l.href, "https://twitch.tv/agu");
        assert!(as_link("hello").is_none());
        assert!(as_link("file.txt").is_none());
        let l = as_link("(www.a.com)").unwrap();
        assert_eq!((l.lead.as_str(), l.text.as_str(), l.trail.as_str()), ("(", "www.a.com", ")"));
        let l = as_link("https://en.wikipedia.org/wiki/Foo_(bar)").unwrap();
        assert_eq!(l.trail, "");
    }

    #[test]
    fn mentions() {
        assert!(text_mentions_login("hi @Agu, hello", "agu"));
        assert!(text_mentions_login("agu: yo", "agu"));
        assert!(!text_mentions_login("agurk", "agu"));
        assert!(!text_mentions_login("hi", ""));
        assert_eq!(mention_word("@a_g_u__,"), Some(("@", "a_g_u__", ",")));
        assert_eq!(mention_word("a-b"), None);
    }
}

#[cfg(test)]
mod moved_tests {
    use super::*;

    fn user(name: &str) -> ChatUser {
        serde_json::from_value(serde_json::json!({"name": name, "login": name.to_lowercase()})).unwrap()
    }

    #[test]
    fn splits_the_name_off_and_bolds_like_7tv() {
        let (r, b) = sub_split("Popo_Master subscribed with Prime.", &user("Popo_Master")).unwrap();
        assert_eq!(r, "Subscribed with Prime.");
        assert_eq!(&r[b.0..b.1], "Subscribed");
        let (r, b) = sub_split("KnubbeTV gifted a Tier 1 sub to necix7135i!", &user("KnubbeTV")).unwrap();
        assert_eq!(r, "Gifted a Tier 1 sub to necix7135i!");
        assert_eq!(&r[b.0..b.1], "necix7135i");
        let (r, b) = sub_split("KnubbeTV is gifting 1 Tier 1 Subs to EsfandTV's community!", &user("KnubbeTV")).unwrap();
        assert!(r.starts_with("is gifting"));
        assert_eq!(b, (0, 0));
        assert!(sub_split("An anonymous user gifted a Tier 1 sub to x!", &user("AnAnonymousGifter")).is_none());
    }

    #[test]
    fn rumble_emotes_split_like_the_global_regex() {
        let emotes: RumbleEmotes = serde_json::from_value(serde_json::json!({ "r+cooldoge": { "file": "a.png" }, "c": { "file": "c.png" } })).unwrap();
        use RumblePart::*;
        assert_eq!(split_rumble_emotes("hey from rumble :r+cooldoge: yo", &emotes), vec![Text("hey from rumble "), Emote { name: "r+cooldoge", file: "a.png".into() }, Text(" yo")]);
        // a miss consumes its closing colon, so ":c:" is never formed from the tail of ":b:"
        assert_eq!(split_rumble_emotes("a:b:c:", &emotes), vec![Text("a:b:c:")]);
        assert_eq!(split_rumble_emotes("::c::", &emotes), vec![Text(":"), Emote { name: "c", file: "c.png".into() }, Text(":")]);
        assert_eq!(split_rumble_emotes("time 10:30: ok :c:", &emotes), vec![Text("time 10:30: ok "), Emote { name: "c", file: "c.png".into() }]);
        assert_eq!(split_rumble_emotes("no list :c:", &RumbleEmotes::new()), vec![Text("no list :c:")]);
        assert_eq!(split_rumble_emotes("", &emotes), vec![]);
    }
}
