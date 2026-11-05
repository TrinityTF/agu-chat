//! Port of src/renderer/chat/search.ts: split search (Ctrl+F). Query syntax: whitespace-separated words, all of
//! which must appear (case-insensitive) in the message text or the author's name / login. `from:name` (or
//! `from:@name`) keeps only that author's rows; several `from:` words mean any of those authors.
use super::types::{Row, RowBody};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchQuery {
    pub from: Vec<String>,
    pub words: Vec<String>,
}

pub fn parse_search(q: &str) -> SearchQuery {
    let mut out = SearchQuery::default();
    for tok in q.split_whitespace() {
        let lower = tok.to_lowercase();
        if let Some(rest) = lower.strip_prefix("from:") {
            let name = rest.strip_prefix('@').unwrap_or(rest);
            if !name.is_empty() {
                out.from.push(name.to_string());
            }
        } else {
            out.words.push(lower);
        }
    }
    out
}

pub fn is_empty_query(q: &SearchQuery) -> bool {
    q.from.is_empty() && q.words.is_empty()
}

/// The author of a row as lowercase [login, display name], or None (mod feed lines have none).
pub fn row_author_names(r: &Row) -> Option<[String; 2]> {
    let lc = |s: &str| s.to_lowercase();
    Some(match &r.body {
        RowBody::Msg(m) => [lc(&m.user.login), lc(&m.user.name)],
        RowBody::Rumble(m) => [lc(&m.user.login), lc(&m.user.name)],
        RowBody::Kick(m) => [lc(&m.user.login), lc(&m.user.name)],
        RowBody::X(m) => [lc(&m.user.login), lc(&m.user.name)],
        RowBody::YouTube(m) => [lc(&m.user.name).trim_start_matches('@').to_string(), lc(&m.user.name)],
        RowBody::Notice(n) => [lc(&n.user.login), lc(&n.user.name)],
        RowBody::Automod(h) => [lc(&h.user.login), lc(&h.user.name)],
        RowBody::ModFeed(_) => return None,
    })
}

pub fn row_text(r: &Row) -> String {
    match &r.body {
        RowBody::Msg(m) => m.text.clone(),
        RowBody::Rumble(m) => m.text.clone(),
        RowBody::Kick(m) => m.text.clone(),
        RowBody::YouTube(m) => m.text.clone(),
        RowBody::X(m) => m.text.clone(),
        RowBody::Notice(n) => [n.system.as_str(), n.text.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" "),
        RowBody::ModFeed(f) => f.text.clone(),
        RowBody::Automod(h) => h.text.clone(),
    }
}

pub fn match_row(r: &Row, q: &SearchQuery) -> bool {
    let names = row_author_names(r);
    if !q.from.is_empty() {
        match &names {
            Some(n) if q.from.iter().any(|f| n.contains(f)) => {}
            _ => return false,
        }
    }
    if q.words.is_empty() {
        return true;
    }
    let hay = format!("{}\n{}", row_text(r), names.map(|n| n.join(" ")).unwrap_or_default()).to_lowercase();
    q.words.iter().all(|w| hay.contains(w))
}

pub fn search_rows(rows: &[Row], query: &str) -> Vec<Row> {
    let q = parse_search(query);
    if is_empty_query(&q) {
        return rows.to_vec();
    }
    rows.iter().filter(|r| match_row(r, &q)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::types::*;
    use agu_chat_core::protocol::Platform;

    fn row(key: String, platform: Platform, sk: &str, body: RowBody) -> Row {
        Row { key, ts: 1, sk: sk.into(), platform, ord: 0, backlog: false, body }
    }
    fn msg(text: &str, login: &str, name: &str) -> Row {
        let m = ChatMsg { id: text.into(), user: ChatUser { id: login.into(), login: login.into(), name: name.into(), color: String::new() }, text: text.into(), ts: 1, ..Default::default() };
        row(format!("{login}-{text}"), Platform::Twitch, "twitch:chan", RowBody::Msg(m))
    }
    fn feed(text: &str) -> Row {
        row(format!("f-{text}"), Platform::Twitch, "twitch:chan", RowBody::ModFeed(ModFeedEntry { ts: 1, feed_kind: "ban".into(), text: text.into() }))
    }
    fn yt(text: &str, name: &str) -> Row {
        let m = YouTubeMsg { id: text.into(), user: YouTubeUser { id: "y".into(), name: name.into(), avatar: String::new() }, text: text.into(), ts: 1, ..Default::default() };
        row(format!("y-{text}"), Platform::Youtube, "youtube:c", RowBody::YouTube(m))
    }
    fn keys(v: Vec<Row>) -> Vec<String> {
        v.into_iter().map(|r| r.key).collect()
    }

    #[test]
    fn parse_splits_words_and_from() {
        assert_eq!(parse_search("  Hello from:@Alice  world FROM:bob from: "), SearchQuery { from: vec!["alice".into(), "bob".into()], words: vec!["hello".into(), "world".into()] });
        assert_eq!(parse_search(""), SearchQuery::default());
    }

    #[test]
    fn all_words_must_match_text_or_username() {
        let r = msg("Good Morning chat", "alice", "Alice_TV");
        assert!(match_row(&r, &parse_search("morning")));
        assert!(match_row(&r, &parse_search("GOOD chat")));
        assert!(match_row(&r, &parse_search("alice_tv morning")));
        assert!(!match_row(&r, &parse_search("morning evening")));
    }

    #[test]
    fn from_keeps_only_that_author() {
        let rows = vec![msg("hi", "alice", "alice"), msg("hi", "bob", "bob"), msg("yo", "carol", "carol"), feed("alice was banned")];
        assert_eq!(keys(search_rows(&rows, "from:alice")), vec!["alice-hi"]);
        assert_eq!(keys(search_rows(&rows, "from:alice from:bob")), vec!["alice-hi", "bob-hi"]);
        assert!(keys(search_rows(&rows, "from:carol hi")).is_empty());
        assert_eq!(keys(search_rows(&rows, "banned")), vec!["f-alice was banned"]);
    }

    #[test]
    fn from_matches_youtube_names_with_or_without_at() {
        assert!(match_row(&yt("hey", "@Streamer"), &parse_search("from:streamer")));
        assert!(match_row(&yt("hey", "@Streamer"), &parse_search("from:@streamer")));
    }

    #[test]
    fn empty_query_returns_rows_unchanged() {
        let rows = vec![msg("a", "x", "x")];
        assert_eq!(search_rows(&rows, "   ").len(), 1);
    }
}
