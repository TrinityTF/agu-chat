//! Which rows of a store a chat list shows: the event toggles, deleted-message hiding (the part `ignores` and the
//! search apply after this). Shared by the desktop pane and the Android views.
use std::collections::HashSet;

use agu_chat_core::protocol::Settings;

use super::twitch::events::{kick_event_visible, notice_visible};
use super::types::*;

/// Rows after the event toggles and deleted-message hiding.
pub fn visible_rows(rows: &[RowRef], rs: &Settings) -> Vec<RowRef> {
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
            RowBody::Notice(n) => n.seventv.is_some() || notice_visible(&n.msg_id, n.source_msg_id.as_deref(), n.gift_id.as_deref(), &bombs),
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
