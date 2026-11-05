//! Port of src/renderer/chat/ignores.ts: ignored users and ignored phrases, applied to a split's rows before
//! they are drawn.
//!
//! * An ignored user (platform + login) hides every chat message of theirs on that platform.
//! * An ignored phrase either hides the message, or replaces each match with a literal replacement. All
//!   replace rules are matched against the ORIGINAL text in one pass (earliest match wins, then the longest),
//!   so rules never rewrite each other's output and Twitch's positional emote / GIF ranges can be remapped.
//!
//! Differences: edit spans are byte offsets (the JS used UTF-16 units); the row cache keys on the row's
//! `Arc` pointer instead of a WeakMap.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use agu_chat_core::protocol::{IgnoredPhrase, IgnoredUser, Platform};
use fancy_regex::Regex;

use super::twitch::parts::split_gif_entries;
use super::types::{Row, RowBody, RowRef};

pub struct CompiledIgnores {
    pub users: HashSet<String>,
    pub hide: Vec<Regex>,
    pub replace: Vec<(Regex, String)>,
    /// Nothing to do: callers can skip the pass entirely.
    pub empty: bool,
}

pub fn no_ignores() -> CompiledIgnores {
    CompiledIgnores { users: HashSet::new(), hide: vec![], replace: vec![], empty: true }
}

/// What an ignore entry stores for a user: lowercase login without "@" (YouTube has no login: its name).
pub fn ignore_login(login: &str, name: &str) -> String {
    let s = if login.is_empty() { name } else { login };
    s.trim().trim_start_matches('@').to_lowercase()
}

pub fn ignore_key(platform: Platform, login: &str) -> String {
    format!("{}:{}", platform.as_str(), ignore_login(login, ""))
}

pub fn is_user_ignored(list: &[IgnoredUser], platform: Platform, login: &str) -> bool {
    let l = ignore_login(login, "");
    !l.is_empty() && list.iter().any(|u| u.platform == platform && u.login == l)
}

/// The list with this user added (or removed when `on` is false).
pub fn toggle_ignored_user(list: &[IgnoredUser], platform: Platform, login: &str, on: bool) -> Vec<IgnoredUser> {
    let l = ignore_login(login, "");
    let mut rest: Vec<IgnoredUser> = list.iter().filter(|u| !(u.platform == platform && u.login == l)).cloned().collect();
    if on && !l.is_empty() {
        rest.push(IgnoredUser { platform, login: l });
    }
    rest
}

fn build(p: &str, regex: bool, cs: bool) -> Result<Regex, String> {
    let body = if regex { p.to_string() } else { fancy_regex::escape(p).to_string() };
    Regex::new(&if cs { body } else { format!("(?i){body}") }).map_err(|e| e.to_string())
}

/// Why a phrase can't be used (an invalid regular expression), or None.
pub fn phrase_error(pattern: &str, regex: bool, case_sensitive: bool) -> Option<String> {
    if !regex {
        return None;
    }
    build(pattern, true, case_sensitive).err()
}

fn compile_phrase(p: &IgnoredPhrase) -> Option<Regex> {
    if p.pattern.is_empty() {
        return None;
    }
    build(&p.pattern, p.regex, p.case_sensitive).ok()
}

pub fn compile_ignores(users: &[IgnoredUser], phrases: &[IgnoredPhrase]) -> CompiledIgnores {
    let mut out = no_ignores();
    for u in users {
        if !u.login.is_empty() {
            out.users.insert(ignore_key(u.platform, &u.login));
        }
    }
    for p in phrases {
        let Some(re) = compile_phrase(p) else { continue };
        if p.action == "replace" {
            out.replace.push((re, p.replacement.clone()));
        } else {
            out.hide.push(re);
        }
    }
    out.empty = out.users.is_empty() && out.hide.is_empty() && out.replace.is_empty();
    out
}

/// Does `re` find a non-empty match in `text`?
fn hits(re: &Regex, text: &str) -> bool {
    re.find_iter(text).filter_map(|m| m.ok()).any(|m| !m.as_str().is_empty())
}

/// One replaced span of the original text, in bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub with: String,
}

/// Non-overlapping matches of every replace rule, in text order.
pub fn find_edits(text: &str, rules: &[(Regex, String)]) -> Vec<Edit> {
    let mut all: Vec<Edit> = Vec::new();
    for (re, with) in rules {
        for m in re.find_iter(text).filter_map(|m| m.ok()) {
            if m.as_str().is_empty() {
                continue;
            }
            all.push(Edit { start: m.start(), end: m.end(), with: with.clone() });
        }
    }
    all.sort_by(|a, b| a.start.cmp(&b.start).then(b.end.cmp(&a.end)));
    let mut out = Vec::new();
    let mut at = 0;
    for e in all {
        if e.start < at {
            continue;
        }
        at = e.end;
        out.push(e);
    }
    out
}

pub fn apply_edits(text: &str, edits: &[Edit]) -> String {
    let mut out = String::new();
    let mut at = 0;
    for e in edits {
        out.push_str(&text[at..e.start]);
        out.push_str(&e.with);
        at = e.end;
    }
    out.push_str(&text[at..]);
    out
}

fn cp_len(s: &str) -> usize {
    s.chars().count()
}

/// Twitch's `emotes` / `gifs` tags index the text by code point. After replacing, keep the ranges that lie
/// wholly outside every edit, shifted to their new position, and drop the ones an edit touched.
pub struct Remap {
    segs: Vec<(usize, usize, usize)>, // (from, to, at)
}

impl Remap {
    fn get(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        self.segs.iter().find(|g| start >= g.0 && end < g.1).map(|g| (g.2 + start - g.0, g.2 + end - g.0))
    }
}

pub fn remapper(text: &str, edits: &[Edit]) -> Remap {
    let mut segs = Vec::new();
    let (mut old_cp, mut new_cp, mut pos) = (0, 0, 0);
    let tail = Edit { start: text.len(), end: text.len(), with: String::new() };
    for e in edits.iter().chain(std::iter::once(&tail)) {
        let keep = cp_len(&text[pos..e.start]);
        segs.push((old_cp, old_cp + keep, new_cp));
        old_cp += keep + cp_len(&text[e.start..e.end]);
        new_cp += keep + cp_len(&e.with);
        pos = e.end;
    }
    Remap { segs }
}

pub fn remap_emote_tag(tag: &str, map: &dyn Fn(usize, usize) -> Option<(usize, usize)>) -> String {
    if tag.is_empty() {
        return String::new();
    }
    let mut out: Vec<String> = Vec::new();
    for part in tag.split('/') {
        let mut it = part.splitn(2, ':');
        let (id, ranges) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        if id.is_empty() || ranges.is_empty() {
            continue;
        }
        let mut kept = Vec::new();
        for r in ranges.split(',') {
            let mut se = r.split('-');
            if let (Some(Ok(s)), Some(Ok(e))) = (se.next().map(|x| x.parse::<usize>()), se.next().map(|x| x.parse::<usize>())) {
                if let Some((a, b)) = map(s, e) {
                    kept.push(format!("{a}-{b}"));
                }
            }
        }
        if !kept.is_empty() {
            out.push(format!("{id}:{}", kept.join(",")));
        }
    }
    out.join("/")
}

pub fn remap_gif_tag(tag: &str, map: &dyn Fn(usize, usize) -> Option<(usize, usize)>) -> String {
    if tag.is_empty() {
        return String::new();
    }
    let mut out = Vec::new();
    for entry in split_gif_entries(tag) {
        // ^(\d+)-(\d+)(\|.*)$
        let Some(bar) = entry.find('|') else { continue };
        let mut se = entry[..bar].split('-');
        let (Some(Ok(s)), Some(Ok(e))) = (se.next().map(|x| x.parse::<usize>()), se.next().map(|x| x.parse::<usize>())) else { continue };
        if let Some((a, b)) = map(s, e) {
            out.push(format!("{a}-{b}{}", &entry[bar..]));
        }
    }
    out.join(",")
}

/// The author of a chat message row as an ignore key, or None for rows that aren't chat messages.
pub fn row_ignore_key(r: &Row) -> Option<String> {
    match &r.body {
        RowBody::Msg(m) => Some(ignore_key(r.platform, &m.user.login)),
        RowBody::Rumble(m) => Some(ignore_key(r.platform, &m.user.login)),
        RowBody::Kick(m) => Some(ignore_key(r.platform, &m.user.login)),
        RowBody::X(m) => Some(ignore_key(r.platform, &m.user.login)),
        RowBody::YouTube(m) => Some(ignore_key(r.platform, &m.user.name)),
        _ => None,
    }
}

/// The row as it should be shown: `None` to hide it, the same `Arc` when nothing applies (so cached row
/// views stay valid), or a copy with the replaced text. Only chat messages are affected.
pub fn apply_ignores(r: &RowRef, c: &CompiledIgnores) -> Option<RowRef> {
    if c.empty {
        return Some(r.clone());
    }
    let Some(key) = row_ignore_key(r) else { return Some(r.clone()) };
    if c.users.contains(&key) {
        return None;
    }
    let text: &str = match &r.body {
        RowBody::Msg(m) => &m.text,
        RowBody::Rumble(m) => &m.text,
        RowBody::Kick(m) => &m.text,
        RowBody::YouTube(m) => &m.text,
        RowBody::X(m) => &m.text,
        _ => return Some(r.clone()),
    };
    if c.hide.iter().any(|re| hits(re, text)) {
        return None;
    }
    if c.replace.is_empty() {
        return Some(r.clone());
    }
    let mut row: Row = (**r).clone();
    match &mut row.body {
        RowBody::YouTube(m) => {
            // Rendered from `parts`: replace inside each text part (emoji parts are left alone).
            let mut changed = false;
            for p in m.parts.iter_mut() {
                if p.t != "text" {
                    continue;
                }
                let edits = find_edits(&p.v, &c.replace);
                if !edits.is_empty() {
                    changed = true;
                    p.v = apply_edits(&p.v, &edits);
                }
            }
            let edits = find_edits(&m.text, &c.replace);
            if !changed && edits.is_empty() {
                return Some(r.clone());
            }
            m.text = apply_edits(&m.text, &edits);
        }
        RowBody::Msg(m) => {
            let edits = find_edits(&m.text, &c.replace);
            if edits.is_empty() {
                return Some(r.clone());
            }
            let rm = remapper(&m.text, &edits);
            let f = |s, e| rm.get(s, e);
            let next = apply_edits(&m.text, &edits);
            m.emotes = remap_emote_tag(&m.emotes, &f);
            if let Some(g) = &m.gifs {
                m.gifs = Some(remap_gif_tag(g, &f));
            }
            m.text = next;
        }
        RowBody::Rumble(m) => edit_text(&mut m.text, c)?,
        RowBody::Kick(m) => edit_text(&mut m.text, c)?,
        RowBody::X(m) => edit_text(&mut m.text, c)?,
        _ => return Some(r.clone()),
    }
    Some(Arc::new(row))
}

fn edit_text(text: &mut String, c: &CompiledIgnores) -> Option<()> {
    let edits = find_edits(text, &c.replace);
    if edits.is_empty() {
        // nothing replaced: signal "unchanged" by cloning identical text (harmless)
        return Some(());
    }
    *text = apply_edits(text, &edits);
    Some(())
}

/// Apply ignores to a list, reusing results for rows already seen with the same rules (`cache`, keyed by the
/// row `Arc` pointer; it keeps the original alive so the address stays valid).
pub fn filter_ignored(rows: &[RowRef], c: &CompiledIgnores, cache: &mut IgnoreCache) -> Vec<RowRef> {
    if c.empty {
        return rows.to_vec();
    }
    let mut out = Vec::with_capacity(rows.len());
    let mut seen: HashSet<usize> = HashSet::with_capacity(rows.len());
    for r in rows {
        let ptr = Arc::as_ptr(r) as usize;
        seen.insert(ptr);
        let v = cache.0.entry(ptr).or_insert_with(|| (r.clone(), apply_ignores(r, c))).1.clone();
        if let Some(v) = v {
            out.push(v);
        }
    }
    cache.0.retain(|k, _| seen.contains(k));
    out
}

#[derive(Default)]
pub struct IgnoreCache(HashMap<usize, (RowRef, Option<RowRef>)>);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::types::*;

    fn row(key: &str, platform: Platform, body: RowBody) -> RowRef {
        Arc::new(Row { key: key.into(), ts: 1, sk: format!("{}:chan", platform.as_str()), platform, ord: 0, backlog: false, body })
    }
    fn twitch(text: &str, login: &str, emotes: &str) -> RowRef {
        let m = ChatMsg { id: "1".into(), ts: 1, user: ChatUser { id: "u1".into(), login: login.into(), name: login.into(), color: String::new() }, text: text.into(), emotes: emotes.into(), ..Default::default() };
        row(&format!("t-{text}"), Platform::Twitch, RowBody::Msg(m))
    }
    fn kick(text: &str, login: &str) -> RowRef {
        let m = KickMsg { id: "2".into(), user: KickUser { id: "9".into(), login: login.into(), name: login.into(), ..Default::default() }, text: text.into(), ..Default::default() };
        row(&format!("k-{text}"), Platform::Kick, RowBody::Kick(m))
    }
    fn youtube(name: &str, parts: Vec<&str>) -> RowRef {
        let ps: Vec<YouTubePart> = parts.iter().map(|v| YouTubePart { t: "text".into(), v: v.to_string(), ..Default::default() }).collect();
        let m = YouTubeMsg { id: "3".into(), user: YouTubeUser { id: "yt1".into(), name: name.into(), avatar: String::new() }, text: parts.join(""), parts: ps, ..Default::default() };
        row("y", Platform::Youtube, RowBody::YouTube(m))
    }
    fn phrase(p: &str, regex: bool, cs: bool, action: &str, replacement: &str) -> IgnoredPhrase {
        IgnoredPhrase { pattern: p.into(), regex, case_sensitive: cs, action: action.into(), replacement: replacement.into() }
    }
    fn text_of(r: &RowRef) -> String {
        match &r.body {
            RowBody::Msg(m) => m.text.clone(),
            RowBody::Kick(m) => m.text.clone(),
            RowBody::YouTube(m) => m.text.clone(),
            _ => String::new(),
        }
    }

    #[test]
    fn ignored_users_hide_that_platform_only() {
        let c = compile_ignores(&[IgnoredUser { platform: Platform::Twitch, login: "alice".into() }], &[]);
        assert!(apply_ignores(&twitch("hi", "alice", ""), &c).is_none());
        assert!(apply_ignores(&twitch("hi", "ALICE", ""), &c).is_none());
        let other = twitch("hi", "carol", "");
        assert!(Arc::ptr_eq(&apply_ignores(&other, &c).unwrap(), &other));
        let k = kick("hi", "alice");
        assert!(Arc::ptr_eq(&apply_ignores(&k, &c).unwrap(), &k));
    }

    #[test]
    fn youtube_users_by_name() {
        let c = compile_ignores(&[IgnoredUser { platform: Platform::Youtube, login: "spammer".into() }], &[]);
        assert!(apply_ignores(&youtube("@Spammer", vec!["x"]), &c).is_none());
    }

    #[test]
    fn notices_are_never_hidden() {
        let c = compile_ignores(&[IgnoredUser { platform: Platform::Twitch, login: "alice".into() }], &[phrase("sub", false, false, "hide", "")]);
        let n = Notice { id: "n".into(), system: "alice subscribed".into(), msg_id: "sub".into(), user: ChatUser { login: "alice".into(), ..Default::default() }, text: "sub".into(), ..Default::default() };
        let r = row("n", Platform::Twitch, RowBody::Notice(n));
        assert!(Arc::ptr_eq(&apply_ignores(&r, &c).unwrap(), &r));
    }

    #[test]
    fn hide_phrases() {
        let c = compile_ignores(&[], &[phrase("a.b", false, false, "hide", "")]);
        assert!(apply_ignores(&twitch("look A.B here", "x", ""), &c).is_none());
        assert!(apply_ignores(&twitch("aXb", "x", ""), &c).is_some());
        let cs = compile_ignores(&[], &[phrase("Spam", false, true, "hide", "")]);
        assert!(apply_ignores(&twitch("spam", "x", ""), &cs).is_some());
        assert!(apply_ignores(&twitch("Spam", "x", ""), &cs).is_none());
        let re = compile_ignores(&[], &[phrase("^!\\w+", true, false, "hide", "")]);
        assert!(apply_ignores(&twitch("!command", "x", ""), &re).is_none());
        assert!(apply_ignores(&twitch("not !command", "x", ""), &re).is_some());
    }

    #[test]
    fn invalid_or_empty_regexes() {
        assert!(phrase_error("(", true, false).is_some());
        assert!(phrase_error("(", false, false).is_none());
        let c = compile_ignores(&[], &[phrase("(", true, false, "hide", ""), phrase("x*", true, false, "hide", "")]);
        assert!(!c.empty);
        let r = twitch("hello", "x", "");
        assert!(Arc::ptr_eq(&apply_ignores(&r, &c).unwrap(), &r));
    }

    #[test]
    fn replace_phrases() {
        let c = compile_ignores(&[], &[phrase("darn", false, false, "replace", "***")]);
        assert_eq!(text_of(&apply_ignores(&kick("Darn it, darn", "bob"), &c).unwrap()), "*** it, ***");
    }

    #[test]
    fn replace_is_one_pass_earliest_longest() {
        let c = compile_ignores(&[], &[phrase("ab", false, false, "replace", "1"), phrase("abc", false, false, "replace", "2"), phrase("1", false, false, "replace", "X")]);
        let edits = find_edits("abc ab", &c.replace);
        assert_eq!(apply_edits("abc ab", &edits), "2 1");
    }

    #[test]
    fn emote_ranges_shift_around_edits() {
        let c = compile_ignores(&[], &[phrase("darn", false, false, "replace", "x")]);
        let out = apply_ignores(&twitch("Kappa darn Kappa", "alice", "25:0-4,11-15"), &c).unwrap();
        match &out.body {
            RowBody::Msg(m) => {
                assert_eq!(m.text, "Kappa x Kappa");
                assert_eq!(m.emotes, "25:0-4,8-12");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn emote_ranges_inside_edits_dropped_code_points() {
        let c = compile_ignores(&[], &[phrase("Kappa", false, false, "replace", "")]);
        let out = apply_ignores(&twitch("😀 Kappa LUL", "alice", "25:2-6/425618:8-10"), &c).unwrap();
        match &out.body {
            RowBody::Msg(m) => {
                assert_eq!(m.text, "😀  LUL");
                assert_eq!(m.emotes, "425618:3-5");
                assert_eq!(m.text.chars().skip(3).take(3).collect::<String>(), "LUL");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn remap_drops_ids_with_no_ranges() {
        let f = |s: usize, e: usize| if s == 0 { None } else { Some((s, e)) };
        assert_eq!(remap_emote_tag("1:0-1/2:3-4", &f), "2:3-4");
    }

    #[test]
    fn youtube_replacement_on_parts() {
        let c = compile_ignores(&[], &[phrase("bad", false, false, "replace", "good")]);
        let out = apply_ignores(&youtube("x", vec!["so bad"]), &c).unwrap();
        match &out.body {
            RowBody::YouTube(m) => {
                assert_eq!(m.parts[0].v, "so good");
                assert_eq!(m.text, "so good");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn filter_keeps_untouched_identical_and_caches() {
        let c = compile_ignores(&[IgnoredUser { platform: Platform::Twitch, login: "alice".into() }], &[phrase("x", false, false, "replace", "y")]);
        let (a, b, d) = (twitch("hi", "alice", ""), twitch("hello", "carol", ""), twitch("x", "carol", ""));
        let mut cache = IgnoreCache::default();
        let rows = vec![a, b.clone(), d];
        let out = filter_ignored(&rows, &c, &mut cache);
        assert_eq!(out.len(), 2);
        assert!(Arc::ptr_eq(&out[0], &b));
        assert_eq!(text_of(&out[1]), "y");
        let again = filter_ignored(&rows, &c, &mut cache);
        assert!(Arc::ptr_eq(&again[1], &out[1]));
    }

    #[test]
    fn toggle_and_check_normalize_logins() {
        assert_eq!(ignore_login("", "@Some One"), "some one");
        let mut list = toggle_ignored_user(&[], Platform::Twitch, "@Alice", true);
        assert_eq!(list, vec![IgnoredUser { platform: Platform::Twitch, login: "alice".into() }]);
        assert!(is_user_ignored(&list, Platform::Twitch, "ALICE"));
        assert!(!is_user_ignored(&list, Platform::Kick, "alice"));
        list = toggle_ignored_user(&list, Platform::Twitch, "alice", true);
        assert_eq!(list.len(), 1);
        list = toggle_ignored_user(&list, Platform::Twitch, "alice", false);
        assert!(list.is_empty());
    }
}
