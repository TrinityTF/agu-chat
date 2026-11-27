//! Reply threads (like DankChat's): which lines of the store belong to one thread. A Twitch reply names its
//! parent and the thread's first message (`reply-thread-parent-msg-id`), so every reply of a thread carries the
//! same root id; a Kick reply only names its parent, so its thread is followed parent by parent.
use std::collections::HashSet;

use super::types::*;

/// How a line hangs in a thread: its own id, the line it answers and the thread's first line ("" when unknown).
#[derive(Clone, Debug, PartialEq)]
pub struct ReplyLink<'a> {
    pub id: &'a str,
    pub parent: &'a str,
    pub thread: &'a str,
}

pub fn reply_link(r: &Row) -> Option<ReplyLink<'_>> {
    match &r.body {
        RowBody::Msg(m) => Some(match &m.reply {
            Some(rp) => ReplyLink { id: &m.id, parent: &rp.parent_id, thread: &rp.thread_id },
            None => ReplyLink { id: &m.id, parent: "", thread: "" },
        }),
        RowBody::Kick(m) => Some(ReplyLink { id: &m.id, parent: m.reply.as_ref().map(|r| r.id.as_str()).unwrap_or(""), thread: "" }),
        _ => None,
    }
}

/// The first message of the thread a reply belongs to, from what the reply knows (`parent`, `thread`) and the
/// lines still in the store: Twitch names it outright; otherwise walk up from the parent while the parent is
/// itself a reply that is still here.
pub fn thread_root(rows: &[RowRef], sk: &str, parent: &str, thread: &str) -> String {
    if !thread.is_empty() {
        return thread.to_string();
    }
    let mut cur = parent.to_string();
    let mut seen: HashSet<String> = HashSet::new();
    while seen.insert(cur.clone()) {
        let up = rows.iter().filter(|r| r.sk == sk).find_map(|r| reply_link(r).filter(|l| l.id == cur).map(|l| if l.thread.is_empty() { l.parent.to_string() } else { l.thread.to_string() }));
        match up {
            Some(p) if !p.is_empty() => cur = p,
            _ => break,
        }
    }
    cur
}

/// The thread's lines in the order they came: its first message (when the store still has it), then every reply
/// to it or to a reply in it.
pub fn thread_rows(rows: &[RowRef], sk: &str, root: &str) -> Vec<RowRef> {
    if root.is_empty() {
        return Vec::new();
    }
    let mut ids: HashSet<&str> = HashSet::from([root]);
    let mut out = Vec::new();
    for r in rows.iter().filter(|r| r.sk == sk) {
        let Some(l) = reply_link(r) else { continue };
        let member = l.id == root || l.thread == root || (!l.parent.is_empty() && ids.contains(l.parent));
        if member {
            if !l.id.is_empty() {
                ids.insert(l.id);
            }
            out.push(r.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Platform;
    use std::sync::Arc;

    fn msg(sk: &str, id: &str, ts: i64, reply: Option<(&str, &str)>) -> RowRef {
        let m = ChatMsg {
            id: id.into(),
            ts,
            text: format!("text {id}"),
            reply: reply.map(|(parent, thread)| ReplyInfo { user: "U".into(), text: "parent".into(), login: "u".into(), parent_id: parent.into(), thread_id: thread.into() }),
            ..Default::default()
        };
        Arc::new(Row { key: format!("{sk}|m:{id}"), ts, sk: sk.into(), platform: Platform::Twitch, ord: ts as u64, backlog: false, body: RowBody::Msg(m) })
    }

    fn kick(sk: &str, id: &str, ts: i64, parent: Option<&str>) -> RowRef {
        let m = KickMsg { id: id.into(), ts, reply: parent.map(|p| KickReply { id: p.into(), text: "t".into(), name: "n".into() }), ..Default::default() };
        Arc::new(Row { key: format!("{sk}|k:{id}"), ts, sk: sk.into(), platform: Platform::Kick, ord: ts as u64, backlog: false, body: RowBody::Kick(m) })
    }

    fn ids(rows: &[RowRef]) -> Vec<String> {
        rows.iter().map(|r| reply_link(r).unwrap().id.to_string()).collect()
    }

    #[test]
    fn root_then_replies_in_order() {
        let sk = "twitch:a";
        let rows = vec![
            msg(sk, "root", 1, None),
            msg(sk, "other", 2, None),
            msg(sk, "r1", 3, Some(("root", "root"))),
            msg(sk, "x", 4, Some(("other", "other"))),
            msg(sk, "r2", 5, Some(("root", "root"))),
        ];
        assert_eq!(ids(&thread_rows(&rows, sk, "root")), vec!["root", "r1", "r2"]);
        assert_eq!(ids(&thread_rows(&rows, sk, "other")), vec!["other", "x"]);
    }

    #[test]
    fn replies_to_replies_stay_in_the_thread() {
        let sk = "twitch:a";
        // Twitch: a reply to a reply names the reply as parent and the root as thread
        let rows = vec![msg(sk, "root", 1, None), msg(sk, "r1", 2, Some(("root", "root"))), msg(sk, "r2", 3, Some(("r1", "root"))), msg(sk, "r3", 4, Some(("r2", "")))];
        assert_eq!(ids(&thread_rows(&rows, sk, "root")), vec!["root", "r1", "r2", "r3"]);
        // clicking any of them finds the same root
        assert_eq!(thread_root(&rows, sk, "r1", "root"), "root");
        assert_eq!(thread_root(&rows, sk, "r2", ""), "root");
    }

    #[test]
    fn a_thread_whose_root_left_the_store() {
        let sk = "twitch:a";
        let rows = vec![msg(sk, "r1", 3, Some(("gone", "gone"))), msg(sk, "y", 4, None), msg(sk, "r2", 5, Some(("r1", "gone")))];
        assert_eq!(ids(&thread_rows(&rows, sk, "gone")), vec!["r1", "r2"]);
        assert_eq!(thread_root(&rows, sk, "gone", ""), "gone");
    }

    #[test]
    fn threads_stay_in_their_own_chat() {
        let rows = vec![msg("twitch:a", "root", 1, None), msg("twitch:b", "r1", 2, Some(("root", "root"))), msg("twitch:a", "r2", 3, Some(("root", "root")))];
        assert_eq!(ids(&thread_rows(&rows, "twitch:a", "root")), vec!["root", "r2"]);
    }

    #[test]
    fn kick_threads_follow_parents() {
        let sk = "kick:a";
        let rows = vec![kick(sk, "k0", 1, None), kick(sk, "k1", 2, Some("k0")), kick(sk, "k2", 3, None), kick(sk, "k3", 4, Some("k1"))];
        let root = thread_root(&rows, sk, "k1", "");
        assert_eq!(root, "k0");
        assert_eq!(ids(&thread_rows(&rows, sk, &root)), vec!["k0", "k1", "k3"]);
        assert!(thread_rows(&rows, sk, "").is_empty());
    }
}
