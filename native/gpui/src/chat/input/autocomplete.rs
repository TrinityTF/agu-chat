//! Port of src/renderer/chat/input/autocomplete.tsx: emote / command autocomplete (Tab prefix, ":xx" substring,
//! "/" command prefix) and "@name" completion, as pure logic plus the two dropdown elements.
//!
//! Text positions are byte offsets into the box' text (the JS used UTF-16 offsets); everything the JS did with
//! `before = text.slice(0, caret)` works on the same slice here.
use gpui::{div, prelude::*, px, relative, AnyElement, ElementId, FontWeight, MouseButton, MouseDownEvent, SharedString};

use super::slash::{cap_value, SLASH_COMMANDS};
use crate::chat::mod_caps::ModCaps;
use crate::chat::pane_render::ImageBox;
use crate::theme::{self, a, c};

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

// ---- dropdowns --------------------------------------------------------------------------------------

/// `AutocompleteDropdown`: absolute bottom-full left-0 z-30 mb-1 max-h-56 w-72, rows px-2 py-1 text-xs.
/// `on_pick` runs on mouse down (the input must not lose focus before the pick).
pub fn autocomplete_dropdown(state: &AutoState, scroll: &gpui::ScrollHandle, on_pick: impl Fn(usize, &mut gpui::Window, &mut gpui::App) + Clone + 'static) -> AnyElement {
    let is_cmd = state.mode == AutoMode::Cmd;
    let mut panel = div()
        .id("auto-dd")
        .max_h(px(224.))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .py(px(4.))
        .text_size(px(12.))
        .line_height(px(16.));
    for (i, m) in state.matches.iter().enumerate() {
        let sel = i == state.idx;
        let pick = on_pick.clone();
        let mut row = div()
            .id(ElementId::Integer(i as u64))
            .flex()
            .w_full()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(4.))
            .text_color(if sel { c(theme::TEXT) } else if m.dim { c(theme::FAINT) } else { c(theme::MUTED) })
            .when(sel, |d| d.bg(a(theme::ACCENT, 0.25)))
            .when(!sel, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
            .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, w, cx| {
                pick(i, w, cx);
                cx.stop_propagation();
            });
        if is_cmd {
            row = row
                .child(div().flex_none().font_family("Cascadia Mono").font_weight(FontWeight::SEMIBOLD).text_color(c(theme::ACCENT_SOFT)).child(SharedString::from(format!("/{}", m.name))))
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().text_color(c(theme::FAINT)).child(SharedString::from(m.label.clone())));
            if m.dim {
                row = row.child(div().flex_none().text_size(px(10.)).text_color(c(theme::AMBER_500)).child("(not available)"));
            }
        } else {
            row = row
                .child(if m.img.is_empty() {
                    div().size(px(20.)).flex_none().flex().items_center().justify_center().text_color(c(theme::FAINT)).child("@").into_any_element()
                } else {
                    ImageBox { url: m.img.clone().into(), side: 20.0, radius: 0.0 }.into_any_element()
                })
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(m.name.clone())))
                .child(div().flex_none().text_size(px(10.)).text_color(c(theme::FAINT)).child(SharedString::from(m.label.clone())));
        }
        panel = panel.child(row);
    }
    div()
        .absolute()
        .bottom(relative(1.))
        .left_0()
        .mb(px(4.))
        .w(px(288.))
        .occlude()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::LINE))
        .bg(c(theme::RAISED))
        .shadow_2xl()
        .child(panel)
        .into_any_element()
}

/// `MentionDropdown`: like the above, w-56, an "@" then the name.
pub fn mention_dropdown(state: &MentionState, scroll: &gpui::ScrollHandle, on_pick: impl Fn(usize, &mut gpui::Window, &mut gpui::App) + Clone + 'static) -> AnyElement {
    let mut panel = div()
        .id("mention-dd")
        .max_h(px(224.))
        .overflow_y_scroll()
        .track_scroll(scroll)
        .py(px(4.))
        .text_size(px(12.))
        .line_height(px(16.));
    for (i, name) in state.matches.iter().enumerate() {
        let sel = i == state.idx;
        let pick = on_pick.clone();
        panel = panel.child(
            div()
                .id(ElementId::Integer(i as u64))
                .flex()
                .w_full()
                .items_center()
                .gap(px(8.))
                .px(px(8.))
                .py(px(4.))
                .text_color(if sel { c(theme::TEXT) } else { c(theme::MUTED) })
                .when(sel, |d| d.bg(a(theme::ACCENT, 0.25)))
                .when(!sel, |d| d.hover(|s| s.bg(c(theme::RAISED_2))))
                .on_mouse_down(MouseButton::Left, move |_: &MouseDownEvent, w, cx| {
                    pick(i, w, cx);
                    cx.stop_propagation();
                })
                .child(div().flex_none().text_color(c(theme::FAINT)).child("@"))
                .child(div().min_w_0().flex_1().overflow_hidden().whitespace_nowrap().text_ellipsis().child(SharedString::from(name.clone()))),
        );
    }
    div()
        .absolute()
        .bottom(relative(1.))
        .left_0()
        .mb(px(4.))
        .w(px(224.))
        .occlude()
        .rounded(px(4.))
        .border_1()
        .border_color(c(theme::LINE))
        .bg(c(theme::RAISED))
        .shadow_2xl()
        .child(panel)
        .into_any_element()
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
}
