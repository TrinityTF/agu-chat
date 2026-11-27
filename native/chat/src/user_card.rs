//! The user card's data and wording without any UI: badges, subscription, dates, durations, Twitch's "flagged"
//! line, profile links and Helix's / kick.com's account answers. The desktop's card window and the phone's card
//! sheet both draw from this.
use agu_chat_core::protocol::Platform;
use serde_json::Value;

use super::types::SuspiciousUser;

/// One badge for the card's grid: a picture when the platform gives one, else a glyph chip.
#[derive(Clone, Debug)]
pub struct CardBadge {
    pub url: Option<String>,
    pub title: String,
    pub glyph: String,
    pub color: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SubInfo {
    pub tier: u8,
    pub months: Option<u32>,
}

/// Twitch's `subscriber/<version>` badge: versions from 3000 are tier 3, from 2000 tier 2, the rest tier 1.
/// The months come from `badge-info`, which names the same set ("subscriber/14"; founders: "founder/14").
pub fn sub_info(badges: &str, badge_info: &str) -> Option<SubInfo> {
    let version = |set: &str| badges.split(',').find_map(|b| b.strip_prefix(set).and_then(|r| r.strip_prefix('/')).map(|v| v.to_string()));
    let sub = version("subscriber");
    if sub.is_none() && version("founder").is_none() {
        return None;
    }
    let tier = match sub.and_then(|v| v.parse::<u32>().ok()) {
        Some(v) if v >= 3000 => 3,
        Some(v) if v >= 2000 => 2,
        _ => 1,
    };
    let months = badge_info.split(',').find_map(|b| {
        let (set, n) = b.split_once('/')?;
        if set == "subscriber" || set == "founder" { n.parse::<u32>().ok() } else { None }
    });
    Some(SubInfo { tier, months })
}

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 100, n % 10) {
        (11..=13, _) => "th",
        (_, 1) => "st",
        (_, 2) => "nd",
        (_, 3) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// "April 30th, 2016" from an ISO timestamp, in the local time zone.
pub fn long_date(iso: &str) -> String {
    use chrono::Datelike;
    chrono::DateTime::parse_from_rfc3339(iso)
        .map(|d| {
            let d = d.with_timezone(&chrono::Local);
            format!("{} {}, {}", MONTHS[d.month0() as usize], ordinal(d.day()), d.year())
        })
        .unwrap_or_else(|_| "an unknown date".into())
}

/// 90 -> "1m 30s", 600 -> "10m", 86400 -> "1d": the largest unit plus the next one down.
pub fn fmt_secs(total: u64) -> String {
    const UNITS: [(&str, u64); 5] = [("w", 604_800), ("d", 86_400), ("h", 3600), ("m", 60), ("s", 1)];
    for (i, (label, size)) in UNITS.iter().enumerate() {
        if total < *size {
            continue;
        }
        let rest = total % size;
        return match UNITS.get(i + 1) {
            Some((nl, ns)) if rest >= *ns => format!("{}{label} {}{nl}", total / size, rest / ns),
            _ => format!("{}{label}", total / size),
        };
    }
    "0s".into()
}

pub fn fmt_stamp(ms: i64) -> String {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().map(|d| d.format("%b %-d, %-I:%M %p").to_string()).unwrap_or_default()
}

/// `suspiciousDetails`: spell out why Twitch flagged this user, for the display-only info line.
pub fn suspicious_details(u: &SuspiciousUser) -> String {
    let mut parts: Vec<String> = Vec::new();
    if u.types.iter().any(|t| t == "ban_evader") {
        parts.push(match u.evaluation.as_deref() {
            Some("likely") => "likely ban evader",
            Some("possible") => "possible ban evader",
            _ => "ban evader",
        }
        .to_string());
    }
    if u.types.iter().any(|t| t == "manually_added") {
        parts.push("manually added by a moderator".into());
    }
    if u.types.iter().any(|t| t == "banned_in_shared_channel") {
        let n = u.shared_ban_channels as i64;
        parts.push(format!("banned in {n} shared channel{}", if n == 1 { "" } else { "s" }));
    }
    parts.join(", ")
}

pub fn profile_url(p: Platform, login: &str) -> Option<String> {
    match p {
        Platform::Twitch => Some(format!("https://twitch.tv/{login}")),
        Platform::Kick => Some(format!("https://kick.com/{login}")),
        Platform::Rumble => Some(format!("https://rumble.com/user/{login}")),
        Platform::X => Some(format!("https://x.com/{login}")),
        _ => None,
    }
}

/// `toLocaleDateString()` / `toLocaleTimeString()` of an ISO timestamp (en-US shapes).
pub fn local_date(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.with_timezone(&chrono::Local).format("%-m/%-d/%Y").to_string()).unwrap_or_else(|_| "Invalid Date".into())
}
pub fn local_time(iso: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.with_timezone(&chrono::Local).format("%-I:%M:%S %p").to_string()).unwrap_or_else(|_| "Invalid Date".into())
}

#[derive(Clone, Debug, Default)]
pub struct ModUser {
    pub login: String,
    pub display_name: String,
    pub avatar: String,
    pub created_at: String,
    pub followed_at: Option<String>,
    pub follow_error: bool,
    pub ban: Option<(bool, Option<String>, String)>,
    pub ban_error: Option<String>,
}

pub fn parse_mod_user(v: &Value) -> ModUser {
    let s = |p: &str| v.pointer(p).and_then(|x| x.as_str()).unwrap_or("").to_string();
    ModUser {
        login: s("/user/login"),
        display_name: s("/user/displayName"),
        avatar: s("/user/avatar"),
        created_at: s("/user/createdAt"),
        followed_at: v.get("followedAt").and_then(|x| x.as_str()).map(|x| x.to_string()),
        follow_error: v.get("followError").map(|x| !x.is_null() && x != &Value::Bool(false) && x != &Value::from("")).unwrap_or(false),
        ban: v.get("ban").filter(|b| b.is_object()).map(|b| (b.get("permanent").and_then(|x| x.as_bool()).unwrap_or(false), b.get("expiresAt").and_then(|x| x.as_str()).map(|x| x.to_string()), b.get("reason").and_then(|x| x.as_str()).unwrap_or("").to_string())),
        ban_error: v.get("banError").and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(|x| x.to_string()),
    }
}

/// Kick's own stand-in picture for an account without one (kick.com serves default-avatar-1..6). Which of the six
/// kick.com picks is not public; the user id keeps it stable per user here.
pub fn kick_default_avatar(user_id: &str) -> String {
    let n = user_id.parse::<u64>().map(|id| id % 6 + 1).unwrap_or(1);
    format!("https://kick.com/img/default-profile-pictures/default-avatar-{n}.webp")
}

