//! Which rows of a store a chat list shows: the event toggles, deleted-message hiding (the part `ignores` and the
//! search apply after this). Shared by the desktop pane and the Android views.
use std::collections::HashSet;

use agu_chat_core::protocol::Settings;

use super::twitch::events::{kick_event_visible, notice_kind, notice_visible, ChatEventKind};
use super::types::*;

/// Rows after the event toggles and deleted-message hiding (announcements stay hidden, like the site's default).
pub fn visible_rows(rows: &[RowRef], rs: &Settings) -> Vec<RowRef> {
    visible_rows_with(rows, rs, false)
}

/// `announcements`: also show a streamer's announcements (neither app does: the desktop's event toggles keep them
/// off, and Android follows it).
pub fn visible_rows_with(rows: &[RowRef], rs: &Settings, announcements: bool) -> Vec<RowRef> {
    let mut bombs: HashSet<String> = HashSet::new();
    for r in rows {
        if let RowBody::Notice(n) = &r.body {
            if let Some(g) = n.gift_id.as_ref().filter(|g| !g.is_empty()) {
                let id = n.source_msg_id.clone().filter(|s| !s.is_empty()).unwrap_or(n.msg_id.clone());
                if id.ends_with("submysterygift") {
                    bombs.insert(g.clone());
                }
            }
        }
    }
    rows.iter()
        .filter(|r| match &r.body {
            RowBody::Notice(n) => {
                n.seventv.is_some()
                    || (announcements && notice_kind(&n.msg_id, n.source_msg_id.as_deref()) == Some(ChatEventKind::Announcements))
                    || notice_visible(&n.msg_id, n.source_msg_id.as_deref(), n.gift_id.as_deref(), &bombs)
            }
            RowBody::ModFeed(_) => rs.show_mod_actions,
            RowBody::Automod(_) => rs.show_automod,
            RowBody::Msg(m) => !(m.deleted && rs.deleted_messages == "hide"),
            RowBody::Rumble(m) => !(m.deleted && rs.deleted_messages == "hide"),
            RowBody::Kick(m) => !(m.deleted && rs.deleted_messages == "hide") && kick_event_visible(m.notice.as_deref()),
            _ => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Platform;
    use std::sync::Arc;

    fn notice(msg_id: &str) -> RowRef {
        Arc::new(Row { key: msg_id.into(), ts: 1, sk: "twitch:c".into(), platform: Platform::Twitch, ord: 0, backlog: false, body: RowBody::Notice(Notice { id: msg_id.into(), msg_id: msg_id.into(), system: "x".into(), ..Default::default() }) })
    }

    #[test]
    fn announcements_are_hidden_unless_asked_for() {
        let rows = vec![notice("announcement"), notice("sub")];
        let s = Settings::default();
        assert_eq!(visible_rows(&rows, &s).len(), 1);
        assert_eq!(visible_rows_with(&rows, &s, true).len(), 2);
    }
}
