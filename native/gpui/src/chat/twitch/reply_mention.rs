//! Port of src/renderer/chat/twitch/replyMention.ts: a reply's text starts with "@parent"; the reply line above
//! already says who, so the row drops it.
use super::parts::MsgPart;

pub fn drop_reply_mention(parts: Vec<MsgPart>, parent: &str) -> Vec<MsgPart> {
    let who = parent.to_lowercase();
    let Some(first) = parts.first() else { return parts };
    match first {
        MsgPart::Mention { login, name, .. } if login.to_lowercase() == who || name.to_lowercase() == who => {
            let mut rest: Vec<MsgPart> = parts[1..].to_vec();
            if let Some(MsgPart::Text(t)) = rest.first() {
                let t = t.trim_start().to_string();
                if t.is_empty() {
                    rest.remove(0);
                } else {
                    rest[0] = MsgPart::Text(t);
                }
            }
            rest
        }
        MsgPart::Text(text) => {
            // ^@(\S+)\s*
            if let Some(after) = text.strip_prefix('@') {
                let name_len = after.find(char::is_whitespace).unwrap_or(after.len());
                let name = &after[..name_len];
                if !name.is_empty() {
                    let trimmed = name.strip_suffix([',', ':']).unwrap_or(name);
                    if trimmed.to_lowercase() == who {
                        let t = after[name_len..].trim_start();
                        let mut out: Vec<MsgPart> = Vec::new();
                        if !t.is_empty() {
                            out.push(MsgPart::Text(t.to_string()));
                        }
                        out.extend_from_slice(&parts[1..]);
                        return out;
                    }
                }
            }
            parts
        }
        _ => parts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::twitch::parts::EmoteSource;

    fn text(t: &str) -> MsgPart {
        MsgPart::Text(t.into())
    }

    #[test]
    fn drops_a_leading_plain_text_mention() {
        assert_eq!(drop_reply_mention(vec![text("@TheLadyCupcake whoever wins")], "TheLadyCupcake"), vec![text("whoever wins")]);
        assert_eq!(drop_reply_mention(vec![text("@theladycupcake, ok")], "TheLadyCupcake"), vec![text("ok")]);
    }

    #[test]
    fn drops_a_mention_part_and_the_space_after() {
        let parts = vec![MsgPart::Mention { login: "a_g_u__".into(), name: "A_g_u__".into(), color: "#fff".into(), user_id: "1".into() }, text(" hi")];
        assert_eq!(drop_reply_mention(parts, "A_g_u__"), vec![text("hi")]);
    }

    #[test]
    fn keeps_the_text_for_someone_else_or_when_only_a_mention() {
        assert_eq!(drop_reply_mention(vec![text("@someone else")], "TheLadyCupcake"), vec![text("@someone else")]);
        let emote = MsgPart::Emote { source: EmoteSource::Twitch, id: "1".into(), name: "Kappa".into(), overlays: vec![] };
        assert_eq!(drop_reply_mention(vec![text("@Bob "), emote.clone()], "bob"), vec![emote]);
    }
}
