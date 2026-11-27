//! Port of src/renderer/chat/input/autocomplete.tsx: emote / command autocomplete (Tab prefix, ":xx" substring,
//! "/" command prefix) and "@name" completion, as pure logic (the desktop draws the two dropdowns, Android a strip).
//!
//! Text positions are byte offsets into the box' text (the JS used UTF-16 offsets); everything the JS did with
//! `before = text.slice(0, caret)` works on the same slice here.
use super::slash::{cap_value, SLASH_COMMANDS};
use crate::mod_caps::ModCaps;

// ---- emote / command autocomplete -------------------------------------------------------------------

/// A flattened inventory entry. `channel_scope` covers channel AND personal sets (both rank above globals).
#[derive(Clone, Debug, PartialEq)]
pub struct InvEntry {
    pub name: String,
    pub img: String,
    pub label: String,
    pub channel_scope: bool,
    pub insert: Option<String>,
}

/// `dim` is only set in "cmd" mode: a recognized slash command the account can't currently use.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct AutoMatch {
    pub name: String,
    pub img: String,
    pub label: String,
    pub dim: bool,
    pub insert: Option<String>,
}

/// TRIGGER A (Tab, prefix-only) vs TRIGGER B (":xx", substring) vs TRIGGER C ("/", command-name prefix).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoMode {
    Prefix,
    Colon,
    Cmd,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutoState {
    pub mode: AutoMode,
    pub start: usize,
    pub end: usize,
    pub matches: Vec<AutoMatch>,
    pub idx: usize,
}

/// `localeCompare` as far as the names matter: case-insensitive first, lowercase before uppercase on a tie.
fn locale_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| b.cmp(a))
}

/// Prefix matches before substring matches, channel/personal sets before global, then alphabetical. Capped to 50
/// (the dropdown shows ~8 rows and scrolls).
pub fn rank_auto_matches(inv: &[InvEntry], query: &str, mode: AutoMode) -> Vec<AutoMatch> {
    let q = query.to_lowercase();
    if q.is_empty() {
        return vec![];
    }
    let mut scored: Vec<(&InvEntry, bool)> = Vec::new();
    for e in inv {
        let lname = e.name.to_lowercase();
        if mode == AutoMode::Prefix {
            if lname.starts_with(&q) {
                scored.push((e, true));
            }
        } else if lname.contains(&q) {
            scored.push((e, lname.starts_with(&q)));
        }
    }
    scored.sort_by(|(ea, pa), (eb, pb)| {
        pb.cmp(pa).then_with(|| eb.channel_scope.cmp(&ea.channel_scope)).then_with(|| locale_cmp(&ea.name, &eb.name))
    });
    scored.into_iter().take(50).map(|(e, _)| AutoMatch { name: e.name.clone(), img: e.img.clone(), label: e.label.clone(), dim: false, insert: e.insert.clone() }).collect()
}

/// Tab on a word: emotes first, then chatter names (completed with the "@" when the word had one).
pub fn rank_tab_matches(inv: &[InvEntry], chatters: &[String], word: &str) -> Vec<AutoMatch> {
    let bare_word = word.strip_prefix('@').unwrap_or(word);
    let emotes = rank_auto_matches(inv, bare_word, AutoMode::Prefix);
    let bare = bare_word.to_lowercase();
    let at = word.starts_with('@');
    if bare.is_empty() || (!emotes.is_empty() && !at) {
        return emotes;
    }
    let mut names: Vec<AutoMatch> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for n in chatters {
        let l = n.to_lowercase();
        if seen.contains(&l) || !l.starts_with(&bare) {
            continue;
        }
        seen.insert(l);
        names.push(AutoMatch { name: if at { format!("@{n}") } else { n.clone() }, img: String::new(), label: "chatter".into(), dim: false, insert: None });
        if names.len() >= 20 {
            break;
        }
    }
    if at {
        names.into_iter().chain(emotes).collect()
    } else {
        emotes.into_iter().chain(names).collect()
    }
}

// ---- the text around the caret ----------------------------------------------------------------------

/// `/(\S+)$/`: the word ending at the end of `before`.
pub fn trailing_word(before: &str) -> Option<&str> {
    let start = before.rfind(char::is_whitespace).map(|i| i + before[i..].chars().next().map(|ch| ch.len_utf8()).unwrap_or(1)).unwrap_or(0);
    let w = &before[start..];
    (!w.is_empty()).then_some(w)
}

/// `/^\/(\S*)$/`: the command name being typed at the very start.
pub fn command_query(before: &str) -> Option<&str> {
    let rest = before.strip_prefix('/')?;
    (!rest.contains(char::is_whitespace)).then_some(rest)
}

/// `/(^|\s)(:[^\s:]{2,})$/`: (byte start of the ":word", the word without its colon).
pub fn colon_query(before: &str) -> Option<(usize, &str)> {
    let i = before.rfind(':')?;
    let tail = &before[i + 1..];
    if tail.chars().count() < 2 || tail.contains(char::is_whitespace) {
        return None;
    }
    let ok_before = i == 0 || before[..i].chars().next_back().map(|ch| ch.is_whitespace()).unwrap_or(true);
    ok_before.then_some((i, tail))
}

/// `/(^|\s)@[^\s@]*$/`: a mention is being typed (so the box is not a command or :emote: at the same time).
pub fn is_mention_context(before: &str) -> bool {
    match before.rfind('@') {
        Some(i) => !before[i + 1..].contains(char::is_whitespace) && (i == 0 || before[..i].chars().next_back().map(|ch| ch.is_whitespace()).unwrap_or(true)),
        None => false,
    }
}

/// What the box offers after its text changed (SendBox.handleChange without the mention dropdown).
pub struct ChangeCtx<'a> {
    pub inventory: &'a [InvEntry],
    /// slash commands only run on a single Twitch target
    pub slash_ok: bool,
    pub twitch_caps: Option<&'a ModCaps>,
    pub names: &'a dyn Fn() -> Vec<String>,
}

pub fn auto_for_change(val: &str, pos: usize, prev: Option<&AutoState>, cx: &ChangeCtx) -> Option<AutoState> {
    let before = &val[..pos.min(val.len())];
    let pos = before.len();
    // Mentions first: "@" can't also start a command or a :emote:.
    if is_mention_context(before) {
        return None;
    }
    if let Some(query) = command_query(before) {
        if cx.slash_ok {
            let query = query.to_lowercase();
            let matches: Vec<AutoMatch> = SLASH_COMMANDS
                .iter()
                .filter(|cmd| cmd.name.starts_with(&query))
                .map(|cmd| AutoMatch {
                    name: cmd.name.to_string(),
                    img: String::new(),
                    label: format!("/{} {}", cmd.name, cmd.usage),
                    dim: match (cmd.cap_key, cx.twitch_caps) {
                        (Some(k), Some(caps)) => !cap_value(caps, k),
                        _ => false,
                    },
                    insert: None,
                })
                .collect();
            return (!matches.is_empty()).then(|| AutoState { mode: AutoMode::Cmd, start: 0, end: pos, matches, idx: 0 });
        }
    }
    if let Some((start, query)) = colon_query(before) {
        let matches = rank_auto_matches(cx.inventory, query, AutoMode::Colon);
        return (!matches.is_empty()).then(|| AutoState { mode: AutoMode::Colon, start, end: pos, matches, idx: 0 });
    }
    let prev = prev.filter(|p| p.mode == AutoMode::Prefix)?;
    let _ = prev;
    let word = trailing_word(before)?;
    let matches = rank_tab_matches(cx.inventory, &(cx.names)(), word);
    (!matches.is_empty()).then(|| AutoState { mode: AutoMode::Prefix, start: pos - word.len(), end: pos, matches, idx: 0 })
}

/// Tab on a plain word opens the prefix list at the caret.
pub fn auto_for_tab(text: &str, pos: usize, cx: &ChangeCtx) -> Option<AutoState> {
    let before = &text[..pos.min(text.len())];
    let word = trailing_word(before)?;
    let matches = rank_tab_matches(cx.inventory, &(cx.names)(), word);
    (!matches.is_empty()).then(|| AutoState { mode: AutoMode::Prefix, start: before.len() - word.len(), end: before.len(), matches, idx: 0 })
}

/// Replaces the tracked [start,end) range with the pick + a trailing space (Enter / Tab / a click). Command mode
/// always replaces from 0 with "/name " and does NOT send. Returns the new text and the caret.
pub fn commit_auto(text: &str, auto: &AutoState, choice: Option<&AutoMatch>) -> Option<(String, usize)> {
    let pick = choice.or_else(|| auto.matches.get(auto.idx)).or_else(|| auto.matches.first())?;
    if auto.mode == AutoMode::Cmd {
        let tail = text.get(auto.end..).unwrap_or("");
        return Some((format!("/{} {}", pick.name, tail), pick.name.len() + 2));
    }
    let ins = pick.insert.clone().unwrap_or_else(|| pick.name.clone());
    let head = text.get(..auto.start).unwrap_or(text);
    let tail = text.get(auto.end..).unwrap_or("");
    Some((format!("{head}{ins} {tail}"), auto.start + ins.len() + 1))
}

/// Cycling with the arrows (emote modes only) inline-writes the highlighted match's name into the box so the user
/// can flip between candidates seeing each name, then press space to accept.
pub fn preview_auto(text: &str, auto: &AutoState, idx: usize) -> Option<(String, usize, AutoState)> {
    let pick = auto.matches.get(idx)?;
    let ins = pick.insert.clone().unwrap_or_else(|| pick.name.clone());
    let head = text.get(..auto.start).unwrap_or(text);
    let tail = text.get(auto.end..).unwrap_or("");
    let pos = auto.start + ins.len();
    let next = AutoState { mode: auto.mode, start: auto.start, end: pos, matches: auto.matches.clone(), idx };
    Some((format!("{head}{ins}{tail}"), pos, next))
}

// ---- @name completion -------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct MentionState {
    pub start: usize,
    pub end: usize,
    pub matches: Vec<String>,
    pub idx: usize,
}

/// A trailing "@word" at the caret. The "@" must start a word: mid-word "@" is an email or a handle already typed,
/// neither of which wants a dropdown over it. (byte start of the "@", query)
pub fn find_mention_query(before: &str) -> Option<(usize, &str)> {
    let i = before.rfind('@')?;
    let tail = &before[i + 1..];
    if tail.contains(char::is_whitespace) || tail.chars().count() > 40 {
        return None;
    }
    let ok_before = i == 0 || before[..i].chars().next_back().map(|ch| ch.is_whitespace()).unwrap_or(true);
    ok_before.then_some((i, tail))
}

/// Prefix matches first, then substring, then alphabetical.
pub fn rank_mention_matches<'a>(names: impl IntoIterator<Item = &'a str>, query: &str, limit: usize) -> Vec<String> {
    let q = query.to_lowercase();
    struct S {
        name: String,
        prefix: bool,
        i: usize,
    }
    let mut scored: Vec<S> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut i = 0;
    for name in names {
        if name.is_empty() {
            continue;
        }
        let lower = name.to_lowercase();
        if seen.contains(&lower) {
            continue;
        }
        // An empty query lists recent chatters: a useful "who is here" rather than an empty dropdown.
        if !q.is_empty() && !lower.contains(&q) {
            continue;
        }
        seen.insert(lower.clone());
        scored.push(S { name: name.to_string(), prefix: lower.starts_with(&q), i });
        i += 1;
    }
    scored.sort_by(|a, b| {
        b.prefix.cmp(&a.prefix).then_with(|| if q.is_empty() { a.i.cmp(&b.i) } else { locale_cmp(&a.name, &b.name) })
    });
    scored.into_iter().take(limit).map(|s| s.name).collect()
}

/// Owns the dropdown state for one input (`useMentions`). `refresh` is called on every change with the new value
/// and caret; `apply` returns the rewritten text and where the caret should land.
#[derive(Default)]
pub struct Mentions {
    pub state: Option<MentionState>,
}

impl Mentions {
    pub fn refresh(&mut self, value: &str, caret: usize, names: &dyn Fn() -> Vec<String>) {
        let before = &value[..caret.min(value.len())];
        let Some((start, query)) = find_mention_query(before) else {
            self.state = None;
            return;
        };
        let names = names();
        let matches = rank_mention_matches(names.iter().map(|s| s.as_str()), query, 8);
        self.state = (!matches.is_empty()).then(|| MentionState { start, end: before.len(), matches, idx: 0 });
    }

    pub fn close(&mut self) {
        self.state = None;
    }

    pub fn mv(&mut self, delta: isize) {
        if let Some(s) = &mut self.state {
            let n = s.matches.len() as isize;
            s.idx = ((s.idx as isize + delta + n) % n) as usize;
        }
    }

    pub fn apply(&mut self, value: &str, choice: Option<&str>) -> Option<(String, usize)> {
        let s = self.state.take()?;
        let name = choice.map(|c| c.to_string()).or_else(|| s.matches.get(s.idx).cloned())?;
        // One trailing space, and none added when the next character already is one.
        let after = value.get(s.end..).unwrap_or("");
        let insert = format!("@{name}{}", if after.starts_with(' ') { "" } else { " " });
        let head = value.get(..s.start).unwrap_or(value);
        Some((format!("{head}{insert}{after}"), s.start + insert.len()))
    }
}

// ---- the strip on a phone ---------------------------------------------------------------------------

/// Display names newest-first of the chatters of the given sources (`sks`), read from the rows: what "@" and Tab
/// complete names from (capped at 1000, like the desktop).
pub fn chatters(rows: &[crate::types::RowRef], sks: &[String]) -> Vec<String> {
    use crate::types::RowBody;
    let mut out = Vec::new();
    for r in rows.iter().rev() {
        if !sks.contains(&r.sk) {
            continue;
        }
        let n = match &r.body {
            RowBody::Msg(m) => &m.user.name,
            RowBody::Rumble(m) => &m.user.name,
            RowBody::Kick(m) => &m.user.name,
            RowBody::YouTube(m) => &m.user.name,
            RowBody::X(m) => &m.user.name,
            _ => continue,
        };
        if !n.is_empty() {
            out.push(n.clone());
        }
        if out.len() >= 1000 {
            break;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestKind {
    /// `:word`, an emote by (part of) its name
    Emote,
    /// `/word`, a slash command
    Command,
    /// `@word`, a chatter
    Mention,
    /// a plain word: the desktop's Tab (an emote by its start, else a chatter)
    Word,
}

/// One tap-to-insert suggestion. `replace` is what takes the place of the text from `start` to `end`.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub name: String,
    pub img: String,
    /// the second line: an emote's source, a command's usage
    pub hint: String,
    /// a command this account can't use
    pub dim: bool,
    pub replace: String,
}

/// What the strip above the keyboard offers for the text and caret (byte offsets into `text`).
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestions {
    pub kind: SuggestKind,
    pub start: usize,
    pub end: usize,
    pub items: Vec<Suggestion>,
}

/// The shortest plain word that is completed without being asked for (the desktop waits for Tab).
const WORD_MIN: usize = 2;

fn from_match(m: AutoMatch, replace: String) -> Suggestion {
    Suggestion { name: m.name, img: m.img, hint: m.label, dim: m.dim, replace }
}

/// The desktop's rules in one place: a mention wins over everything, then a command (only where commands run), then
/// `:emote`, then the Tab completion of the word at the caret. Picking a suggestion puts `replace` over `start..end`.
pub fn suggest(text: &str, caret: usize, cx: &ChangeCtx) -> Option<Suggestions> {
    let mut caret = caret.min(text.len());
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    let before = &text[..caret];
    let after = &text[caret..];
    if is_mention_context(before) {
        let (start, query) = find_mention_query(before)?;
        let names = (cx.names)();
        let items: Vec<Suggestion> = rank_mention_matches(names.iter().map(|s| s.as_str()), query, 8)
            .into_iter()
            .map(|n| Suggestion { replace: format!("@{n}{}", if after.starts_with(' ') { "" } else { " " }), name: format!("@{n}"), img: String::new(), hint: "chatter".into(), dim: false })
            .collect();
        return (!items.is_empty()).then_some(Suggestions { kind: SuggestKind::Mention, start, end: caret, items });
    }
    if let Some(query) = command_query(before) {
        if cx.slash_ok {
            let query = query.to_lowercase();
            let items: Vec<Suggestion> = SLASH_COMMANDS
                .iter()
                .filter(|c| c.name.starts_with(&query))
                .map(|c| Suggestion {
                    name: format!("/{}", c.name),
                    img: String::new(),
                    hint: c.usage.to_string(),
                    dim: match (c.cap_key, cx.twitch_caps) {
                        (Some(k), Some(caps)) => !cap_value(caps, k),
                        _ => false,
                    },
                    replace: format!("/{} ", c.name),
                })
                .collect();
            return (!items.is_empty()).then_some(Suggestions { kind: SuggestKind::Command, start: 0, end: caret, items });
        }
    }
    if let Some((start, query)) = colon_query(before) {
        let items: Vec<Suggestion> = rank_auto_matches(cx.inventory, query, AutoMode::Colon)
            .into_iter()
            .map(|m| {
                let replace = format!("{} ", m.insert.clone().unwrap_or_else(|| m.name.clone()));
                from_match(m, replace)
            })
            .collect();
        return (!items.is_empty()).then_some(Suggestions { kind: SuggestKind::Emote, start, end: caret, items });
    }
    let word = trailing_word(before).filter(|w| w.chars().count() >= WORD_MIN)?;
    // a half-typed command or :emote that found nothing above is not a word to complete
    if word.starts_with('/') || word.starts_with(':') {
        return None;
    }
    let items: Vec<Suggestion> = rank_tab_matches(cx.inventory, &(cx.names)(), word)
        .into_iter()
        .map(|m| {
            let replace = format!("{} ", m.insert.clone().unwrap_or_else(|| m.name.clone()));
            from_match(m, replace)
        })
        .collect();
    // the word already is the only suggestion: nothing left to complete
    if items.len() == 1 && items[0].replace.trim_end() == word {
        return None;
    }
    (!items.is_empty()).then_some(Suggestions { kind: SuggestKind::Word, start: caret - word.len(), end: caret, items })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, label: &str, channel_scope: bool) -> InvEntry {
        InvEntry { name: name.into(), img: format!("https://x/{name}"), label: label.into(), channel_scope, insert: None }
    }
    fn inv() -> Vec<InvEntry> {
        vec![e("Kappa", "Twitch", false), e("KappaPride", "Twitch", false), e("xKappa", "7TV", true), e("kapow", "7TV", true), e("LUL", "Twitch", false)]
    }
    fn names(v: &[AutoMatch]) -> Vec<&str> {
        v.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn prefix_ranking_puts_channel_scope_first_then_alphabetical() {
        let m = rank_auto_matches(&inv(), "kap", AutoMode::Prefix);
        assert_eq!(names(&m), vec!["kapow", "Kappa", "KappaPride"]);
    }

    #[test]
    fn colon_mode_matches_substrings_after_prefixes() {
        let m = rank_auto_matches(&inv(), "kappa", AutoMode::Colon);
        assert_eq!(names(&m), vec!["Kappa", "KappaPride", "xKappa"]);
        assert!(rank_auto_matches(&inv(), "", AutoMode::Colon).is_empty());
    }

    #[test]
    fn ranking_is_capped_at_50() {
        let many: Vec<InvEntry> = (0..80).map(|i| e(&format!("emote{i:02}"), "x", false)).collect();
        assert_eq!(rank_auto_matches(&many, "emote", AutoMode::Prefix).len(), 50);
    }

    #[test]
    fn tab_prefers_emotes_and_falls_back_to_chatters() {
        let chatters = vec!["Kapitan".to_string(), "bob".to_string(), "kapitan".to_string(), "Karl".to_string()];
        let m = rank_tab_matches(&inv(), &chatters, "kap");
        // emotes win while there are any
        assert_eq!(names(&m), vec!["kapow", "Kappa", "KappaPride"]);
        let m = rank_tab_matches(&inv(), &chatters, "bo");
        assert_eq!(names(&m), vec!["bob"]);
        assert_eq!(m[0].label, "chatter");
        // with an "@" the names come first and keep the "@"
        let m = rank_tab_matches(&inv(), &chatters, "@kap");
        assert_eq!(names(&m)[0], "@Kapitan");
        assert_eq!(names(&m).len(), 1 + 3);
        // deduplicated case-insensitively
        assert_eq!(names(&m).iter().filter(|n| n.eq_ignore_ascii_case("@kapitan")).count(), 1);
    }

    #[test]
    fn text_around_the_caret() {
        assert_eq!(trailing_word("hello wor"), Some("wor"));
        assert_eq!(trailing_word("hello "), None);
        assert_eq!(trailing_word("one"), Some("one"));
        assert_eq!(command_query("/ba"), Some("ba"));
        assert_eq!(command_query("/ban bob"), None);
        assert_eq!(command_query("x /ba"), None);
        assert_eq!(colon_query("hi :ka"), Some((3, "ka")));
        assert_eq!(colon_query("hi :k"), None);
        assert_eq!(colon_query("a:ka"), None);
        assert_eq!(colon_query(":ka"), Some((0, "ka")));
        assert!(is_mention_context("hi @bo"));
        assert!(is_mention_context("@"));
        assert!(!is_mention_context("mail@bo"));
        assert!(!is_mention_context("hi @bo x"));
    }

    fn cx_for<'a>(inventory: &'a [InvEntry], slash_ok: bool, caps: Option<&'a ModCaps>, names: &'a dyn Fn() -> Vec<String>) -> ChangeCtx<'a> {
        ChangeCtx { inventory, slash_ok, twitch_caps: caps, names }
    }

    #[test]
    fn typing_a_command_lists_commands_and_dims_the_ones_without_rights() {
        let i = inv();
        let caps = ModCaps { ban: true, ..Default::default() };
        let nm = || Vec::<String>::new();
        let cx = cx_for(&i, true, Some(&caps), &nm);
        let st = auto_for_change("/ti", 3, None, &cx).unwrap();
        assert_eq!(st.mode, AutoMode::Cmd);
        assert_eq!(names(&st.matches), vec!["timeout"]);
        assert_eq!(st.matches[0].label, "/timeout <user> <duration> [reason]");
        assert!(!st.matches[0].dim);
        let st = auto_for_change("/s", 2, None, &cx).unwrap();
        let slow = st.matches.iter().find(|m| m.name == "slow").unwrap();
        assert!(slow.dim);
        assert!(st.matches.iter().any(|m| m.name == "shield"));
        // not on a single twitch target: no command list
        let off = cx_for(&i, false, None, &nm);
        assert!(auto_for_change("/ti", 3, None, &off).is_none());
        assert!(auto_for_change("/zzz", 4, None, &cx).is_none());
    }

    #[test]
    fn typing_a_colon_word_offers_emotes_and_a_mention_wins_over_both() {
        let i = inv();
        let nm = || Vec::<String>::new();
        let cx = cx_for(&i, true, None, &nm);
        let st = auto_for_change("say :kap", 8, None, &cx).unwrap();
        assert_eq!((st.mode, st.start, st.end), (AutoMode::Colon, 4, 8));
        assert!(auto_for_change("say @kap", 8, None, &cx).is_none());
    }

    #[test]
    fn prefix_list_follows_typing_only_while_it_is_open() {
        let i = inv();
        let nm = || vec!["Bobby".to_string()];
        let cx = cx_for(&i, true, None, &nm);
        assert!(auto_for_change("a kap", 5, None, &cx).is_none());
        let open = auto_for_tab("a kap", 5, &cx).unwrap();
        assert_eq!((open.mode, open.start, open.end), (AutoMode::Prefix, 2, 5));
        let again = auto_for_change("a kapp", 6, Some(&open), &cx).unwrap();
        assert_eq!(names(&again.matches), vec!["Kappa", "KappaPride"]);
        assert_eq!(again.start, 2);
        assert!(auto_for_change("a zzz", 5, Some(&open), &cx).is_none());
    }

    #[test]
    fn committing_replaces_the_range_and_adds_a_space() {
        let st = AutoState { mode: AutoMode::Colon, start: 4, end: 8, matches: vec![AutoMatch { name: "Kappa".into(), ..Default::default() }, AutoMatch { name: "KappaPride".into(), ..Default::default() }], idx: 0 };
        assert_eq!(commit_auto("say :kap tail", &st, None), Some(("say Kappa  tail".to_string(), 10)));
        let picked = st.matches[1].clone();
        assert_eq!(commit_auto("say :kap", &st, Some(&picked)).unwrap().0, "say KappaPride ");
        // Kick / Rumble entries carry their wire text
        let kick = AutoState { mode: AutoMode::Prefix, start: 0, end: 2, matches: vec![AutoMatch { name: "LUL".into(), insert: Some("[emote:1:LUL]".into()), ..Default::default() }], idx: 0 };
        assert_eq!(commit_auto("LU", &kick, None), Some(("[emote:1:LUL] ".to_string(), 14)));
        // command mode always rewrites from 0
        let cmd = AutoState { mode: AutoMode::Cmd, start: 0, end: 3, matches: vec![AutoMatch { name: "timeout".into(), ..Default::default() }], idx: 0 };
        assert_eq!(commit_auto("/ti", &cmd, None), Some(("/timeout ".to_string(), 9)));
    }

    #[test]
    fn arrow_preview_writes_the_name_without_a_space() {
        let st = AutoState { mode: AutoMode::Colon, start: 4, end: 8, matches: vec![AutoMatch { name: "Kappa".into(), ..Default::default() }, AutoMatch { name: "KappaPride".into(), ..Default::default() }], idx: 0 };
        let (text, pos, next) = preview_auto("say :kap", &st, 1).unwrap();
        assert_eq!((text.as_str(), pos), ("say KappaPride", 14));
        assert_eq!((next.start, next.end, next.idx), (4, 14, 1));
        // and cycling again replaces the previous preview
        let (text2, _, _) = preview_auto(&text, &next, 0).unwrap();
        assert_eq!(text2, "say Kappa");
    }

    #[test]
    fn mention_queries() {
        assert_eq!(find_mention_query("hi @bo"), Some((3, "bo")));
        assert_eq!(find_mention_query("@"), Some((0, "")));
        assert_eq!(find_mention_query("a@b"), None);
        assert_eq!(find_mention_query("hi @bo x"), None);
        let long = format!("@{}", "a".repeat(41));
        assert_eq!(find_mention_query(&long), None);
    }

    #[test]
    fn mention_ranking_prefix_then_substring_and_recent_order_for_empty() {
        let n = ["zed", "Bobby", "robert", "bob", "BOB", "amy"];
        assert_eq!(rank_mention_matches(n.iter().copied(), "bob", 8), vec!["bob", "Bobby"]);
        // empty query: incoming (newest first) order, deduplicated
        assert_eq!(rank_mention_matches(n.iter().copied(), "", 3), vec!["zed", "Bobby", "robert"]);
        assert!(rank_mention_matches(n.iter().copied(), "qq", 8).is_empty());
    }

    #[test]
    fn mentions_apply_rewrites_with_one_space() {
        let mut m = Mentions::default();
        let nm = || vec!["Bobby".to_string(), "bob".to_string()];
        m.refresh("hello @bo", 9, &nm);
        assert_eq!(m.state.as_ref().unwrap().matches, vec!["bob", "Bobby"]);
        m.mv(1);
        assert_eq!(m.state.as_ref().unwrap().idx, 1);
        m.mv(1);
        assert_eq!(m.state.as_ref().unwrap().idx, 0);
        assert_eq!(m.apply("hello @bo", None), Some(("hello @bob ".to_string(), 11)));
        assert!(m.state.is_none());
        // no extra space when one follows already
        m.refresh("hi @bo there", 6, &nm);
        assert_eq!(m.apply("hi @bo there", Some("Bobby")), Some(("hi @Bobby there".to_string(), 9)));
        // closed when the caret leaves the mention
        m.refresh("hi @bo", 6, &nm);
        m.refresh("hi @bo x", 8, &nm);
        assert!(m.state.is_none());
    }

    #[test]
    fn the_strip_follows_the_desktops_triggers() {
        let i = inv();
        let caps = ModCaps { ban: true, ..Default::default() };
        let nm = || vec!["Bobby".to_string(), "bob".to_string(), "Kapitan".to_string()];
        let cx = cx_for(&i, true, Some(&caps), &nm);
        // ":" searches emotes anywhere in the name; the pick replaces ":word" and adds a space
        let s = suggest("say :kappa", 10, &cx).unwrap();
        assert_eq!((s.kind, s.start, s.end), (SuggestKind::Emote, 4, 10));
        assert_eq!(s.items.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["Kappa", "KappaPride", "xKappa"]);
        assert_eq!(s.items[0].replace, "Kappa ");
        // "@" lists chatters, prefix matches first, "@" kept in the pick, one space
        let s = suggest("hi @bo", 6, &cx).unwrap();
        assert_eq!((s.kind, s.start), (SuggestKind::Mention, 3));
        assert_eq!(s.items.iter().map(|x| x.replace.as_str()).collect::<Vec<_>>(), vec!["@bob ", "@Bobby "]);
        // an empty query is who was here last
        let s = suggest("@", 1, &cx).unwrap();
        assert_eq!(s.items[0].name, "@Bobby");
        // no extra space when one follows the caret already
        let s = suggest("hi @bo there", 6, &cx).unwrap();
        assert_eq!(s.items[0].replace, "@bob");
        // "/" lists commands with their usage, dimmed without the rights
        let s = suggest("/ti", 3, &cx).unwrap();
        assert_eq!((s.kind, s.start, s.end), (SuggestKind::Command, 0, 3));
        assert_eq!((s.items[0].name.as_str(), s.items[0].hint.as_str(), s.items[0].replace.as_str(), s.items[0].dim), ("/timeout", "<user> <duration> [reason]", "/timeout ", false));
        let s = suggest("/s", 2, &cx).unwrap();
        assert!(s.items.iter().find(|x| x.name == "/slow").unwrap().dim);
        // commands only run on a single Twitch chat
        let off = cx_for(&i, false, None, &nm);
        assert!(suggest("/ti", 3, &off).is_none());
    }

    #[test]
    fn a_plain_word_completes_like_tab() {
        let i = inv();
        let nm = || vec!["Karl".to_string()];
        let cx = cx_for(&i, true, None, &nm);
        // emotes by their start (channel sets first), names when no emote starts that way
        let s = suggest("a kap", 5, &cx).unwrap();
        assert_eq!((s.kind, s.start, s.end), (SuggestKind::Word, 2, 5));
        assert_eq!(s.items.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), vec!["kapow", "Kappa", "KappaPride"]);
        let s = suggest("hey kar", 7, &cx).unwrap();
        assert_eq!(s.items[0].name, "Karl");
        // one letter and a word that is finished (a space after it) offer nothing
        assert!(suggest("a k", 3, &cx).is_none());
        assert!(suggest("a kap ", 6, &cx).is_none());
        assert!(suggest("nothing", 7, &cx).is_none());
        // the word is judged up to the caret: text after it is kept
        let s = suggest("a kap tail", 5, &cx).unwrap();
        assert_eq!((s.start, s.end), (2, 5));
        // Kick / YouTube emotes insert their wire text
        let kick = vec![InvEntry { name: "LUL".into(), img: "u".into(), label: "Kick".into(), channel_scope: true, insert: Some("[emote:1:LUL]".into()) }];
        let cx2 = cx_for(&kick, true, None, &nm);
        assert_eq!(suggest(":lu", 3, &cx2).unwrap().items[0].replace, "[emote:1:LUL] ");
    }

    #[test]
    fn suggestions_survive_multibyte_text() {
        let i = inv();
        let nm = || Vec::<String>::new();
        let cx = cx_for(&i, true, None, &nm);
        let t = "h\u{e9}llo :kap";
        let s = suggest(t, t.len(), &cx).unwrap();
        assert_eq!(&t[s.start..s.end], ":kap");
        // a caret inside a character is moved back to a boundary rather than panicking
        assert!(suggest("\u{e9}", 1, &cx).is_none());
    }

    #[test]
    fn chatters_are_newest_first_and_per_source() {
        use crate::types::*;
        use agu_chat_core::protocol::Platform;
        use std::sync::Arc;
        let msg = |sk: &str, name: &str| -> RowRef {
            Arc::new(Row { key: format!("{sk}{name}"), ts: 1, sk: sk.into(), platform: Platform::Twitch, ord: 0, backlog: false, body: RowBody::Msg(ChatMsg { user: ChatUser { name: name.into(), ..Default::default() }, ..Default::default() }) })
        };
        let rows = vec![msg("a", "Old"), msg("b", "Elsewhere"), msg("a", "New")];
        assert_eq!(chatters(&rows, &["a".to_string()]), vec!["New", "Old"]);
    }
}
