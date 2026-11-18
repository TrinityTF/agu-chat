//! YouTube's chat modes as far as an anonymous reader sees them: the "Slow mode is on" / "Members-only mode is on"
//! lines YouTube puts in the chat (`liveChatModeChangeMessageRenderer`), and the restriction text of the chat's
//! action panel. Whether the logged-in account is a member is not visible here, so members-only is shown as a lock
//! but never stops sending (YouTube answers a refused message itself).
use serde_json::Value;

use crate::chat_rules::ChatModes;

fn text_of(v: &Value) -> String {
    if let Some(s) = v["simpleText"].as_str() {
        return s.to_string();
    }
    v["runs"].as_array().map(|r| r.iter().filter_map(|x| x["text"].as_str()).collect::<String>()).unwrap_or_default()
}

/// Seconds in "Send a message every 5 seconds" / "... every 2 minutes".
fn seconds_in(s: &str) -> Option<i64> {
    let words: Vec<&str> = s.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Ok(n) = w.trim_matches(|c: char| !c.is_ascii_digit()).parse::<i64>() {
            let unit = words.get(i + 1).map(|u| u.to_lowercase()).unwrap_or_default();
            return Some(if unit.starts_with("min") { n * 60 } else { n });
        }
    }
    None
}

/// A chat action that is a mode change, applied to `m`; true when it was one.
pub fn apply_mode_change(m: &mut ChatModes, action: &Value) -> bool {
    let r = &action["addChatItemAction"]["item"]["liveChatModeChangeMessageRenderer"];
    if !r.is_object() {
        return false;
    }
    let text = text_of(&r["text"]);
    let sub = text_of(&r["subtext"]);
    let lower = text.to_lowercase();
    let on = !(lower.contains(" off") || lower.contains("turned off") || lower.contains("disabled"));
    if lower.contains("slow mode") {
        m.slow_secs = if on { seconds_in(&sub).or_else(|| seconds_in(&text)).unwrap_or(1) } else { 0 };
    } else if lower.contains("members") {
        m.members_only = on;
    } else if lower.contains("subscribers") {
        m.subs_only = on;
    } else {
        return false;
    }
    true
}

/// The action panel's restriction text ("Members-only mode. Join this channel ..."), when the chat has one.
pub fn panel_text(cc: &Value) -> Option<String> {
    let r = &cc["actionPanel"]["liveChatRestrictedParticipationRenderer"];
    r.is_object().then(|| text_of(&r["message"])).filter(|t| !t.is_empty())
}

/// What the panel says about the modes (it may only say "Sign in to chat", which says nothing).
pub fn apply_panel(m: &mut ChatModes, text: &str) {
    let lower = text.to_lowercase();
    if lower.contains("members-only") || lower.contains("members only") {
        m.members_only = true;
    }
    if lower.contains("subscribers-only") || lower.contains("subscribers only") {
        m.subs_only = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn change(text: &str, sub: &str) -> Value {
        json!({"addChatItemAction": {"item": {"liveChatModeChangeMessageRenderer": {"id": "x", "icon": {"iconType": "SLOW_MODE"}, "text": {"runs": [{"text": text, "bold": true}]}, "subtext": {"runs": [{"text": sub}]}}}}})
    }

    #[test]
    fn mode_lines_turn_modes_on_and_off() {
        let mut m = ChatModes::default();
        assert!(apply_mode_change(&mut m, &change("Slow mode is on", "Send a message every 5 seconds")));
        assert_eq!(m.slow_secs, 5);
        assert!(apply_mode_change(&mut m, &change("Slow mode is on", "Send a message every 2 minutes")));
        assert_eq!(m.slow_secs, 120);
        assert!(apply_mode_change(&mut m, &change("Members-only mode is on", "Only members of this channel can chat")));
        assert!(m.members_only);
        assert!(apply_mode_change(&mut m, &change("Members-only mode is off", "")));
        assert!(apply_mode_change(&mut m, &change("Slow mode is off", "")));
        assert_eq!(m, ChatModes::default());
        assert!(apply_mode_change(&mut m, &change("Subscribers-only mode is on", "")));
        assert!(m.subs_only);
    }

    #[test]
    fn other_lines_are_left_alone() {
        let mut m = ChatModes::default();
        assert!(!apply_mode_change(&mut m, &json!({"addChatItemAction": {"item": {"liveChatTextMessageRenderer": {}}}})));
        assert!(!apply_mode_change(&mut m, &change("Something new", "")));
        assert_eq!(m, ChatModes::default());
    }

    #[test]
    fn the_action_panel() {
        let cc = json!({"actionPanel": {"liveChatRestrictedParticipationRenderer": {"message": {"runs": [{"text": "Members-only mode. "}, {"text": "Join this channel"}, {"text": " to chat."}]}}}});
        let t = panel_text(&cc).unwrap();
        let mut m = ChatModes::default();
        apply_panel(&mut m, &t);
        assert!(m.members_only);
        let mut m = ChatModes::default();
        apply_panel(&mut m, "Sign in to chat");
        assert_eq!(m, ChatModes::default());
        assert_eq!(panel_text(&json!({"actionPanel": {"liveChatMessageInputRenderer": {}}})), None);
    }
}
