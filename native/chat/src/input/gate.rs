//! Who the account is in a Twitch channel, as far as the app can tell, for the chat modes that keep people out.
use agu_chat_core::chat_rules::ChatViewer;

/// The Twitch account in a channel as far as the app can tell: the broadcaster and mods (mod rights), VIPs and
/// staff (the badges on its own latest line there) are exempt; those badges also show a subscription. Twitch says
/// nothing about follows here, so followers-only never locks the box (Twitch refuses the message if it applies).
pub fn twitch_viewer(broadcaster: bool, is_mod: bool, own_badges: Option<&str>) -> ChatViewer {
    let names = own_badges.map(crate::mod_caps::badge_names).unwrap_or_default();
    let has = |b: &str| names.contains(b);
    ChatViewer {
        exempt: broadcaster || is_mod || ["broadcaster", "moderator", "vip", "staff", "admin", "global_mod"].iter().any(|b| has(b)),
        subscribed: (has("subscriber") || has("founder")).then_some(true),
        ..Default::default()
    }
}
