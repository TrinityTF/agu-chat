//! The user card as JSON for the phone: what the desktop's card window (gpui `user_card.rs`) decides to show for a
//! person, without the drawing. `session.rs` gathers the relay's answers and the person's lines; this turns them
//! into the card's sections: header, badges, info lines, mod tools, tabs, history, comments.
use agu_chat_core::protocol::Platform;
use agu_chat_model::card_web::{show_signin_prompt, sub_line, tabs_for, ts_ms, Logs, MsgPage, Tab, WebCard, WebStatus};
use agu_chat_model::mod_actions::ModTarget;
use agu_chat_model::mod_caps::{ModCaps, ModReach, TIMEOUT_LADDER};
use agu_chat_model::mod_row::timeout_ladder;
use agu_chat_model::types::SuspiciousUser;
use agu_chat_model::user_card::{fmt_stamp, kick_default_avatar, local_time, long_date, profile_url, suspicious_details, ModUser, SubInfo};
use serde_json::{json, Value};

pub struct CardUser {
    pub id: String,
    pub login: String,
    pub name: String,
    pub avatar: String,
}

pub struct CardIn<'a> {
    pub platform: Platform,
    pub whisper: bool,
    pub user: &'a CardUser,
    pub web: Option<&'a WebCard>,
    pub web_status: WebStatus,
    pub web_error: Option<&'a str>,
    pub data: Option<&'a ModUser>,
    pub relogin: bool,
    /// the first lookup is still running
    pub loading: bool,
    pub caps: &'a ModCaps,
    pub reach: ModReach,
    pub suspicious: Option<&'a SuspiciousUser>,
    /// Kick / Rumble: the account moderates this chat
    pub co_can_moderate: bool,
    /// the subscription the person's chat badges show
    pub sub: Option<SubInfo>,
    pub recent_total: usize,
    /// Some(ignored) when the person can be ignored
    pub ignore: Option<bool>,
    /// what Kick / Rumble actions act on: the person's newest line here
    pub target: Option<&'a ModTarget>,
    /// the person's newest line here is a Rumble line
    pub last_is_rumble: bool,
}

fn note(text: impl Into<String>, tone: &str) -> Value {
    json!({"text": text.into(), "tone": tone})
}

fn history(items: Vec<&agu_chat_model::card_web::ActionItem>) -> Value {
    Value::Array(items.into_iter().map(|e| json!({"ts": ts_ms(&e.ts), "stamp": fmt_stamp(ts_ms(&e.ts)), "text": e.text, "kind": e.kind})).collect())
}

fn tab_id(t: Tab) -> &'static str {
    match t {
        Tab::Messages => "messages",
        Tab::Warnings => "warnings",
        Tab::Timeouts => "timeouts",
        Tab::Bans => "bans",
        Tab::Comments => "comments",
    }
}

pub fn card_json(c: &CardIn) -> Value {
    let is_twitch = c.platform == Platform::Twitch;
    let has_account = is_twitch || c.platform == Platform::Kick;
    let login = c.data.map(|d| d.login.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| c.user.login.clone());
    let display = c.data.map(|d| d.display_name.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| c.user.name.clone());
    let mut avatar = c.data.map(|d| d.avatar.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| c.user.avatar.clone());
    if avatar.is_empty() && c.platform == Platform::Kick && !c.loading {
        avatar = kick_default_avatar(&c.user.id);
    }
    let profile = if login.is_empty() { None } else { profile_url(c.platform, &login) };
    let banned = c.data.and_then(|d| d.ban.clone());
    let has_logs = c.web.map(|w| w.logs.is_some()).unwrap_or(false);
    let logs: Option<&Logs> = c.web.and_then(|w| w.logs.as_ref());

    // ---- info lines, chips and notes
    let mut info: Vec<Value> = Vec::new();
    let mut chips: Vec<Value> = Vec::new();
    let mut notes: Vec<Value> = Vec::new();
    if has_account && c.relogin {
        notes.push(note("Log in again to enable mod tools.", "amber"));
    } else if has_account && !c.loading {
        if let Some(d) = c.data {
            if !d.created_at.is_empty() {
                info.push(json!({"icon": "clock", "text": format!("Account created on {}", long_date(&d.created_at))}));
            }
            // "Not following" only when we actually confirmed it: a failed lookup omits the line.
            if d.followed_at.is_some() || !d.follow_error {
                info.push(json!({"icon": "star", "text": match &d.followed_at { Some(f) => format!("Following since {}", long_date(f)), None => "Not following".to_string() }}));
            }
        }
    }
    match (c.web, &c.sub) {
        (Some(w), _) if w.sub.is_some() => {
            let sb = w.sub.as_ref().unwrap();
            info.push(json!({"icon": "tag", "text": sub_line(sb.tier, sb.months, sb.gift, sb.prime)}));
        }
        // Twitch's card leaves the subscription out (its integrity check): the chat badge's months then
        (_, Some(sb)) => info.push(json!({"icon": "tag", "text": sub_line(sb.tier, sb.months, false, false)})),
        _ => {}
    }
    if let Some((permanent, expires, reason)) = &banned {
        chips.push(if *permanent {
            json!({"text": format!("Banned{}", if reason.is_empty() { String::new() } else { format!(": {reason}") }), "tone": "red"})
        } else {
            json!({"text": format!("Timed out until {}", expires.as_deref().map(local_time).unwrap_or_else(|| "?".into())), "tone": "amber"})
        });
    } else if let Some(e) = c.data.and_then(|d| d.ban_error.as_ref()) {
        notes.push(note(format!("Ban status unknown: {e}"), "faint"));
    }
    if let Some(e) = c.web_error {
        notes.push(note(e, "faint"));
    }
    if let Some(w) = c.web {
        if !w.same_user {
            notes.push(note(
                format!("twitch.tv is signed in as {}, not the Twitch account Agu Chat is logged in as.", if w.me_login.is_empty() { "another account".to_string() } else { w.me_login.clone() }),
                "amber",
            ));
        }
        if let Some(e) = &w.logs_error {
            notes.push(note(e, "faint"));
        }
    }
    if has_account && !c.loading {
        if let Some(s) = c.suspicious {
            let det = suspicious_details(s);
            notes.push(note(format!("Twitch flags this user: {}{}", if s.status == "restricted" { "restricted" } else { "monitored" }, if det.is_empty() { String::new() } else { format!(" - {det}") }), "orange"));
        }
    }
    if has_account && !c.loading && !c.relogin && c.data.is_none() && info.is_empty() && chips.is_empty() && notes.is_empty() {
        notes.push(note("No account info.", "faint"));
    }

    // ---- mod tools
    let caps = c.caps;
    let reach = &c.reach;
    let mut mod_json = Value::Null;
    let show_twitch_mod = is_twitch && !c.whisper && (reach.ban || reach.timeout || reach.warn || banned.is_some()) && (caps.ban || caps.warn);
    if show_twitch_mod {
        let ladder = (banned.is_none() && reach.timeout && caps.ban).then(|| TIMEOUT_LADDER.iter().map(|(l, s)| json!([l, s])).collect::<Vec<_>>());
        let can_ban = reach.ban && caps.ban;
        let (unban, ban) = match &banned {
            Some((permanent, _, _)) => (Some(if *permanent { "Unban" } else { "Untimeout" }), !*permanent && can_ban),
            None => (None, can_ban),
        };
        mod_json = json!({"kind": "twitch", "ladder": ladder, "warn": reach.warn && caps.warn, "ban": ban, "unban": unban});
    } else if c.platform == Platform::Rumble && c.co_can_moderate && c.reach.ban && c.last_is_rumble && c.target.is_some() {
        mod_json = json!({"kind": "rumble", "timeoutSeconds": 300});
    } else if c.platform == Platform::Kick && c.co_can_moderate && c.reach.ban && !c.user.id.is_empty() && c.target.is_some() {
        // banned: Some(true), timed out: Some(false), free (or unknown): None
        let state = c.data.and_then(|d| d.ban.as_ref().map(|b| b.0));
        let ladder = (state != Some(true)).then(|| timeout_ladder(Platform::Kick).iter().map(|(l, s)| json!([l, s])).collect::<Vec<_>>());
        let unban = state.map(|p| if p { "Unban" } else { "Untimeout" });
        mod_json = json!({"kind": "kick", "ladder": ladder, "ban": state.is_none(), "unban": unban});
    }

    // ---- tabs
    let tabs: Vec<Value> = tabs_for(c.platform, c.whisper, has_logs)
        .into_iter()
        .map(|t| {
            let count = match logs {
                Some(l) => l.label(t),
                None => c.recent_total.to_string(),
            };
            json!({"id": tab_id(t), "label": t.label(), "count": count})
        })
        .collect();
    let signin = show_signin_prompt(c.platform, c.whisper, caps.is_mod == Some(true) || caps.is_broadcaster == Some(true), c.web_status);
    let hist = |t: Tab| history(logs.map(|l| l.history(t)).unwrap_or_default());
    let mut comments: Vec<Value> = Vec::new();
    if let Some(l) = logs {
        let me = c.web.map(|w| w.me_login.clone()).unwrap_or_default();
        let mut list = l.comments.clone();
        list.sort_by(|x, y| ts_ms(&y.ts).cmp(&ts_ms(&x.ts)));
        comments = list.iter().map(|cm| json!({"id": cm.id, "text": cm.text, "by": cm.by, "stamp": fmt_stamp(ts_ms(&cm.ts)), "mine": !me.is_empty() && cm.by.eq_ignore_ascii_case(&me)})).collect();
    }
    let web_badges: Vec<Value> = c.web.map(|w| w.badges.iter().map(|b| json!({"url": b.url, "title": b.title})).collect()).unwrap_or_default();

    json!({
        "platform": c.platform.as_str(),
        "id": c.user.id,
        "login": login,
        "name": if display.is_empty() { "unknown".to_string() } else { display },
        "avatar": avatar,
        "profile": profile,
        "loading": c.loading && has_account,
        "badges": web_badges,
        "info": info,
        "chips": chips,
        "notes": notes,
        "mod": mod_json,
        "target": c.target.map(|t| json!({"id": t.id, "uid": t.user_id, "login": t.login, "name": t.name, "menu": t.menu})),
        "tabs": tabs,
        "signin": signin,
        "messagesWeb": has_logs,
        "warnings": hist(Tab::Warnings),
        "timeouts": hist(Tab::Timeouts),
        "bans": hist(Tab::Bans),
        "comments": comments,
        "commentTarget": c.web.map(|w| w.user_id.clone()).filter(|i| !i.is_empty()).unwrap_or_else(|| c.user.id.clone()),
        "ignore": c.ignore,
        // the trash button beside each of their own lines in this chat (Twitch)
        "canDelete": is_twitch && !c.whisper && caps.delete && reach.delete,
    })
}

/// One page of Twitch's log of the person's lines: each as a row the list can draw (`kind` "msg", text only).
pub fn messages_json(page: &MsgPage, parts: &dyn Fn(&str, &str) -> Value) -> Value {
    let items: Vec<Value> = page
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| {
            let caught = it.kind == "caught";
            let note = if caught { Some(format!("(held by AutoMod{})", if it.status.is_empty() { String::new() } else { format!(", {}", it.status.to_lowercase()) })) } else { None };
            json!({"key": format!("{}-{i}", it.ts), "stamp": fmt_stamp(ts_ms(&it.ts)), "kind": it.kind, "note": note, "parts": parts(&it.text, if it.kind == "action" { "" } else { &it.emotes })})
        })
        .collect();
    json!({"items": items, "more": page.more, "cursor": page.cursor})
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_model::card_web::parse_card;

    fn user() -> CardUser {
        CardUser { id: "42".into(), login: "cooldude".into(), name: "CoolDude".into(), avatar: String::new() }
    }

    fn web() -> WebCard {
        parse_card(&json!({
            "user": {"id": "42", "login": "cooldude", "displayName": "CoolDude", "avatar": "https://p", "createdAt": "2016-04-30T10:00:00Z"},
            "followedAt": "2020-01-02T00:00:00Z",
            "sub": {"tier": 1, "months": 14, "gift": false, "prime": false},
            "badges": [{"title": "Subscriber", "url": "https://b/2"}],
            "canAccessModLogs": true,
            "logs": {
                "messages": {"label": "12"},
                "warnings": {"label": "1", "items": [{"ts": "2026-10-01T00:00:00Z", "text": "Mod warned CoolDude", "type": "WARN"}]},
                "timeouts": {"label": "1", "items": [{"ts": "2026-10-03T00:00:00Z", "text": "Mod timed out CoolDude for 10m", "type": "TIMEOUT"}]},
                "untimeouts": {"label": "0", "items": []}, "bans": {"label": "0", "items": []}, "unbans": {"label": "0", "items": []},
                "comments": [{"id": "c1", "ts": "2026-10-02T00:00:00Z", "text": "careful", "by": "me"}, {"id": "c2", "ts": "2026-10-04T00:00:00Z", "text": "ok", "by": "other"}]
            },
            "sameUser": true, "me": {"login": "Me"}
        }))
    }

    fn mod_caps() -> ModCaps {
        ModCaps { ban: true, warn: true, delete: true, is_mod: Some(true), ..Default::default() }
    }

    fn input<'a>(u: &'a CardUser, w: Option<&'a WebCard>, caps: &'a ModCaps, reach: ModReach) -> CardIn<'a> {
        CardIn { platform: Platform::Twitch, whisper: false, user: u, web: w, web_status: WebStatus::SignedIn, web_error: None, data: None, relogin: false, loading: false, caps, reach, suspicious: None, co_can_moderate: false, sub: None, recent_total: 3, ignore: Some(false), target: None, last_is_rumble: false }
    }

    #[test]
    fn a_twitch_card_with_mod_logs_has_every_tab_and_the_mod_tools() {
        let (u, w, caps) = (user(), web(), mod_caps());
        let v = card_json(&input(&u, Some(&w), &caps, agu_chat_model::mod_caps::REACH_ALL));
        assert_eq!(v["name"], "CoolDude");
        assert_eq!(v["profile"], "https://twitch.tv/cooldude");
        let tabs: Vec<&str> = v["tabs"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(tabs, vec!["messages", "warnings", "timeouts", "bans", "comments"]);
        assert_eq!(v["tabs"][0]["count"], "12");
        assert_eq!(v["messagesWeb"], true);
        assert_eq!(v["info"][0]["text"], "Tier 1 - Subscribed for 14 months");
        assert_eq!(v["badges"][0]["title"], "Subscriber");
        assert_eq!(v["mod"]["kind"], "twitch");
        assert_eq!(v["mod"]["ladder"].as_array().unwrap().len(), 9);
        assert_eq!(v["mod"]["ladder"][1], json!(["5m", 300]));
        assert_eq!((v["mod"]["warn"].clone(), v["mod"]["ban"].clone(), v["mod"]["unban"].clone()), (json!(true), json!(true), Value::Null));
        assert_eq!(v["warnings"][0]["text"], "Mod warned CoolDude");
        // comments newest first, and only your own are yours to delete
        assert_eq!(v["comments"][0]["id"], "c2");
        assert_eq!((v["comments"][0]["mine"].clone(), v["comments"][1]["mine"].clone()), (json!(false), json!(true)));
        assert_eq!(v["ignore"], false);
    }

    #[test]
    fn without_mod_rights_there_are_no_mod_tools() {
        let (u, caps) = (user(), ModCaps::default());
        let v = card_json(&input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL));
        assert!(v["mod"].is_null());
        assert_eq!(v["tabs"].as_array().unwrap().len(), 1, "no mod logs: only the lines in this chat");
        assert_eq!(v["tabs"][0]["count"], "3");
        assert_eq!(v["messagesWeb"], false);
    }

    #[test]
    fn a_banned_user_offers_unban_and_a_timed_out_one_also_ban() {
        let (u, caps) = (user(), mod_caps());
        let mut data = ModUser { login: "cooldude".into(), display_name: "CoolDude".into(), ban: Some((true, None, "spam".into())), ..Default::default() };
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.data = Some(&data);
        let v = card_json(&c);
        assert_eq!(v["chips"][0]["text"], "Banned: spam");
        assert_eq!(v["mod"]["unban"], "Unban");
        assert_eq!(v["mod"]["ban"], false);
        assert!(v["mod"]["ladder"].is_null());
        data.ban = Some((false, Some("2026-11-02T00:00:00Z".into()), String::new()));
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.data = Some(&data);
        let v = card_json(&c);
        assert_eq!(v["mod"]["unban"], "Untimeout");
        assert_eq!(v["mod"]["ban"], true);
        assert!(v["chips"][0]["text"].as_str().unwrap().starts_with("Timed out until"));
    }

    #[test]
    fn the_broadcasters_card_has_no_actions() {
        let (u, caps) = (user(), mod_caps());
        let none = ModReach { ban: false, timeout: false, warn: false, delete: false };
        assert!(card_json(&input(&u, None, &caps, none))["mod"].is_null());
    }

    #[test]
    fn a_moderator_without_a_twitch_tv_session_is_offered_the_login() {
        let (u, caps) = (user(), mod_caps());
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.web_status = WebStatus::Expired;
        assert_eq!(card_json(&c)["signin"], true);
        let plain = ModCaps::default();
        let mut c = input(&u, None, &plain, agu_chat_model::mod_caps::REACH_ALL);
        c.web_status = WebStatus::Expired;
        assert_eq!(card_json(&c)["signin"], false);
    }

    #[test]
    fn relogin_and_account_notes() {
        let (u, caps) = (user(), mod_caps());
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.relogin = true;
        assert_eq!(card_json(&c)["notes"][0]["text"], "Log in again to enable mod tools.");
        let c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        assert_eq!(card_json(&c)["notes"][0]["text"], "No account info.");
    }

    #[test]
    fn kick_shows_its_ladder_and_unban_state() {
        let u = CardUser { id: "7".into(), login: "kicker".into(), name: "Kicker".into(), avatar: String::new() };
        let caps = ModCaps::default();
        let t = ModTarget { id: "m".into(), user_id: "7".into(), name: "Kicker".into(), ..Default::default() };
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.platform = Platform::Kick;
        c.co_can_moderate = true;
        c.target = Some(&t);
        let v = card_json(&c);
        assert_eq!(v["mod"]["kind"], "kick");
        assert_eq!(v["mod"]["ladder"].as_array().unwrap().len(), 6);
        assert_eq!(v["mod"]["ban"], true);
        assert!(v["avatar"].as_str().unwrap().contains("default-avatar-"), "Kick's stand-in picture");
        assert_eq!(v["tabs"].as_array().unwrap().len(), 1);
        let data = ModUser { ban: Some((true, None, String::new())), ..Default::default() };
        c.data = Some(&data);
        let v = card_json(&c);
        assert_eq!((v["mod"]["unban"].clone(), v["mod"]["ban"].clone()), (json!("Unban"), json!(false)));
        assert!(v["mod"]["ladder"].is_null());
    }

    #[test]
    fn rumble_has_one_timeout_and_a_ban_when_there_is_a_line_to_act_on() {
        let u = CardUser { id: "7".into(), login: "r".into(), name: "R".into(), avatar: String::new() };
        let caps = ModCaps::default();
        let t = ModTarget { id: "m".into(), login: "r".into(), ..Default::default() };
        let mut c = input(&u, None, &caps, agu_chat_model::mod_caps::REACH_ALL);
        c.platform = Platform::Rumble;
        c.co_can_moderate = true;
        c.last_is_rumble = true;
        c.target = Some(&t);
        assert_eq!(card_json(&c)["mod"]["kind"], "rumble");
        c.last_is_rumble = false;
        assert!(card_json(&c)["mod"].is_null());
    }

    #[test]
    fn a_log_page_becomes_rows_with_a_note_for_held_lines() {
        let page = agu_chat_model::card_web::parse_messages(&json!({"items": [{"kind": "chat", "ts": "2026-10-01T00:00:00Z", "text": "hi"}, {"kind": "caught", "ts": "2026-10-01T00:01:00Z", "text": "x", "status": "DENIED"}], "more": true, "cursor": "abc"}));
        let v = messages_json(&page, &|t, _| json!([{"t": "text", "s": t}]));
        assert_eq!(v["items"].as_array().unwrap().len(), 2);
        assert_eq!(v["items"][1]["note"], "(held by AutoMod, denied)");
        assert_eq!(v["items"][0]["parts"][0]["s"], "hi");
        assert_eq!((v["more"].clone(), v["cursor"].clone()), (json!(true), json!("abc")));
    }
}
