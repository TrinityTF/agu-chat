//! Port of src/renderer/chat/twitch/events.ts: which USERNOTICEs show, under the site's default event toggles.
use std::collections::HashSet;

pub struct ChatEventSettings {
    pub redeems: bool,
    pub cheers: bool,
    pub subs: bool,
    pub gifts: bool,
    pub raids: bool,
    pub streaks: bool,
    pub announcements: bool,
    pub bits_badges: bool,
    pub charity: bool,
    pub combos: bool,
    pub shared_rewards: bool,
    pub group_gifts: bool,
    /// Kick: KICKs gifts (Kick raids follow `raids`)
    pub kicks: bool,
}

pub const DEFAULT_EVENTS: ChatEventSettings = ChatEventSettings {
    redeems: true,
    cheers: true,
    subs: true,
    gifts: true,
    raids: true,
    streaks: true,
    announcements: false,
    bits_badges: false,
    charity: false,
    combos: false,
    shared_rewards: false,
    group_gifts: false, // like 7TV: every recipient of a gift bomb gets its own line
    kicks: true,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChatEventKind {
    Redeems,
    Cheers,
    Subs,
    Gifts,
    Raids,
    Streaks,
    Announcements,
    BitsBadges,
    Charity,
    Combos,
    SharedRewards,
}

pub fn notice_kind_of(id: &str) -> Option<ChatEventKind> {
    use ChatEventKind::*;
    Some(match id {
        "redeem" => Redeems,
        "powerup" => Cheers,
        "sub" | "resub" | "extendsub" | "primepaidupgrade" | "giftpaidupgrade" | "anongiftpaidupgrade" | "standardpayforward" | "communitypayforward" => Subs,
        "subgift" | "anonsubgift" | "submysterygift" | "anonsubmysterygift" => Gifts,
        "raid" | "unraid" => Raids,
        "announcement" => Announcements,
        "viewermilestone" => Streaks,
        "bitsbadgetier" => BitsBadges,
        "charitydonation" => Charity,
        "onetapgiftredeemed" => Combos,
        "rewardgift" => SharedRewards,
        _ => return None,
    })
}

/// The id a notice is classified by: its source msg-id when it has one.
pub fn notice_id<'a>(msg_id: &'a str, source_msg_id: Option<&'a str>) -> &'a str {
    source_msg_id.filter(|s| !s.is_empty()).unwrap_or(msg_id)
}

pub fn notice_kind(msg_id: &str, source_msg_id: Option<&str>) -> Option<ChatEventKind> {
    notice_kind_of(notice_id(msg_id, source_msg_id))
}

pub fn kind_on(ev: &ChatEventSettings, k: Option<ChatEventKind>) -> bool {
    use ChatEventKind::*;
    match k {
        None => false,
        Some(Redeems) => ev.redeems,
        Some(Cheers) => ev.cheers,
        Some(Subs) => ev.subs,
        Some(Gifts) => ev.gifts,
        Some(Raids) => ev.raids,
        Some(Streaks) => ev.streaks,
        Some(Announcements) => ev.announcements,
        Some(BitsBadges) => ev.bits_badges,
        Some(Charity) => ev.charity,
        Some(Combos) => ev.combos,
        Some(SharedRewards) => ev.shared_rewards,
    }
}

/// Twitch's own redeems, which arrive as a msg-id on an ordinary message. (title, is_bits)
pub fn builtin_redeem(msg_id: &str) -> Option<(&'static str, bool)> {
    match msg_id {
        "highlighted-message" => Some(("Highlight My Message", false)),
        "gigantified-emote-message" => Some(("Gigantify an Emote", true)),
        "animated-message" => Some(("Message Effects", true)),
        _ => None,
    }
}

/// Whether a Twitch notice should be on screen under the event toggles. `bombs` is the set of gift-bomb ids
/// already showing their summary line: their per-recipient lines fold under it.
pub fn notice_visible(msg_id: &str, source_msg_id: Option<&str>, gift_id: Option<&str>, bombs: &HashSet<String>) -> bool {
    let ev = &DEFAULT_EVENTS;
    if !kind_on(ev, notice_kind(msg_id, source_msg_id)) {
        return false;
    }
    let id = notice_id(msg_id, source_msg_id);
    if ev.group_gifts && (id == "subgift" || id == "anonsubgift") {
        if let Some(g) = gift_id.filter(|g| !g.is_empty()) {
            if bombs.contains(g) {
                return false;
            }
        }
    }
    true
}

/// Whether a Kick line shows under the event toggles: a KICKs gift follows `kicks`, a raid `raids`, chat always shows.
pub fn kick_event_visible(notice: Option<&str>) -> bool {
    kick_event_on(&DEFAULT_EVENTS, notice)
}

fn kick_event_on(ev: &ChatEventSettings, notice: Option<&str>) -> bool {
    match notice {
        Some("kicks") => ev.kicks,
        Some("host") => ev.raids,
        _ => true,
    }
}

#[cfg(test)]
mod kick_tests {
    use super::*;

    #[test]
    fn kick_events_follow_their_toggles_and_show_by_default() {
        assert!(kick_event_visible(Some("kicks")));
        assert!(kick_event_visible(Some("host")));
        assert!(kick_event_visible(None));
        assert!(kick_event_visible(Some("celebration")));
        let off = ChatEventSettings { kicks: false, raids: false, ..DEFAULT_EVENTS };
        assert!(!kick_event_on(&off, Some("kicks")));
        assert!(!kick_event_on(&off, Some("host")));
        assert!(kick_event_on(&off, None));
    }
}
