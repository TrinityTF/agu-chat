//! Chat modes (followers-only, subscribers-only, slow mode, ...) and whether they keep the logged-in account from
//! chatting. Sources put what they know in the `conn` frame (`chatModes`, and `chatViewer` when the platform says
//! who the viewer is in the channel); the send box turns the pair into a lock + reason, or a muted mode hint.
use serde::{Deserialize, Serialize};

/// The chat modes of a channel, the same for every platform. Off / unknown is the default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatModes {
    /// Followers-only: how long one must have followed, in minutes (0 = any follow). None = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub followers_min: Option<i64>,
    pub subs_only: bool,
    /// YouTube's members-only chat
    pub members_only: bool,
    pub emote_only: bool,
    /// seconds between two messages (0 = off)
    pub slow_secs: i64,
    /// Twitch's unique chat (r9k)
    pub unique_chat: bool,
    /// Kick: how old an account must be to chat, in minutes. None = off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_age_min: Option<i64>,
}

impl ChatModes {
    pub fn any(&self) -> bool {
        *self != ChatModes::default()
    }
}

/// What a platform says about the logged-in account in the channel. Unknown fields stay None and never block.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChatViewer {
    /// broadcaster, moderator, VIP or staff: chat modes do not apply
    pub exempt: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribed: Option<bool>,
    /// Some(false) = known not to follow; Some(true) with `following_since` = follows since then.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follows: Option<bool>,
    /// ms since the epoch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub following_since: Option<i64>,
    /// ms since the epoch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_created: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateLevel {
    /// the rule keeps this account from chatting: lock, sending off
    Blocked,
    /// a rule may keep this account from chatting, but the platform does not say: lock, sending stays on
    Restricted,
    /// modes are on but do not stop this account: a muted hint
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatGate {
    pub level: GateLevel,
    pub text: String,
}

fn plural(n: i64, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

/// Minutes as people say them: "45 min", "1 h 30 min", "4 days", "3 months".
pub fn duration_text(min: i64) -> String {
    let min = min.max(1);
    if min < 60 {
        return format!("{min} min");
    }
    if min < 1440 {
        let (h, m) = (min / 60, min % 60);
        return if m == 0 { format!("{h} h") } else { format!("{h} h {m} min") };
    }
    // Twitch's longest follow time is 3 months (129600 min), Kick's account age 1 month = 43830 min
    if min >= 43_200 {
        let months = ((min as f64) / 43_830.0).round().max(1.0) as i64;
        return plural(months, "month");
    }
    let (d, h) = (min / 1440, (min % 1440) / 60);
    if h == 0 { plural(d, "day") } else { format!("{} {h} h", plural(d, "day")) }
}

/// The modes as short hints, in the order the websites show them.
fn mode_hints(m: &ChatModes) -> Vec<String> {
    let mut out = Vec::new();
    if m.members_only {
        out.push("Members-only chat".to_string());
    }
    if m.subs_only {
        out.push("Subscribers-only chat".to_string());
    }
    if let Some(f) = m.followers_min {
        out.push(if f > 0 { format!("Followers-only chat ({})", duration_text(f)) } else { "Followers-only chat".to_string() });
    }
    if m.emote_only {
        out.push("Emote-only chat".to_string());
    }
    if m.slow_secs > 0 {
        out.push(format!("Slow mode {}", secs_text(m.slow_secs)));
    }
    if m.unique_chat {
        out.push("Unique chat".to_string());
    }
    out
}

fn secs_text(s: i64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s % 60 == 0 {
        format!("{} min", s / 60)
    } else {
        format!("{} min {}s", s / 60, s % 60)
    }
}

/// What the send box says about the channel's modes for this viewer; None when no mode is on.
pub fn chat_gate(m: &ChatModes, v: &ChatViewer, now_ms: i64) -> Option<ChatGate> {
    if !m.any() {
        return None;
    }
    let hints = mode_hints(m);
    let info = || (!hints.is_empty()).then(|| ChatGate { level: GateLevel::Info, text: hints.join(" · ") });
    if v.exempt {
        return info();
    }
    let blocked = |text: String| Some(ChatGate { level: GateLevel::Blocked, text });
    let mut maybe: Option<String> = None;
    for (on, label) in [(m.members_only, "Members-only chat"), (m.subs_only, "Subscribers-only chat")] {
        if !on {
            continue;
        }
        match v.subscribed {
            Some(false) => return blocked(label.to_string()),
            None => {
                maybe.get_or_insert_with(|| label.to_string());
            }
            Some(true) => {}
        }
    }
    if let Some(need) = m.followers_min {
        match (v.follows, v.following_since) {
            (Some(false), _) => {
                return blocked(if need > 0 { format!("Followers-only chat: follow for {} first", duration_text(need)) } else { "Followers-only chat: follow to chat".to_string() });
            }
            (Some(true), Some(since)) if need > 0 => {
                let followed = (now_ms - since).max(0) / 60_000;
                if followed < need {
                    return blocked(format!("Followers-only chat: you can chat in {}", duration_text(need - followed)));
                }
            }
            (Some(true), _) => {}
            (None, _) => {
                maybe.get_or_insert_with(|| if need > 0 { format!("Followers-only chat ({} after following)", duration_text(need)) } else { "Followers-only chat".to_string() });
            }
        }
    }
    if let (Some(need), Some(created)) = (m.account_age_min, v.account_created) {
        let age = (now_ms - created).max(0) / 60_000;
        if age < need {
            return blocked(format!("Accounts must be {} old to chat here", duration_text(need)));
        }
    }
    if let Some(text) = maybe {
        // the other modes still matter (slow mode, emote-only)
        let rest: Vec<&String> = hints.iter().filter(|h| !h.starts_with(text.split(' ').next().unwrap_or(""))).collect();
        let text = std::iter::once(&text).chain(rest).cloned().collect::<Vec<_>>().join(" · ");
        return Some(ChatGate { level: GateLevel::Restricted, text });
    }
    info()
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;
    const MIN: i64 = 60_000;

    fn gate(m: &ChatModes, v: &ChatViewer) -> Option<(GateLevel, String)> {
        chat_gate(m, v, NOW).map(|g| (g.level, g.text))
    }

    fn known(subscribed: bool, follows_for_min: Option<i64>) -> ChatViewer {
        ChatViewer { exempt: false, subscribed: Some(subscribed), follows: Some(follows_for_min.is_some()), following_since: follows_for_min.map(|m| NOW - m * MIN), account_created: Some(NOW - 400 * 24 * 60 * MIN) }
    }

    #[test]
    fn no_modes_no_gate() {
        assert_eq!(gate(&ChatModes::default(), &ChatViewer::default()), None);
    }

    #[test]
    fn followers_only_blocks_until_followed_long_enough() {
        let m = ChatModes { followers_min: Some(60), ..Default::default() };
        assert_eq!(gate(&m, &known(false, None)), Some((GateLevel::Blocked, "Followers-only chat: follow for 1 h first".into())));
        assert_eq!(gate(&m, &known(false, Some(15))), Some((GateLevel::Blocked, "Followers-only chat: you can chat in 45 min".into())));
        assert_eq!(gate(&m, &known(false, Some(61))), Some((GateLevel::Info, "Followers-only chat (1 h)".into())));
        let any = ChatModes { followers_min: Some(0), ..Default::default() };
        assert_eq!(gate(&any, &known(false, None)), Some((GateLevel::Blocked, "Followers-only chat: follow to chat".into())));
        assert_eq!(gate(&any, &known(false, Some(0))), Some((GateLevel::Info, "Followers-only chat".into())));
    }

    #[test]
    fn subscribers_and_members_only() {
        let subs = ChatModes { subs_only: true, ..Default::default() };
        assert_eq!(gate(&subs, &known(false, Some(5))), Some((GateLevel::Blocked, "Subscribers-only chat".into())));
        assert_eq!(gate(&subs, &known(true, None)).unwrap().0, GateLevel::Info);
        let members = ChatModes { members_only: true, ..Default::default() };
        assert_eq!(gate(&members, &ChatViewer { subscribed: Some(false), ..Default::default() }), Some((GateLevel::Blocked, "Members-only chat".into())));
    }

    #[test]
    fn unknown_status_is_a_lock_but_not_a_block() {
        let m = ChatModes { members_only: true, slow_secs: 5, ..Default::default() };
        assert_eq!(gate(&m, &ChatViewer::default()), Some((GateLevel::Restricted, "Members-only chat · Slow mode 5s".into())));
        let f = ChatModes { followers_min: Some(30), ..Default::default() };
        assert_eq!(gate(&f, &ChatViewer::default()), Some((GateLevel::Restricted, "Followers-only chat (30 min after following)".into())));
    }

    #[test]
    fn mods_vips_and_the_broadcaster_are_exempt() {
        let m = ChatModes { subs_only: true, followers_min: Some(10), slow_secs: 30, emote_only: true, ..Default::default() };
        let mod_ = ChatViewer { exempt: true, subscribed: Some(false), follows: Some(false), ..Default::default() };
        assert_eq!(gate(&m, &mod_), Some((GateLevel::Info, "Subscribers-only chat · Followers-only chat (10 min) · Emote-only chat · Slow mode 30s".into())));
    }

    #[test]
    fn modes_that_never_block_are_hints() {
        let m = ChatModes { slow_secs: 5, ..Default::default() };
        assert_eq!(gate(&m, &ChatViewer::default()), Some((GateLevel::Info, "Slow mode 5s".into())));
        let m = ChatModes { emote_only: true, unique_chat: true, slow_secs: 120, ..Default::default() };
        assert_eq!(gate(&m, &ChatViewer::default()), Some((GateLevel::Info, "Emote-only chat · Slow mode 2 min · Unique chat".into())));
    }

    #[test]
    fn new_accounts_wait_for_the_account_age() {
        let m = ChatModes { account_age_min: Some(43_830), ..Default::default() };
        let young = ChatViewer { account_created: Some(NOW - 3 * 24 * 60 * MIN), ..Default::default() };
        assert_eq!(gate(&m, &young), Some((GateLevel::Blocked, "Accounts must be 1 month old to chat here".into())));
        // old enough, or not known: the age rule says nothing
        assert_eq!(gate(&m, &known(false, None)), None);
        assert_eq!(gate(&m, &ChatViewer::default()), None);
    }

    #[test]
    fn durations_read_like_the_websites() {
        assert_eq!(duration_text(10), "10 min");
        assert_eq!(duration_text(60), "1 h");
        assert_eq!(duration_text(90), "1 h 30 min");
        assert_eq!(duration_text(1440), "1 day");
        assert_eq!(duration_text(5760), "4 days");
        assert_eq!(duration_text(10_080 + 120), "7 days 2 h");
        assert_eq!(duration_text(43_830), "1 month");
        assert_eq!(duration_text(129_600), "3 months");
    }

    #[test]
    fn the_wire_shape_is_camel_case_and_tolerant() {
        let m: ChatModes = serde_json::from_value(serde_json::json!({"followersMin": 60, "slowSecs": 5, "somethingNew": 1})).unwrap();
        assert_eq!(m, ChatModes { followers_min: Some(60), slow_secs: 5, ..Default::default() });
        assert_eq!(serde_json::to_value(&m).unwrap()["followersMin"], 60);
    }
}
