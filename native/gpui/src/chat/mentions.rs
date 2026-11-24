//! The Mentions panel's model (like DankChat's mentions sheet): every live message that mentioned you, from
//! every open chat, in one list.
//!
//! What counts is decided by `classify`, which is also what turns a tab's dot red (`ChatPane::scan_alerts`), so
//! the bell and the tab dots always agree: a whisper, a Twitch message naming your login (a reply to you starts
//! with your @name), or a highlight rule of kind user / phrase / regex. Badge-only rules ring but stay a plain
//! message. Your own lines, backlog, deleted lines and lines hidden by ignores or the event toggles never count.
use std::collections::{HashSet, VecDeque};

use agu_chat_core::protocol::HighlightRule;

use super::chat_text::text_mentions_login;
use super::twitch::events::notice_visible;
use super::types::*;
use crate::layout::tree::is_whisper_key;

/// How many mentions the panel keeps for the session (the oldest go first).
pub const MAX_MENTIONS: usize = 200;

/// The key prefix of a row drawn in the Mentions panel (its hover state and element ids stay apart from the list's).
pub const PANEL_KEY: &str = "mention|";

/// What a live row does to its tab and the alerts.
#[derive(Clone, Debug, PartialEq)]
pub enum Alert {
    /// ordinary chat: the tab gets the plain "new" dot
    Plain,
    /// a whisper, or your login in a Twitch message
    Mention,
    /// a highlight rule matched
    Rule(HighlightRule),
}

impl Alert {
    /// Turns the tab dot red and goes to the Mentions panel. Badge rules are frequent (mods, VIPs): they ring
    /// but stay a plain message.
    pub fn red(&self) -> bool {
        match self {
            Alert::Plain => false,
            Alert::Mention => true,
            Alert::Rule(r) => r.kind != "badge",
        }
    }
}

/// A message that was deleted, timed out or banned away.
pub fn row_deleted(r: &Row) -> bool {
    match &r.body {
        RowBody::Msg(m) => m.deleted,
        RowBody::Rumble(m) => m.deleted,
        RowBody::Kick(m) => m.deleted,
        _ => false,
    }
}

/// What a newly arrived row is, or None when it marks nothing at all (backlog, the mod feed, your own line, a
/// line hidden by ignores (`hidden`) or by the event toggles). `rule_of` is asked only when it is not a mention.
pub fn classify(row: &Row, own: bool, hidden: bool, me_login_lower: &str, kick_me: &[String], rule_of: impl FnOnce() -> Option<HighlightRule>) -> Option<Alert> {
    if row.backlog || matches!(row.body, RowBody::ModFeed(_)) || own || hidden {
        return None;
    }
    if let RowBody::Notice(n) = &row.body {
        if !notice_visible(&n.msg_id, n.source_msg_id.as_deref(), n.gift_id.as_deref(), &HashSet::new()) {
            return None;
        }
    }
    // gone before it was seen: still news for the tab, but nothing to call you over
    if row_deleted(row) {
        return Some(Alert::Plain);
    }
    // A whisper is addressed to the user by definition.
    if matches!(&row.body, RowBody::Msg(m) if is_whisper_key(&row.sk) || text_mentions_login(&m.text, me_login_lower)) {
        return Some(Alert::Mention);
    }
    if matches!(&row.body, RowBody::Kick(m) if kick_mentions(m, kick_me)) {
        return Some(Alert::Mention);
    }
    Some(rule_of().map(Alert::Rule).unwrap_or(Alert::Plain))
}

/// Is this Kick line addressed at the logged-in Kick account (`me`: its username and slug, lowercased): a reply to
/// one of its lines, or its name in the text.
pub fn kick_mentions(m: &KickMsg, me: &[String]) -> bool {
    m.reply.as_ref().is_some_and(|r| me.iter().any(|n| !n.is_empty() && r.name.eq_ignore_ascii_case(n))) || me.iter().any(|n| text_mentions_login(&m.text, n))
}

/// One message across every split showing its chat: the source key (case-folded) and the row's own id.
pub fn mention_id(r: &Row) -> String {
    let rest = r.key.get(r.sk.len()..).filter(|_| r.key.starts_with(&r.sk)).unwrap_or(&r.key);
    format!("{}|{}", r.sk.to_lowercase(), rest)
}

#[derive(Clone, Debug)]
pub struct MentionEntry {
    pub id: String,
    /// the split it came in on (the jump goes there while it still shows that chat)
    pub leaf_id: String,
    /// "#channel" as the split header names it
    pub channel: String,
    pub row: RowRef,
    pub read: bool,
}

/// The session's mentions, oldest first.
#[derive(Default)]
pub struct Mentions {
    entries: VecDeque<MentionEntry>,
}

impl Mentions {
    /// Keep a mention; false when it is already here (the same chat in two splits) or deleted. `read` is true
    /// while the panel is open: it is seen at once.
    pub fn add(&mut self, leaf_id: &str, channel: &str, row: RowRef, read: bool) -> bool {
        if row_deleted(&row) {
            return false;
        }
        let id = mention_id(&row);
        if self.entries.iter().any(|e| e.id == id) {
            return false;
        }
        self.entries.push_back(MentionEntry { id, leaf_id: leaf_id.to_string(), channel: channel.to_string(), row, read });
        while self.entries.len() > MAX_MENTIONS {
            self.entries.pop_front();
        }
        true
    }

    /// Drop mentions whose message was deleted since (by `mention_id`); true when any went.
    pub fn remove(&mut self, ids: &[String]) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| !ids.contains(&e.id));
        self.entries.len() != n
    }

    pub fn unread(&self) -> usize {
        self.entries.iter().filter(|e| !e.read).count()
    }

    pub fn mark_all_read(&mut self) {
        for e in self.entries.iter_mut() {
            e.read = true;
        }
    }

    pub fn entries(&self) -> &VecDeque<MentionEntry> {
        &self.entries
    }

    pub fn get(&self, id: &str) -> Option<&MentionEntry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The bell's badge: nothing at 0, the count up to 99, then "99+".
pub fn bell_count_text(unread: usize) -> Option<String> {
    match unread {
        0 => None,
        1..=99 => Some(unread.to_string()),
        _ => Some("99+".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Platform;
    use std::sync::Arc;

    fn twitch(sk: &str, id: &str, text: &str) -> Row {
        let m = ChatMsg { id: id.into(), ts: 1, text: text.into(), user: ChatUser { id: "u1".into(), login: "bob".into(), name: "Bob".into(), color: String::new() }, ..Default::default() };
        Row { key: format!("{sk}|m:{id}"), ts: 1, sk: sk.into(), platform: Platform::Twitch, ord: 1, backlog: false, body: RowBody::Msg(m) }
    }

    fn kick(sk: &str, id: &str, text: &str) -> Row {
        let m = KickMsg { id: id.into(), ts: 1, text: text.into(), ..Default::default() };
        Row { key: format!("{sk}|k:{id}"), ts: 1, sk: sk.into(), platform: Platform::Kick, ord: 1, backlog: false, body: RowBody::Kick(m) }
    }

    fn rule(kind: &str) -> HighlightRule {
        HighlightRule { id: "r".into(), enabled: true, kind: kind.into(), pattern: "x".into(), case_sensitive: false, color: "#ff9f1a".into(), sound: false, sound_file: String::new(), flash: false }
    }

    fn red(row: &Row, own: bool, hidden: bool, r: Option<HighlightRule>) -> bool {
        classify(row, own, hidden, "agu", &[], || r).is_some_and(|a| a.red())
    }

    #[test]
    fn own_name_whispers_and_rules_count() {
        assert!(red(&twitch("twitch:x", "1", "hey @agu, look"), false, false, None));
        assert!(red(&twitch("twitch:x", "1", "AGU: hi"), false, false, None));
        // a reply to you starts with your @name
        assert!(red(&twitch("twitch:x", "1", "@Agu sure"), false, false, None));
        assert!(!red(&twitch("twitch:x", "1", "agua is water"), false, false, None));
        assert!(red(&twitch("twitch:whisper:bob", "1", "psst"), false, false, None));
        // other platforms: through the highlight rules
        for k in ["user", "phrase", "regex"] {
            assert!(red(&kick("kick:x", "1", "hello"), false, false, Some(rule(k))), "{k}");
        }
        assert!(!red(&kick("kick:x", "1", "hello"), false, false, None));
    }

    #[test]
    fn kick_replies_and_names_count_for_the_kick_account() {
        let me = vec!["a_g_u__".to_string(), "a-g-u".to_string()];
        let kred = |r: &Row| classify(r, false, false, "agu", &me, || None).is_some_and(|a| a.red());
        let mut reply = kick("kick:x", "1", "fair point");
        if let RowBody::Kick(m) = &mut reply.body {
            m.reply = Some(KickReply { id: "o1".into(), text: "yo".into(), name: "A_G_U__".into() });
        }
        assert!(kred(&reply));
        assert!(kred(&kick("kick:x", "1", "hey @a_g_u__ gg")));
        assert!(!kred(&kick("kick:x", "1", "hello")));
        let mut other = reply.clone();
        if let RowBody::Kick(m) = &mut other.body {
            m.reply.as_mut().unwrap().name = "someone".into();
        }
        assert!(!kred(&other));
        // logged out of Kick: nothing to match
        assert!(!red(&reply, false, false, None));
    }

    #[test]
    fn badge_rules_ring_but_stay_plain() {
        let a = classify(&twitch("twitch:x", "1", "hi"), false, false, "agu", &[], || Some(rule("badge"))).unwrap();
        assert!(matches!(a, Alert::Rule(_)) && !a.red());
    }

    #[test]
    fn own_hidden_deleted_and_backlog_lines_never_count() {
        let row = twitch("twitch:x", "1", "@agu hi");
        assert!(!red(&row, true, false, None));
        assert!(!red(&row, false, true, None));
        assert_eq!(classify(&row, false, true, "agu", &[], || None), None);
        let mut gone = row.clone();
        if let RowBody::Msg(m) = &mut gone.body {
            m.deleted = true;
        }
        assert_eq!(classify(&gone, false, false, "agu", &[], || Some(rule("phrase"))), Some(Alert::Plain));
        let old = Row { backlog: true, ..row.clone() };
        assert_eq!(classify(&old, false, false, "agu", &[], || None), None);
        let feed = Row { body: RowBody::ModFeed(ModFeedEntry::default()), ..row };
        assert_eq!(classify(&feed, false, false, "agu", &[], || Some(rule("phrase"))), None);
        // not logged in: your name can't be matched
        assert!(classify(&twitch("twitch:x", "1", "hi agu"), false, false, "", &[], || None).is_some_and(|a| !a.red()));
    }

    #[test]
    fn collector_keeps_order_dedupes_and_skips_deleted() {
        let mut m = Mentions::default();
        assert!(m.add("l1", "#x", Arc::new(twitch("twitch:x", "1", "@agu a")), false));
        assert!(m.add("l2", "#y", Arc::new(twitch("twitch:y", "2", "@agu b")), false));
        // the same chat open in a second split
        assert!(!m.add("l3", "#x", Arc::new(twitch("twitch:x", "1", "@agu a")), false));
        let mut gone = twitch("twitch:x", "3", "@agu c");
        if let RowBody::Msg(msg) = &mut gone.body {
            msg.deleted = true;
        }
        assert!(!m.add("l1", "#x", Arc::new(gone), false));
        let ids: Vec<&str> = m.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["twitch:x||m:1", "twitch:y||m:2"]);
        assert_eq!(m.unread(), 2);
        assert!(m.remove(&["twitch:x||m:1".to_string()]));
        assert!(!m.remove(&["nope".to_string()]));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn collector_caps_at_200_dropping_the_oldest() {
        let mut m = Mentions::default();
        for i in 0..250 {
            m.add("l", "#x", Arc::new(twitch("twitch:x", &i.to_string(), "@agu")), false);
        }
        assert_eq!(m.len(), MAX_MENTIONS);
        assert_eq!(m.entries().front().unwrap().id, "twitch:x||m:50");
        assert_eq!(m.entries().back().unwrap().id, "twitch:x||m:249");
        assert_eq!(m.unread(), MAX_MENTIONS);
    }

    #[test]
    fn opening_marks_read_and_mentions_while_open_are_read() {
        let mut m = Mentions::default();
        m.add("l", "#x", Arc::new(twitch("twitch:x", "1", "@agu")), false);
        m.add("l", "#x", Arc::new(twitch("twitch:x", "2", "@agu")), false);
        assert_eq!(m.unread(), 2);
        m.mark_all_read();
        assert_eq!(m.unread(), 0);
        m.add("l", "#x", Arc::new(twitch("twitch:x", "3", "@agu")), true);
        assert_eq!(m.unread(), 0);
        m.add("l", "#x", Arc::new(twitch("twitch:x", "4", "@agu")), false);
        assert_eq!(m.unread(), 1);
        assert_eq!(m.len(), 4);
    }

    #[test]
    fn ids_are_one_per_message_across_splits() {
        let r = twitch("twitch:Foo", "9", "");
        assert_eq!(mention_id(&r), "twitch:foo||m:9");
        let odd = Row { key: "other".into(), ..r };
        assert_eq!(mention_id(&odd), "twitch:foo|other");
    }

    #[test]
    fn bell_count_caps_at_99_plus() {
        assert_eq!(bell_count_text(0), None);
        assert_eq!(bell_count_text(1).as_deref(), Some("1"));
        assert_eq!(bell_count_text(99).as_deref(), Some("99"));
        assert_eq!(bell_count_text(100).as_deref(), Some("99+"));
        assert_eq!(bell_count_text(12345).as_deref(), Some("99+"));
    }
}
