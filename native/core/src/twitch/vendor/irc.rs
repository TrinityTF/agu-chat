//! Port of src/main/twitch/vendor/irc.js (vendored from modtools.agu.lol render/lib/irc.js, render commit
//! 7704aec). Change carried over: parse_irc_line also reads a last parameter sent without the ':' prefix
//! (recent-messages sends one-word messages that way).
//!
//! IRC line parsing plus the PRIVMSG -> ChatMsg and USERNOTICE -> Notice builders, mirrored from the Durable
//! Object's chat pipeline so this relay speaks the exact same wire protocol. build_notice() forwards EVERY
//! USERNOTICE that carries a system-msg tag.
use std::collections::HashMap;

use serde_json::{json, Value};

use crate::util::{js_int_or, js_trim, now_ms, uuid_v4};

/// One parsed IRC line (`{tags, command, channel, user, text}`).
#[derive(Debug, Clone, Default)]
pub struct IrcLine {
    pub tags: HashMap<String, String>,
    pub command: String,
    pub channel: String,
    pub user: String,
    pub text: String,
}

impl IrcLine {
    pub fn tag(&self, k: &str) -> &str {
        self.tags.get(k).map(String::as_str).unwrap_or("")
    }
}

/// IRC /me messages arrive as the CTCP-wrapped text \x01ACTION <text>\x01: strip the wrapper (the trailing
/// \x01 is optional defensively). JS: `/^\x01?ACTION (.*?)\x01?$/`; returns the inner text.
pub fn action_text(text: &str) -> Option<String> {
    let t = text.strip_prefix('\u{1}').unwrap_or(text);
    let t = t.strip_prefix("ACTION ")?;
    let t = t.strip_suffix('\u{1}').unwrap_or(t);
    // `.` does not match line breaks
    if t.contains(['\n', '\r', '\u{2028}', '\u{2029}']) {
        return None;
    }
    Some(t.to_string())
}

fn unescape_tag(v: &str) -> String {
    // v.replace(/\\(.?)/g, (_, c) => c === "s" ? " " : c === "n" ? "\n" : c === "r" ? "\r" : c === ":" ? ";" : c)
    let mut out = String::with_capacity(v.len());
    let mut it = v.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('s') => out.push(' '),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(':') => out.push(';'),
            Some(o) => out.push(o),
            None => {}
        }
    }
    out
}

/// JS `s.slice(1, -1)`
fn inner(s: &str) -> &str {
    let mut chars = s.chars();
    chars.next();
    chars.next_back();
    chars.as_str()
}

/// Parses one IRC line: `@tag=val;tag2=val2 :nick!user@host PRIVMSG #chan :text`
pub fn parse_irc_line(line: &str) -> IrcLine {
    let mut out = IrcLine::default();
    let mut rest = line;
    if rest.starts_with('@') {
        let sp = rest.find(' ');
        let tag_str = match sp {
            Some(sp) => &rest[1..sp],
            None => inner(rest),
        };
        for kv in tag_str.split(';') {
            let (k, v) = match kv.find('=') {
                None => (kv, String::new()),
                Some(eq) => (&kv[..eq], unescape_tag(&kv[eq + 1..])),
            };
            out.tags.insert(k.to_string(), v);
        }
        if let Some(sp) = sp {
            rest = &rest[sp + 1..];
        }
    }
    if rest.starts_with(':') {
        let sp = rest.find(' ');
        let prefix = match sp {
            Some(sp) => &rest[1..sp],
            None => inner(rest),
        };
        out.user = prefix.split('!').next().unwrap_or("").to_string();
        if let Some(sp) = sp {
            rest = &rest[sp + 1..];
        }
    }
    let trail_idx = rest.find(" :");
    let mut head = rest;
    if let Some(i) = trail_idx {
        out.text = rest[i + 2..].to_string();
        head = &rest[..i];
    }
    let parts: Vec<&str> = head.split(' ').collect();
    out.command = parts[0].to_string();
    out.channel = parts.iter().find(|p| p.starts_with('#')).map(|p| p[1..].to_string()).unwrap_or_default();
    // IRC only needs the ':' when the last parameter has spaces: `PRIVMSG #chan word` is valid.
    if trail_idx.is_none() && parts.len() >= 3 {
        out.text = parts[parts.len() - 1].to_string();
    }
    out
}

/// Builds a chat-page ChatMsg from a parsed PRIVMSG line. `display_user` is `tags["display-name"] || m.user`
/// (pass "" for none).
pub fn build_chat_msg(m: &IrcLine, display_user: &str) -> Value {
    let action = action_text(&m.text);
    let reply_user = m.tag("reply-parent-display-name");
    let or_null = |k: &str| {
        let v = m.tag(k);
        if v.is_empty() {
            Value::Null
        } else {
            json!(v)
        }
    };
    let id = if m.tag("id").is_empty() { uuid_v4() } else { m.tag("id").to_string() };
    let name = if display_user.is_empty() { m.user.as_str() } else { display_user };
    json!({
        "platform": "twitch",
        "id": id,
        "ts": js_int_or(m.tag("tmi-sent-ts"), now_ms()),
        "user": { "id": m.tag("user-id"), "login": m.user, "name": name, "color": m.tag("color") },
        "badges": m.tag("badges"),
        // months behind a subscriber / founder badge ("subscriber/14"): the user card's tier line
        "badgeInfo": m.tag("badge-info"),
        "text": action.clone().unwrap_or_else(|| m.text.clone()),
        "emotes": m.tag("emotes"),
        // Twitch's GIF keyboard (tier 2/3 subs): `<start>-<end>|<gifID>|<gifURL>`, comma separated. The URL is
        // passed straight through, never parsed or proxied. PRIVMSG only.
        "gifs": m.tag("gifs"),
        "action": action.is_some(),
        "firstMsg": m.tag("first-msg") == "1",
        "reply": if reply_user.is_empty() { Value::Null } else { json!({ "user": reply_user, "text": m.tag("reply-parent-msg-body") }) },
        "rewardId": or_null("custom-reward-id"),
        "msgId": or_null("msg-id"),
        "returning": m.tag("returning-chatter") == "1",
        // A cheer: the Bits on this message. 0 for every ordinary message.
        "bits": js_int_or(m.tag("bits"), 0),
    })
}

/// Turn an IRC PRIVMSG's `badges` tag (plus the mod/subscriber/vip flag tags) into the `{set_id}` shape.
pub fn irc_badges(tags: &HashMap<String, String>) -> Vec<Value> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let get = |k: &str| tags.get(k).map(String::as_str).unwrap_or("");
    for part in get("badges").split(',') {
        let id = part.split('/').next().unwrap_or("");
        if !id.is_empty() && seen.insert(id.to_string()) {
            out.push(json!({ "set_id": id }));
        }
    }
    if get("mod") == "1" && !seen.contains("moderator") {
        out.push(json!({ "set_id": "moderator" }));
    }
    if get("subscriber") == "1" && !seen.contains("subscriber") && !seen.contains("founder") {
        out.push(json!({ "set_id": "subscriber" }));
    }
    if get("vip") == "1" && !seen.contains("vip") {
        out.push(json!({ "set_id": "vip" }));
    }
    out
}

/// Builds a Notice from a parsed USERNOTICE line (sub/resub/raid/announcement/...). None when the line
/// carries no system-msg.
pub fn build_notice(m: &IrcLine) -> Option<Value> {
    let sys_msg = js_trim(m.tag("system-msg"));
    if sys_msg.is_empty() {
        return None;
    }
    let id = if m.tag("id").is_empty() { uuid_v4() } else { m.tag("id").to_string() };
    let name = if m.tag("display-name").is_empty() { m.user.as_str() } else { m.tag("display-name") };
    Some(json!({
        "id": id,
        "ts": now_ms(),
        "system": sys_msg,
        "msgId": m.tag("msg-id"),
        "user": { "id": m.tag("user-id"), "login": m.user, "name": name, "color": m.tag("color") },
        "text": js_trim(&m.text),
        // The attached user text carries its own emote positions on USERNOTICE lines exactly like PRIVMSG's
        // `emotes` tag. "" when the message has none.
        "emotes": m.tag("emotes"),
        // Shared chat wraps every notice as msg-id "sharedchatnotice" and puts the real kind (sub, raid, ...)
        // here; the chat page sorts on whichever is set so its show/hide toggles work on both.
        "sourceMsgId": m.tag("source-msg-id"),
        // A gift bomb is one "submysterygift" line followed by one "subgift" per recipient, all carrying the
        // same community gift id; the chat page folds the per-recipient lines under the first one.
        "giftId": m.tag("msg-param-community-gift-id"),
        "giftCount": js_int_or(m.tag("msg-param-mass-gift-count"), 0),
    }))
}

#[cfg(test)]
mod tests {
    //! Port of test/irc.test.mjs
    use super::*;

    #[test]
    fn reads_the_trailing_text_after_space_colon() {
        let m = parse_irc_line("@id=1 :bob!bob@bob.tmi.twitch.tv PRIVMSG #chan :hello there");
        assert_eq!(m.command, "PRIVMSG");
        assert_eq!(m.channel, "chan");
        assert_eq!(m.user, "bob");
        assert_eq!(m.text, "hello there");
    }

    #[test]
    fn reads_a_one_word_message_sent_without_the_colon() {
        let m = parse_irc_line("@emote-only=1;id=2 :silveraku!silveraku@silveraku.tmi.twitch.tv PRIVMSG #zackrawrr anthor6Birbviolence");
        assert_eq!(m.text, "anthor6Birbviolence");
        assert_eq!(m.channel, "zackrawrr");
    }

    #[test]
    fn lines_with_no_message_stay_empty() {
        assert_eq!(parse_irc_line(":bob!bob@bob.tmi.twitch.tv JOIN #chan").text, "");
    }

    #[test]
    fn tags_unescape_and_chat_msg_shape() {
        let m = parse_irc_line("@badges=moderator/1;color=#FF0000;display-name=Bob;id=abc;tmi-sent-ts=1700000000123;user-id=7;bits=100;reply-parent-display-name=Al;reply-parent-msg-body=hi\\sthere :bob!bob@bob.tmi.twitch.tv PRIVMSG #chan :\u{1}ACTION waves\u{1}");
        let msg = build_chat_msg(&m, "Bob");
        assert_eq!(msg["text"], "waves");
        assert_eq!(msg["action"], true);
        assert_eq!(msg["ts"], 1_700_000_000_123i64);
        assert_eq!(msg["bits"], 100);
        assert!(msg["bits"].is_i64());
        assert_eq!(msg["reply"], json!({"user":"Al","text":"hi there"}));
        assert_eq!(msg["rewardId"], Value::Null);
        assert_eq!(msg["user"]["color"], "#FF0000");
    }

    #[test]
    fn notices_need_a_system_msg() {
        let m = parse_irc_line("@msg-id=sub;system-msg=Bob\\ssubscribed;display-name=Bob :tmi.twitch.tv USERNOTICE #chan :hey");
        let n = build_notice(&m).unwrap();
        assert_eq!(n["system"], "Bob subscribed");
        assert_eq!(n["text"], "hey");
        assert!(build_notice(&parse_irc_line(":tmi.twitch.tv USERNOTICE #chan")).is_none());
    }
}
