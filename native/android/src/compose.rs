//! The message box's logic, as the desktop's send box does it, without any call to the network: what a typed text
//! turns into (a slash command's request, a confirm step, a plain send), the suggestion strip as JSON, the emote
//! picker as JSON. `session.rs` runs the plans.
use agu_chat_core::protocol::Platform;
use agu_chat_model::input::autocomplete::{Suggestions, SuggestKind};
use agu_chat_model::input::inventory::{Inventory, PickerProvider};
use agu_chat_model::input::slash::{is_me, parse_command, parse_user_arg, plan_slash, split_args, Confirmed, Planned, SlashPlan};
use serde_json::{json, Value};

/// A message is at most this many characters (the box cuts it, like the desktop's).
pub const MAX_CHARS: usize = 500;

/// UTF-16 offset of a byte offset (Kotlin strings count UTF-16 units).
pub fn utf16_of(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].encode_utf16().count()
}

/// Byte offset of a UTF-16 offset; an offset inside a surrogate pair lands after the character.
pub fn byte_of(text: &str, utf16: usize) -> usize {
    let mut n = 0;
    for (i, ch) in text.char_indices() {
        if n >= utf16 {
            return i;
        }
        n += ch.len_utf16();
    }
    text.len()
}

pub fn suggestions_json(text: &str, s: &Suggestions) -> Value {
    json!({
        "kind": match s.kind { SuggestKind::Emote => "emote", SuggestKind::Command => "command", SuggestKind::Mention => "mention", SuggestKind::Word => "word" },
        "start": utf16_of(text, s.start),
        "end": utf16_of(text, s.end),
        "items": s.items.iter().map(|i| json!({"name": i.name, "img": i.img, "hint": i.hint, "dim": i.dim, "replace": i.replace})).collect::<Vec<_>>(),
    })
}

fn provider_id(p: PickerProvider) -> &'static str {
    match p {
        PickerProvider::Twitch => "twitch",
        PickerProvider::SevenTv => "7tv",
        PickerProvider::Bttv => "bttv",
        PickerProvider::Ffz => "ffz",
        PickerProvider::Rumble => "rumble",
        PickerProvider::Kick => "kick",
        PickerProvider::YouTube => "youtube",
    }
}

fn provider_label(p: PickerProvider) -> &'static str {
    match p {
        PickerProvider::Twitch => "Twitch",
        PickerProvider::SevenTv => "7TV",
        PickerProvider::Bttv => "BTTV",
        PickerProvider::Ffz => "FFZ",
        PickerProvider::Rumble => "Rumble",
        PickerProvider::Kick => "Kick",
        PickerProvider::YouTube => "YouTube",
    }
}

/// The picker: a tab for each provider the chats have (in the desktop's order), and the sections of the chosen tab
/// (all of them when `tab` is empty), those emotes whose name contains `q` when it is not empty. A search looks in
/// every tab at once.
pub fn picker_json(inv: &Inventory, tab: &str, q: &str) -> Value {
    let order = [PickerProvider::Twitch, PickerProvider::SevenTv, PickerProvider::Bttv, PickerProvider::Ffz, PickerProvider::Rumble, PickerProvider::Kick, PickerProvider::YouTube];
    let tabs: Vec<Value> = order.iter().filter(|p| inv.sections.iter().any(|s| s.provider == **p)).map(|p| json!({"id": provider_id(*p), "label": provider_label(*p)})).collect();
    let q = q.trim().to_lowercase();
    let sections: Vec<Value> = inv
        .sections
        .iter()
        .filter(|s| !q.is_empty() || tab.is_empty() || provider_id(s.provider) == tab)
        .filter_map(|s| {
            let items: Vec<Value> = s
                .items
                .iter()
                .filter(|it| q.is_empty() || it.name.to_lowercase().contains(&q))
                .map(|it| json!({"key": it.key, "name": it.name, "img": it.img, "tip": it.tooltip, "insert": it.insert.clone().unwrap_or_else(|| it.name.clone())}))
                .collect();
            (!items.is_empty()).then(|| json!({"id": s.id, "label": s.label, "provider": provider_id(s.provider), "items": items}))
        })
        .collect();
    json!({"tabs": tabs, "sections": sections})
}

/// Where a typed text goes: the targets it is sent to, one of the chats of the box.
#[derive(Clone, Debug)]
pub struct Target {
    pub sk: String,
    pub platform: Platform,
    pub whisper: bool,
}

/// What pressing Send does with a text.
#[derive(Clone, Debug, PartialEq)]
pub enum SendPlan {
    /// nothing to send
    Nothing,
    /// a Twitch command: this request (the message, last/pinned ids filled in)
    Slash { target: String, plan: SlashPlan },
    /// /clear and /raid ask first
    Confirm { target: String, what: Confirmed, text: String },
    /// the toast to show instead (a warning) and nothing sent
    Refuse(String),
    /// the text, cut to the limit, goes to each of these chats
    Plain { text: String, targets: Vec<String> },
}

/// The desktop's `SendBox::send` up to the network: slash commands (everything but `/me`) run against a single Twitch
/// chat (a whisper is plain text), `/clear` and `/raid` need a confirm, several chats take no command.
pub fn plan_send(raw: &str, targets: &[Target], last_message_id: Option<&str>, pinned_id: Option<&str>) -> SendPlan {
    let t = raw.trim();
    if t.is_empty() || targets.is_empty() {
        return SendPlan::Nothing;
    }
    if t.starts_with('/') && !is_me(t) {
        let single = (targets.len() == 1).then(|| &targets[0]).filter(|s| s.platform == Platform::Twitch && !s.whisper);
        if let Some(s) = single {
            let (cmd, rest) = parse_command(t).unwrap_or_default();
            if cmd == "clear" {
                return SendPlan::Confirm { target: s.sk.clone(), what: Confirmed::Clear, text: "Clear the entire chat? This can't be undone.".into() };
            }
            if cmd == "raid" {
                let login = parse_user_arg(split_args(&rest).first().copied().unwrap_or(""));
                return if login.is_empty() {
                    SendPlan::Refuse("Usage: /raid <user>".into())
                } else {
                    SendPlan::Confirm { target: s.sk.clone(), text: format!("Start raiding @{login}?"), what: Confirmed::Raid { login } }
                };
            }
            return match plan_slash(t, last_message_id, pinned_id) {
                Ok(Some(Planned::Plan(plan))) => SendPlan::Slash { target: s.sk.clone(), plan },
                Ok(Some(Planned::Unsupported(msg))) => SendPlan::Refuse(msg),
                Ok(None) => SendPlan::Nothing,
                Err(e) => SendPlan::Refuse(e),
            };
        }
        if targets.len() > 1 {
            return SendPlan::Refuse("Commands run on a single Twitch chat: pick it in the target menu".into());
        }
    }
    SendPlan::Plain { text: t.chars().take(MAX_CHARS).collect(), targets: targets.iter().map(|s| s.sk.clone()).collect() }
}

/// The toast lines for the targets that failed, in target order; with several targets each names its platform.
pub fn send_failures(outcomes: Vec<(Platform, Option<String>)>, multi: bool) -> Vec<String> {
    outcomes
        .into_iter()
        .filter_map(|(p, err)| err.map(|e| if multi { format!("{}: {e}", platform_label(p)) } else { e }))
        .collect()
}

pub fn platform_label(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => "Twitch",
        Platform::Kick => "Kick",
        Platform::Youtube => "YouTube",
        Platform::X => "X",
        Platform::Rumble => "Rumble",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_model::input::inventory::{PickerItem, PickerSection};

    fn tw(sk: &str) -> Target {
        Target { sk: sk.into(), platform: Platform::Twitch, whisper: false }
    }
    fn kick(sk: &str) -> Target {
        Target { sk: sk.into(), platform: Platform::Kick, whisper: false }
    }
    fn slash(raw: &str) -> SlashPlan {
        match plan_send(raw, &[tw("twitch:a")], Some("m-last"), Some("m-pinned")) {
            SendPlan::Slash { plan, .. } => plan,
            other => panic!("{raw}: {other:?}"),
        }
    }

    #[test]
    fn commands_map_to_the_relay_requests_the_desktop_makes() {
        let p = slash("/ban @Bob spamming");
        assert_eq!((p.path, p.method), ("/api/mod/action", "POST"));
        assert_eq!(p.body.unwrap(), json!({"action": "ban", "userLogin": "bob", "reason": "spamming"}));
        let p = slash("/timeout bob 10m");
        assert_eq!(p.body.unwrap(), json!({"action": "timeout", "userLogin": "bob", "duration": 600}));
        let p = slash("/slow 30");
        assert_eq!((p.path, p.method), ("/api/chat/settings", "POST"));
        assert_eq!(p.body.unwrap(), json!({"slow_mode": true, "slow_mode_wait_time": 30}));
        let p = slash("/announce purple Hello chat");
        assert_eq!((p.path, p.method), ("/api/chat/announce", "POST"));
        assert_eq!(p.body.unwrap(), json!({"text": "Hello chat", "color": "purple"}));
        let p = slash("/pin");
        assert_eq!(p.body.unwrap(), json!({"action": "pin", "messageId": "m-last", "durationSeconds": 1200}));
        let p = slash("/unpin");
        assert_eq!(p.body.unwrap(), json!({"action": "unpin", "messageId": "m-pinned"}));
        let p = slash("/shield on");
        assert_eq!((p.path, p.method), ("/api/chat/shield", "PUT"));
        let p = slash("/unraid");
        assert_eq!((p.path, p.method, p.body), ("/api/chat/raid", "DELETE", None));
        let p = slash("/delete abc123");
        assert_eq!(p.body.unwrap(), json!({"action": "delete", "messageId": "abc123"}));
        let p = slash("/shoutout @Someone");
        assert_eq!((p.path, p.body.unwrap()), ("/api/chat/shoutout", json!({"login": "someone"})));
    }

    #[test]
    fn clear_and_raid_ask_first() {
        match plan_send("/clear", &[tw("twitch:a")], None, None) {
            SendPlan::Confirm { what, text, target } => {
                assert_eq!((what, target.as_str()), (Confirmed::Clear, "twitch:a"));
                assert_eq!(text, "Clear the entire chat? This can't be undone.");
            }
            o => panic!("{o:?}"),
        }
        match plan_send("/raid @Friend", &[tw("twitch:a")], None, None) {
            SendPlan::Confirm { what, text, .. } => {
                assert_eq!(what, Confirmed::Raid { login: "friend".into() });
                assert_eq!(text, "Start raiding @friend?");
            }
            o => panic!("{o:?}"),
        }
        assert_eq!(plan_send("/raid", &[tw("twitch:a")], None, None), SendPlan::Refuse("Usage: /raid <user>".into()));
    }

    #[test]
    fn bad_and_unknown_commands_say_so_and_send_nothing() {
        assert_eq!(plan_send("/ban", &[tw("t")], None, None), SendPlan::Refuse("Usage: /ban <user> [reason]".into()));
        assert_eq!(plan_send("/timeout bob", &[tw("t")], None, None), SendPlan::Refuse("Usage: /timeout <user> <duration> [reason]".into()));
        assert_eq!(plan_send("/pin", &[tw("t")], None, None), SendPlan::Refuse("Nothing to pin yet".into()));
        match plan_send("/vip bob", &[tw("t")], None, None) {
            SendPlan::Refuse(t) => assert!(t.contains("\"/vip\" isn't supported"), "{t}"),
            o => panic!("{o:?}"),
        }
        // not a command: a "/" alone, or empty text
        assert_eq!(plan_send("   ", &[tw("t")], None, None), SendPlan::Nothing);
        assert_eq!(plan_send("hi", &[], None, None), SendPlan::Nothing);
    }

    #[test]
    fn me_and_other_platforms_and_whispers_send_the_text_as_it_is() {
        let plain = |raw: &str, t: &[Target]| match plan_send(raw, t, None, None) {
            SendPlan::Plain { text, targets } => (text, targets),
            o => panic!("{o:?}"),
        };
        assert_eq!(plain("/me waves", &[tw("t")]), ("/me waves".to_string(), vec!["t".to_string()]));
        assert_eq!(plain("/ban bob", &[kick("k")]).0, "/ban bob");
        let whisper = Target { sk: "w".into(), platform: Platform::Twitch, whisper: true };
        assert_eq!(plain("/ban bob", &[whisper]).0, "/ban bob");
        // several chats: no commands
        assert_eq!(plan_send("/slowoff", &[tw("t"), kick("k")], None, None), SendPlan::Refuse("Commands run on a single Twitch chat: pick it in the target menu".into()));
        // but a plain text goes to all, and is cut at 500 characters
        let long = "x".repeat(600);
        let (text, targets) = plain(&long, &[tw("t"), kick("k")]);
        assert_eq!((text.chars().count(), targets.len()), (MAX_CHARS, 2));
    }

    #[test]
    fn failures_name_the_platform_only_for_several_targets() {
        let out = vec![(Platform::Twitch, None), (Platform::Kick, Some("slow mode".to_string()))];
        assert_eq!(send_failures(out.clone(), true), vec!["Kick: slow mode".to_string()]);
        assert_eq!(send_failures(out, false), vec!["slow mode".to_string()]);
    }

    #[test]
    fn utf16_offsets_round_trip() {
        let t = "a\u{e9}\u{1F600}b";
        assert_eq!(utf16_of(t, 0), 0);
        assert_eq!(utf16_of(t, 1), 1);
        assert_eq!(utf16_of(t, 3), 2);
        assert_eq!(utf16_of(t, 7), 4);
        assert_eq!(byte_of(t, 2), 3);
        assert_eq!(byte_of(t, 4), 7);
        assert_eq!(byte_of(t, 99), t.len());
    }

    #[test]
    fn the_picker_has_a_tab_per_provider_and_searches_all_of_them() {
        let item = |k: &str, n: &str| PickerItem { key: k.into(), name: n.into(), img: format!("https://x/{k}"), tooltip: n.into(), insert: None };
        let inv = Inventory {
            sections: vec![
                PickerSection { id: "twitch-global".into(), label: "Twitch Global".into(), provider: PickerProvider::Twitch, items: vec![item("tw:1", "Kappa"), item("tw:2", "LUL")] },
                PickerSection { id: "7tv-channel".into(), label: "7TV Channel".into(), provider: PickerProvider::SevenTv, items: vec![item("7tv:1", "KappaHD")] },
                PickerSection { id: "kick".into(), label: "Kick".into(), provider: PickerProvider::Kick, items: vec![PickerItem { insert: Some("[emote:5:KEK]".into()), ..item("kick:KEK", "KEK") }] },
            ],
            inventory: vec![],
        };
        let all = picker_json(&inv, "", "");
        assert_eq!(all["tabs"].as_array().unwrap().iter().map(|t| t["label"].as_str().unwrap()).collect::<Vec<_>>(), vec!["Twitch", "7TV", "Kick"]);
        assert_eq!(all["sections"].as_array().unwrap().len(), 3);
        let tab = picker_json(&inv, "7tv", "");
        assert_eq!(tab["sections"].as_array().unwrap().len(), 1);
        assert_eq!(tab["sections"][0]["items"][0]["insert"], "KappaHD");
        // a search ignores the tab and drops what does not match
        let q = picker_json(&inv, "kick", "kap");
        let names: Vec<&str> = q["sections"].as_array().unwrap().iter().flat_map(|s| s["items"].as_array().unwrap().iter().map(|i| i["name"].as_str().unwrap())).collect();
        assert_eq!(names, vec!["Kappa", "KappaHD"]);
        // Kick's picker inserts its wire text
        assert_eq!(picker_json(&inv, "kick", "")["sections"][0]["items"][0]["insert"], "[emote:5:KEK]");
    }
}
