//! The user card's twitch.tv data: Twitch's own viewer card (badges, follow, subscription, ban status, warnings,
//! timeouts, bans, shared mod comments, the logged messages), read through core's `/api/twitch/card*` routes
//! (`agu_chat_core::twitch::web`). This file is the model, the parsing of those answers and the rules for which
//! tabs the card shows; `user_card.rs` draws it.
use agu_chat_core::protocol::Platform;
use serde_json::Value;

use super::user_card::CardBadge;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Messages,
    Warnings,
    Timeouts,
    Bans,
    Comments,
}

impl Tab {
    pub fn label(self) -> &'static str {
        match self {
            Tab::Messages => "Messages",
            Tab::Warnings => "Warnings",
            Tab::Timeouts => "Timeouts",
            Tab::Bans => "Bans",
            Tab::Comments => "Comments",
        }
    }
}

/// What the card knows about the twitch.tv sign-in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum WebStatus {
    /// not asked yet
    #[default]
    Unknown,
    SignedOut,
    Expired,
    SignedIn,
}

impl WebStatus {
    /// from the status + `code` of a `/api/twitch/card` answer
    pub fn from_answer(status: u16, body: &Value) -> WebStatus {
        match (status, body.get("code").and_then(|c| c.as_str())) {
            (200..=299, _) => WebStatus::SignedIn,
            (401, Some("expired")) => WebStatus::Expired,
            (401, _) => WebStatus::SignedOut,
            _ => WebStatus::Unknown,
        }
    }
}

/// Which tabs the card shows. Twitch with the mod logs: all five (Twitch's own); everything else only has the
/// person's lines in the pane, so only Messages.
pub fn tabs_for(platform: Platform, whisper: bool, has_logs: bool) -> Vec<Tab> {
    if platform == Platform::Twitch && !whisper && has_logs {
        vec![Tab::Messages, Tab::Warnings, Tab::Timeouts, Tab::Bans, Tab::Comments]
    } else {
        vec![Tab::Messages]
    }
}

/// The muted "Log in to Twitch again" line: a Twitch channel (not a whisper) where the user moderates and has no
/// working twitch.tv sign-in.
pub fn show_signin_prompt(platform: Platform, whisper: bool, is_mod_here: bool, web: WebStatus) -> bool {
    platform == Platform::Twitch && !whisper && is_mod_here && matches!(web, WebStatus::SignedOut | WebStatus::Expired)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebSub {
    pub tier: u8,
    pub months: Option<u32>,
    pub gift: bool,
    pub prime: bool,
}

/// "Tier 2 - Subscribed for 14 months (gift)"
pub fn sub_line(tier: u8, months: Option<u32>, gift: bool, prime: bool) -> String {
    let months = match months {
        Some(1) => " for 1 month".to_string(),
        Some(n) if n > 1 => format!(" for {n} months"),
        _ => String::new(),
    };
    let how = if prime { " (Prime)" } else if gift { " (gift)" } else { "" };
    format!("Tier {tier} - Subscribed{months}{how}")
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebBan {
    pub permanent: bool,
    pub expires_at: Option<String>,
    pub reason: String,
    pub by: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionItem {
    pub ts: String,
    pub text: String,
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Section {
    /// "3", "99+"
    pub label: String,
    pub items: Vec<ActionItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Comment {
    pub id: String,
    pub ts: String,
    pub text: String,
    pub by: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Logs {
    /// "1000+" when Twitch has more than it counted
    pub messages_label: String,
    pub warnings: Section,
    pub timeouts: Section,
    pub untimeouts: Section,
    pub bans: Section,
    pub unbans: Section,
    pub comments: Vec<Comment>,
}

impl Logs {
    /// The tab's count label.
    pub fn label(&self, tab: Tab) -> String {
        match tab {
            Tab::Messages => self.messages_label.clone(),
            Tab::Warnings => self.warnings.label.clone(),
            Tab::Timeouts => self.timeouts.label.clone(),
            Tab::Bans => self.bans.label.clone(),
            Tab::Comments => self.comments.len().to_string(),
        }
    }

    /// The lines of a history tab: what was done and what lifted it, newest first.
    pub fn history(&self, tab: Tab) -> Vec<&ActionItem> {
        let (a, b): (&Section, Option<&Section>) = match tab {
            Tab::Warnings => (&self.warnings, None),
            Tab::Timeouts => (&self.timeouts, Some(&self.untimeouts)),
            Tab::Bans => (&self.bans, Some(&self.unbans)),
            _ => return vec![],
        };
        let mut v: Vec<&ActionItem> = a.items.iter().chain(b.into_iter().flat_map(|s| s.items.iter())).collect();
        v.sort_by(|x, y| ts_ms(&y.ts).cmp(&ts_ms(&x.ts)));
        v
    }
}

#[derive(Clone, Debug, Default)]
pub struct WebCard {
    pub user_id: String,
    pub login: String,
    pub display: String,
    pub avatar: String,
    pub created_at: String,
    pub followed_at: Option<String>,
    pub sub: Option<WebSub>,
    pub badges: Vec<CardBadge>,
    pub can_access_logs: bool,
    pub ban: Option<WebBan>,
    /// None: no mod-log access (or not loaded)
    pub logs: Option<Logs>,
    /// why the logs are missing though the user may read them (the integrity check, a failed call)
    pub logs_error: Option<String>,
    /// the twitch.tv account is the Twitch account the app is logged in as
    pub same_user: bool,
    /// the twitch.tv sign-in's login (comments by it are the user's own)
    pub me_login: String,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string()
}

fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(String::from)
}

fn section(v: &Value) -> Section {
    Section {
        label: v.get("label").and_then(|l| l.as_str()).unwrap_or("0").to_string(),
        items: v.get("items").and_then(|i| i.as_array()).map(|a| a.iter().map(|i| ActionItem { ts: s(i, "ts"), text: s(i, "text"), kind: s(i, "type") }).collect()).unwrap_or_default(),
    }
}

pub fn parse_comment(c: &Value) -> Comment {
    Comment { id: s(c, "id"), ts: s(c, "ts"), text: s(c, "text"), by: s(c, "by") }
}

/// The `/api/twitch/card` answer.
pub fn parse_card(v: &Value) -> WebCard {
    let u = v.get("user").cloned().unwrap_or(Value::Null);
    let badges = v
        .get("badges")
        .and_then(|b| b.as_array())
        .map(|a| {
            a.iter()
                .map(|b| {
                    let title = s(b, "title");
                    let desc = s(b, "description");
                    // the badge's name only; Twitch's description is the fallback for a nameless one
                    CardBadge { url: opt(b, "url"), title: if title.is_empty() { desc } else { title }, glyph: String::new(), color: 0 }
                })
                .collect()
        })
        .unwrap_or_default();
    let sub = v.get("sub").filter(|x| x.is_object()).map(|x| WebSub {
        tier: x.get("tier").and_then(|t| t.as_u64()).unwrap_or(1) as u8,
        months: x.get("months").and_then(|m| m.as_u64()).map(|m| m as u32),
        gift: x.get("gift").and_then(|g| g.as_bool()).unwrap_or(false),
        prime: x.get("prime").and_then(|g| g.as_bool()).unwrap_or(false),
    });
    let ban = v.get("ban").filter(|b| b.is_object()).map(|b| WebBan { permanent: b.get("permanent").and_then(|p| p.as_bool()).unwrap_or(false), expires_at: opt(b, "expiresAt"), reason: s(b, "reason"), by: s(b, "by") });
    let logs = v.get("logs").filter(|l| l.is_object()).map(|l| Logs {
        messages_label: l.pointer("/messages/label").and_then(|x| x.as_str()).unwrap_or("0").to_string(),
        warnings: section(&l["warnings"]),
        timeouts: section(&l["timeouts"]),
        untimeouts: section(&l["untimeouts"]),
        bans: section(&l["bans"]),
        unbans: section(&l["unbans"]),
        comments: l.get("comments").and_then(|c| c.as_array()).map(|a| a.iter().map(parse_comment).collect()).unwrap_or_default(),
    });
    WebCard {
        user_id: s(&u, "id"),
        login: s(&u, "login"),
        display: s(&u, "displayName"),
        avatar: s(&u, "avatar"),
        created_at: s(&u, "createdAt"),
        followed_at: opt(v, "followedAt"),
        sub,
        badges,
        can_access_logs: v.get("canAccessModLogs").and_then(|x| x.as_bool()).unwrap_or(false),
        ban,
        logs,
        logs_error: v.pointer("/logsError/message").and_then(|m| m.as_str()).map(String::from),
        same_user: v.get("sameUser").and_then(|x| x.as_bool()).unwrap_or(true),
        me_login: v.pointer("/me/login").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MsgItem {
    pub kind: String,
    pub ts: String,
    pub text: String,
    pub status: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MsgPage {
    pub items: Vec<MsgItem>,
    pub more: bool,
    pub cursor: Option<String>,
}

/// The `/api/twitch/card/messages` answer.
pub fn parse_messages(v: &Value) -> MsgPage {
    MsgPage {
        items: v.get("items").and_then(|i| i.as_array()).map(|a| a.iter().map(|m| MsgItem { kind: s(m, "kind"), ts: s(m, "ts"), text: s(m, "text"), status: s(m, "status") }).collect()).unwrap_or_default(),
        more: v.get("more").and_then(|m| m.as_bool()).unwrap_or(false),
        cursor: opt(v, "cursor"),
    }
}

/// Milliseconds of an ISO timestamp (0 when it does not parse), for ordering and display.
pub fn ts_ms(iso: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(iso).map(|d| d.timestamp_millis()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answer() -> Value {
        json!({
            "user": {"id": "42", "login": "cooldude", "displayName": "CoolDude", "avatar": "https://p", "createdAt": "2016-04-30T10:00:00Z"},
            "followedAt": "2020-01-02T00:00:00Z",
            "sub": {"tier": 3, "months": 1, "gift": false, "prime": true},
            "isModerator": false,
            "badges": [{"title": "Subscriber", "description": "Subscriber for 14 months", "url": "https://b/2"}, {"title": "Moderator", "description": "Moderator", "url": ""}],
            "canAccessModLogs": true,
            "ban": {"permanent": false, "expiresAt": "2026-11-02T00:00:00Z", "reason": "spam", "by": "Mod"},
            "logs": {
                "messages": {"label": "1000+"},
                "warnings": {"label": "1", "items": [{"ts": "2026-10-01T00:00:00Z", "text": "Mod warned CoolDude", "type": "WARN"}]},
                "timeouts": {"label": "2", "items": [{"ts": "2026-10-03T00:00:00Z", "text": "Mod timed out CoolDude for 10m", "type": "TIMEOUT"}]},
                "untimeouts": {"label": "1", "items": [{"ts": "2026-10-04T00:00:00Z", "text": "Mod removed timeout on CoolDude", "type": "UNTIMEOUT"}]},
                "bans": {"label": "0", "items": []}, "unbans": {"label": "0", "items": []},
                "comments": [{"id": "c1", "ts": "2026-10-02T00:00:00Z", "text": "careful", "by": "m"}]
            },
            "sameUser": false, "me": {"login": "agu"}
        })
    }

    #[test]
    fn card_answer_parses_into_the_model() {
        let c = parse_card(&answer());
        assert_eq!((c.login.as_str(), c.display.as_str()), ("cooldude", "CoolDude"));
        assert_eq!(c.followed_at.as_deref(), Some("2020-01-02T00:00:00Z"));
        assert_eq!(c.sub, Some(WebSub { tier: 3, months: Some(1), gift: false, prime: true }));
        assert_eq!(sub_line(3, Some(1), false, true), "Tier 3 - Subscribed for 1 month (Prime)");
        assert_eq!(sub_line(1, Some(14), true, false), "Tier 1 - Subscribed for 14 months (gift)");
        assert_eq!(sub_line(2, None, false, false), "Tier 2 - Subscribed");
        assert_eq!(c.badges.len(), 2);
        assert_eq!(c.badges[0].title, "Subscriber");
        assert_eq!(c.badges[1].title, "Moderator");
        assert!(c.badges[1].url.is_none());
        assert_eq!(c.ban, Some(WebBan { permanent: false, expires_at: Some("2026-11-02T00:00:00Z".into()), reason: "spam".into(), by: "Mod".into() }));
        assert!(!c.same_user);
        assert_eq!(c.me_login, "agu");
        let l = c.logs.unwrap();
        assert_eq!(l.label(Tab::Messages), "1000+");
        assert_eq!(l.label(Tab::Comments), "1");
        assert_eq!(l.comments[0].by, "m");
    }

    #[test]
    fn history_tabs_merge_lifted_actions_newest_first() {
        let l = parse_card(&answer()).logs.unwrap();
        let t = l.history(Tab::Timeouts);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].kind, "UNTIMEOUT");
        assert_eq!(t[1].kind, "TIMEOUT");
        assert_eq!(l.history(Tab::Warnings).len(), 1);
        assert!(l.history(Tab::Bans).is_empty());
        assert!(l.history(Tab::Messages).is_empty());
    }

    #[test]
    fn card_without_logs_or_ban() {
        let c = parse_card(&json!({"user": {"id": "1", "login": "a"}, "canAccessModLogs": false, "logsError": {"code": "integrity", "message": "no"}}));
        assert!(c.logs.is_none() && c.ban.is_none() && c.sub.is_none());
        assert_eq!(c.logs_error.as_deref(), Some("no"));
        assert!(c.same_user);
    }

    #[test]
    fn messages_page() {
        let p = parse_messages(&json!({"items": [{"kind": "chat", "ts": "2026-10-01T00:00:00Z", "text": "hi"}, {"kind": "caught", "text": "x", "status": "DENIED"}], "more": true, "cursor": "abc"}));
        assert_eq!(p.items.len(), 2);
        assert_eq!(p.items[1].status, "DENIED");
        assert!(p.more);
        assert_eq!(p.cursor.as_deref(), Some("abc"));
        assert_eq!(parse_messages(&json!({"items": [], "cursor": null})), MsgPage::default());
    }

    #[test]
    fn tabs_per_platform_and_state() {
        use Platform::*;
        let all = vec![Tab::Messages, Tab::Warnings, Tab::Timeouts, Tab::Bans, Tab::Comments];
        assert_eq!(tabs_for(Twitch, false, true), all);
        assert_eq!(tabs_for(Twitch, false, false), vec![Tab::Messages]);
        assert_eq!(tabs_for(Twitch, true, true), vec![Tab::Messages], "a whisper has no mod history");
        for p in [Kick, Youtube, Rumble, X] {
            assert_eq!(tabs_for(p, false, true), vec![Tab::Messages]);
        }
    }

    #[test]
    fn sign_in_prompt_only_for_a_mod_without_a_session() {
        use Platform::*;
        assert!(show_signin_prompt(Twitch, false, true, WebStatus::SignedOut));
        assert!(show_signin_prompt(Twitch, false, true, WebStatus::Expired));
        assert!(!show_signin_prompt(Twitch, false, false, WebStatus::SignedOut), "not a mod there");
        assert!(!show_signin_prompt(Twitch, false, true, WebStatus::SignedIn));
        assert!(!show_signin_prompt(Twitch, false, true, WebStatus::Unknown));
        assert!(!show_signin_prompt(Twitch, true, true, WebStatus::SignedOut), "whisper");
        assert!(!show_signin_prompt(Kick, false, true, WebStatus::SignedOut));
    }

    #[test]
    fn web_status_from_answer() {
        assert_eq!(WebStatus::from_answer(200, &json!({})), WebStatus::SignedIn);
        assert_eq!(WebStatus::from_answer(401, &json!({"code": "signed-out"})), WebStatus::SignedOut);
        assert_eq!(WebStatus::from_answer(401, &json!({"code": "expired"})), WebStatus::Expired);
        assert_eq!(WebStatus::from_answer(502, &json!({})), WebStatus::Unknown);
    }
}
