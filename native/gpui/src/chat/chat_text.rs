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
