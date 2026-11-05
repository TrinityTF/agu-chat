//! Port of src/renderer/chat/highlights.ts: compile the user's highlight rules once, then match each chat
//! row against them. Differences: a phrase is matched with a hand-written whole-word scan (the JS used
//! lookbehind regex); `regex` rules use fancy-regex, which keeps JS-style lookaround and backreferences.
use agu_chat_core::protocol::HighlightRule;
use fancy_regex::Regex;

use super::types::{Row, RowBody};

/// What a rule looks at: the message text and who wrote it.
pub struct MatchInput {
    pub text: String,
    pub login: String,
    pub name: String,
}

enum Test {
    Phrase { needle: String, cs: bool },
    Regex(Regex),
    User(Vec<String>, bool),
}

struct Compiled {
    rule: HighlightRule,
    test: Test,
}

pub struct Highlighter {
    list: Vec<Compiled>,
}

/// A user rule's names: separated by commas or whitespace, a leading "@" is ignored.
pub fn rule_users(pattern: &str) -> Vec<String> {
    pattern
        .split(|c: char| c.is_whitespace() || c == ',')
        .map(|s| s.trim_start_matches('@').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The problem with a regex rule's pattern, or None when it compiles.
pub fn regex_error(pattern: &str, case_sensitive: bool) -> Option<String> {
    build_regex(pattern, case_sensitive).err()
}

fn build_regex(pattern: &str, cs: bool) -> Result<Regex, String> {
    let p = if cs { pattern.to_string() } else { format!("(?i){pattern}") };
    Regex::new(&p).map_err(|e| e.to_string())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whole-word occurrence of `needle` in `text`: letters/digits/_ in any script count as word characters, so a
/// phrase that starts or ends with punctuation ("!drop") still works.
fn phrase_hit(text: &str, needle: &str, cs: bool) -> bool {
    let (t, n) = if cs { (text.to_string(), needle.to_string()) } else { (text.to_lowercase(), needle.to_lowercase()) };
    let mut from = 0;
    while from <= t.len() {
        let Some(i) = t[from..].find(&n) else { return false };
        let s = from + i;
        let e = s + n.len();
        let before_ok = t[..s].chars().next_back().map(|c| !is_word(c)).unwrap_or(true);
        let after_ok = t[e..].chars().next().map(|c| !is_word(c)).unwrap_or(true);
        if before_ok && after_ok {
            return true;
        }
        from = s + t[s..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    false
}

fn compile_rule(rule: &HighlightRule) -> Option<Compiled> {
    if !rule.enabled {
        return None;
    }
    let pattern = rule.pattern.trim();
    if pattern.is_empty() {
        return None;
    }
    let cs = rule.case_sensitive;
    let test = match rule.kind.as_str() {
        "phrase" => Test::Phrase { needle: pattern.to_string(), cs },
        "regex" => Test::Regex(build_regex(pattern, cs).ok()?),
        "user" => {
            let names: Vec<String> = rule_users(pattern).into_iter().map(|s| if cs { s } else { s.to_lowercase() }).collect();
            if names.is_empty() {
                return None;
            }
            Test::User(names, cs)
        }
        _ => return None,
    };
    Some(Compiled { rule: rule.clone(), test })
}

pub fn compile_highlights(rules: &[HighlightRule]) -> Highlighter {
    Highlighter { list: rules.iter().filter_map(compile_rule).collect() }
}

impl Highlighter {
    /// How many rules can match at all (enabled, non-empty, valid).
    pub fn size(&self) -> usize {
        self.list.len()
    }

    /// The first enabled rule (in list order) that matches.
    pub fn matches(&self, m: &MatchInput) -> Option<&HighlightRule> {
        for c in &self.list {
            let hit = match &c.test {
                Test::Phrase { needle, cs } => phrase_hit(&m.text, needle, *cs),
                Test::Regex(re) => re.is_match(&m.text).unwrap_or(false),
                Test::User(names, cs) => {
                    let f = |s: &str| if *cs { s.to_string() } else { s.to_lowercase() };
                    (!m.login.is_empty() && names.contains(&f(&m.login))) || (!m.name.is_empty() && names.contains(&f(&m.name)))
                }
            };
            if hit {
                return Some(&c.rule);
            }
        }
        None
    }
}

/// Kick text with `[emote:123:name]` tokens shown as the emote's name.
pub fn kick_plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("[emote:") {
        out.push_str(&rest[..i]);
        let tail = &rest[i + 7..];
        let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && tail[digits..].starts_with(':') {
            let after = &tail[digits + 1..];
            if let Some(close) = after.find(']') {
                let name = &after[..close];
                if (1..=80).contains(&name.chars().count()) {
                    out.push_str(name);
                    rest = &after[close + 1..];
                    continue;
                }
            }
        }
        out.push_str("[emote:");
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// The text and author a rule is matched against, or None for rows rules never apply to (system lines).
pub fn match_input_of(row: &Row) -> Option<MatchInput> {
    Some(match &row.body {
        RowBody::Msg(m) => MatchInput { text: m.text.clone(), login: m.user.login.clone(), name: m.user.name.clone() },
        RowBody::Rumble(m) => MatchInput { text: m.text.clone(), login: m.user.login.clone(), name: m.user.name.clone() },
        RowBody::Kick(m) => MatchInput { text: kick_plain(&m.text), login: m.user.login.clone(), name: m.user.name.clone() },
        RowBody::YouTube(m) => MatchInput { text: m.text.clone(), login: String::new(), name: m.user.name.clone() },
        RowBody::X(m) => MatchInput { text: m.text.clone(), login: m.user.login.clone(), name: m.user.name.clone() },
        _ => return None,
    })
}

/// "#rrggbb" -> (r, g, b); anything else falls back to the accent purple.
pub fn parse_hex(color: &str) -> (u8, u8, u8) {
    let h = color.strip_prefix('#').unwrap_or("");
    if h.len() == 6 && h.chars().all(|c| c.is_ascii_hexdigit()) {
        let n = u32::from_str_radix(h, 16).unwrap();
        return ((n >> 16) as u8, (n >> 8) as u8, n as u8);
    }
    (145, 71, 255)
}

fn fmt_num(a: f32) -> String {
    if a == a.trunc() {
        format!("{}", a as i32)
    } else {
        format!("{a}")
    }
}

/// "#rrggbb" plus an alpha, as an rgba() string (kept for parity with the JS and its tests).
pub fn tint(color: &str, alpha: f32) -> String {
    let (r, g, b) = parse_hex(color);
    format!("rgba({r},{g},{b},{})", fmt_num(alpha))
}

/// The row style for a matched rule: a coloured left bar and a light tint of the same colour.
pub struct RuleRowStyle {
    pub border_left: String,
    pub bar: (u8, u8, u8),
    pub background: (u8, u8, u8, f32),
}

pub fn rule_row_style(color: &str) -> RuleRowStyle {
    let (r, g, b) = parse_hex(color);
    RuleRowStyle { border_left: format!("3px solid {}", tint(color, 1.0)), bar: (r, g, b), background: (r, g, b, 0.14) }
}

pub const RULE_COLORS: [&str; 8] = ["#ff9f1a", "#2bd4a4", "#3fa9ff", "#ff5cc8", "#f5d90a", "#a970ff", "#ff5c5c", "#7ddc1f"];

pub fn next_rule_color(rules: &[HighlightRule]) -> &'static str {
    let used: Vec<String> = rules.iter().map(|r| r.color.to_lowercase()).collect();
    RULE_COLORS.iter().find(|c| !used.contains(&c.to_string())).copied().unwrap_or(RULE_COLORS[rules.len() % RULE_COLORS.len()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(kind: &str, pattern: &str) -> HighlightRule {
        HighlightRule { id: "r".into(), enabled: true, kind: kind.into(), pattern: pattern.into(), case_sensitive: false, color: "#ff9f1a".into(), sound: false, sound_file: String::new(), flash: false }
    }
    fn msg(text: &str, login: &str, name: &str) -> MatchInput {
        MatchInput { text: text.into(), login: login.into(), name: name.into() }
    }

    #[test]
    fn phrase_matches_whole_words_case_insensitive() {
        let h = compile_highlights(&[rule("phrase", "agu")]);
        assert!(h.matches(&msg("hi agu!", "s", "s")).is_some());
        assert!(h.matches(&msg("AGU is here", "s", "s")).is_some());
        assert!(h.matches(&msg("agu", "s", "s")).is_some());
        assert!(h.matches(&msg("agurk salad", "s", "s")).is_none());
        assert!(h.matches(&msg("magu", "s", "s")).is_none());
        assert!(h.matches(&msg("my_agu", "s", "s")).is_none());
    }

    #[test]
    fn phrase_case_sensitivity() {
        let mut r = rule("phrase", "Drop");
        r.case_sensitive = true;
        let h = compile_highlights(&[r]);
        assert!(h.matches(&msg("Drop incoming", "s", "s")).is_some());
        assert!(h.matches(&msg("drop incoming", "s", "s")).is_none());
    }

    #[test]
    fn phrase_is_literal() {
        let h = compile_highlights(&[rule("phrase", "!drop (now)")]);
        assert!(h.matches(&msg("type !drop (now) please", "s", "s")).is_some());
        assert!(h.matches(&msg("type !drop now please", "s", "s")).is_none());
        let dots = compile_highlights(&[rule("phrase", "a.b")]);
        assert!(dots.matches(&msg("axb", "s", "s")).is_none());
        assert!(dots.matches(&msg("see a.b", "s", "s")).is_some());
    }

    #[test]
    fn phrase_word_boundaries_non_latin() {
        let h = compile_highlights(&[rule("phrase", "привет")]);
        assert!(h.matches(&msg("ну привет всем", "s", "s")).is_some());
        assert!(h.matches(&msg("приветствую", "s", "s")).is_none());
    }

    #[test]
    fn regex_rules() {
        let ci = compile_highlights(&[rule("regex", "^gg+$")]);
        assert!(ci.matches(&msg("GGGG", "s", "s")).is_some());
        assert!(ci.matches(&msg("ggez", "s", "s")).is_none());
        let mut r = rule("regex", "W+");
        r.case_sensitive = true;
        let cs = compile_highlights(&[r]);
        assert!(cs.matches(&msg("WWW", "s", "s")).is_some());
        assert!(cs.matches(&msg("www", "s", "s")).is_none());
    }

    #[test]
    fn invalid_regex_never_matches() {
        let h = compile_highlights(&[rule("regex", "(unclosed")]);
        assert_eq!(h.size(), 0);
        assert!(h.matches(&msg("(unclosed", "s", "s")).is_none());
        assert!(regex_error("(unclosed", false).is_some());
        assert!(regex_error("a+b", false).is_none());
    }

    #[test]
    fn user_rules() {
        let h = compile_highlights(&[rule("user", "@Alice, bob  carol")]);
        assert!(h.matches(&msg("hello", "alice", "Alice")).is_some());
        assert!(h.matches(&msg("hello", "bob_123", "BOB")).is_some());
        assert!(h.matches(&msg("hello", "", "Carol")).is_some());
        assert!(h.matches(&msg("alice said hi", "dave", "dave")).is_none());
        assert_eq!(rule_users(" @a, b c,,"), vec!["a", "b", "c"]);
        let mut r = rule("user", "Alice");
        r.case_sensitive = true;
        let h = compile_highlights(&[r]);
        assert!(h.matches(&msg("x", "nope", "Alice")).is_some());
        assert!(h.matches(&msg("x", "alice", "alice")).is_none());
    }

    #[test]
    fn disabled_empty_and_order() {
        let mut off = rule("phrase", "hello");
        off.enabled = false;
        let empty = rule("phrase", "   ");
        let mut a = rule("phrase", "hello");
        a.color = "#111111".into();
        let mut b = rule("regex", "hel+o");
        b.color = "#222222".into();
        let h = compile_highlights(&[off, empty, a.clone(), b.clone()]);
        assert_eq!(h.size(), 2);
        assert_eq!(h.matches(&msg("hello there", "s", "s")).unwrap().color, "#111111");
        assert_eq!(compile_highlights(&[b, a]).matches(&msg("hello there", "s", "s")).unwrap().color, "#222222");
        assert!(compile_highlights(&[]).matches(&msg("hello", "s", "s")).is_none());
    }

    #[test]
    fn kick_emotes_read_as_names() {
        assert_eq!(kick_plain("gg [emote:37226:KEKW] lol"), "gg KEKW lol");
        let h = compile_highlights(&[rule("phrase", "KEKW")]);
        assert!(h.matches(&msg(&kick_plain("gg [emote:37226:KEKW] lol"), "bo", "Bo")).is_some());
    }

    #[test]
    fn colours() {
        assert_eq!(tint("#ff0080", 0.5), "rgba(255,0,128,0.5)");
        assert_eq!(tint("nope", 1.0), "rgba(145,71,255,1)");
        assert_eq!(rule_row_style("#000000").border_left, "3px solid rgba(0,0,0,1)");
        assert_eq!(next_rule_color(&[]), "#ff9f1a");
        let mut r = rule("phrase", "x");
        r.color = "#FF9F1A".into();
        assert_eq!(next_rule_color(&[r]), "#2bd4a4");
    }
}
