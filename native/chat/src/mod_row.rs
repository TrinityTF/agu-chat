//! What a moderator may do to one chat line, by platform: which of the mod buttons / sheet actions a row offers.
//! The desktop works this out while it builds a row (rows.rs); the phone app asks here, per row, and draws buttons
//! only for what is allowed. Pure: the caps come from `/api/mod/caps`, `/api/kick/caps` and `/api/rumble/caps`.
use agu_chat_core::protocol::Platform;

use super::mod_actions::ModTarget;
use super::mod_caps::{co_reach, mod_reach, youtube_can_delete, youtube_can_mod, CoCaps, ModCaps, ModViewer};
use super::types::{Row, RowBody};

/// What the viewer's account is, in the row's channel.
#[derive(Clone, Debug, Default)]
pub struct ModEnv {
    pub platform: Option<Platform>,
    /// Twitch: what the account may do in the channel
    pub twitch: ModCaps,
    pub viewer: ModViewer,
    /// Kick / Rumble: moderator or owner there; YouTube: logged in
    pub co: CoCaps,
    /// the account's login on the platform ("" when logged out)
    pub viewer_login: String,
    pub viewer_id: String,
    /// the channel's name (the owner's name on Kick / Rumble)
    pub channel: String,
}

impl ModEnv {
    /// Whether this account can moderate anything in the channel: the mod mode toggle shows only then. YouTube
    /// says whether it is allowed only when an action is tried, so a login is enough there.
    pub fn can_moderate(&self) -> bool {
        match self.platform {
            Some(Platform::Twitch) => self.twitch.is_broadcaster == Some(true) || self.twitch.is_mod == Some(true) || self.twitch.ban || self.twitch.delete,
            Some(Platform::Kick) | Some(Platform::Rumble) => self.co.can_moderate,
            Some(Platform::Youtube) => self.co.connected,
            _ => false,
        }
    }

    /// Whether the channel has chat modes the account may change (Twitch only): the shield button shows then.
    pub fn can_set_modes(&self) -> bool {
        self.platform == Some(Platform::Twitch) && (self.twitch.modes || self.twitch.shield || self.twitch.terms)
    }
}

/// The timeout lengths offered for a platform's chatters: Twitch's ladder, Kick's, a few for Rumble; YouTube picks
/// its own length, so it has no choices.
pub fn timeout_ladder(platform: Platform) -> Vec<(&'static str, u32)> {
    match platform {
        Platform::Twitch => super::mod_caps::TIMEOUT_LADDER.to_vec(),
        Platform::Kick => vec![("1m", 60), ("5m", 300), ("10m", 600), ("1h", 3600), ("24h", 86400), ("7d", 604800)],
        Platform::Rumble => vec![("5m", 300), ("10m", 600), ("1h", 3600), ("24h", 86400)],
        _ => vec![],
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowMod {
    pub target: ModTarget,
    pub ban: bool,
    pub timeout: bool,
    pub warn: bool,
    pub delete: bool,
    pub unban: bool,
    /// Rumble: "Pin this message" (a moderator or the owner may pin any line)
    pub pin: bool,
    /// a command button (`/announce ...`) may run: any moderator on Twitch
    pub cmd: bool,
}

impl RowMod {
    pub fn any(&self) -> bool {
        self.ban || self.timeout || self.warn || self.delete || self.unban || self.pin || self.cmd
    }
}

fn twitch_mod(env: &ModEnv, target: ModTarget, badges: &str, deleted: bool, deletable: bool) -> RowMod {
    let reach = mod_reach(&env.viewer, &target.user_id, badges);
    let c = &env.twitch;
    RowMod {
        ban: c.ban && reach.ban && !deleted,
        timeout: c.ban && reach.timeout && !deleted,
        warn: c.warn && reach.warn && !deleted,
        delete: c.delete && reach.delete && !deleted && deletable,
        unban: c.ban && reach.ban,
        pin: false,
        cmd: c.ban || c.delete || c.warn,
        target,
    }
}

/// The moderation a row offers, or None when it has none (not a person's line, no rights, or nothing allowed).
pub fn row_mod(row: &Row, env: &ModEnv) -> Option<RowMod> {
    let m = match (&row.body, row.platform) {
        (RowBody::Msg(m), Platform::Twitch) => twitch_mod(env, ModTarget { id: m.id.clone(), user_id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), menu: None }, &m.badges, m.deleted, true),
        (RowBody::Notice(n), Platform::Twitch) if !n.user.id.is_empty() && n.seventv.is_none() => {
            let t = ModTarget { id: n.id.clone(), user_id: n.user.id.clone(), login: n.user.login.clone(), name: n.user.name.clone(), menu: None };
            twitch_mod(env, t, &n.badges, n.deleted, !n.text.is_empty())
        }
        (RowBody::Automod(h), Platform::Twitch) => {
            let t = ModTarget { id: h.id.clone(), user_id: h.user.id.clone(), login: h.user.login.clone(), name: if h.user.name.is_empty() { h.user.login.clone() } else { h.user.name.clone() }, menu: None };
            twitch_mod(env, t, "", false, false)
        }
        (RowBody::Rumble(m), Platform::Rumble) => {
            let target = if m.user.login.is_empty() { &m.user.name } else { &m.user.login };
            let badges: Vec<&str> = m.badges.iter().map(|b| b.as_str()).collect();
            // Banning and timing out follow co_reach (nobody mutes the owner or themselves); deleting and pinning do
            // not: Rumble lets a moderator or the owner delete any line, their own and the owner's included.
            let ok = env.co.can_moderate && !m.deleted && co_reach(Platform::Rumble, &env.viewer_login, &env.channel, target, &badges).ban;
            let any_line = env.co.can_moderate && !m.deleted;
            RowMod {
                target: ModTarget { id: m.id.clone(), user_id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), menu: None },
                ban: ok,
                timeout: ok,
                unban: ok,
                delete: any_line,
                pin: any_line && !m.notice.is_some(),
                ..Default::default()
            }
        }
        (RowBody::Kick(m), Platform::Kick) => {
            let target = if m.user.login.is_empty() { &m.user.name } else { &m.user.login };
            let badges: Vec<&str> = m.badges.iter().map(|b| b.r#type.as_str()).collect();
            let ok = env.co.can_moderate && !m.user.id.is_empty() && !m.deleted && co_reach(Platform::Kick, &env.viewer_login, &env.channel, target, &badges).ban;
            let gift = m.notice.as_deref() == Some("kicks");
            RowMod {
                target: ModTarget { id: m.id.clone(), user_id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), menu: None },
                ban: ok,
                timeout: ok,
                unban: ok,
                delete: env.co.can_delete && !m.deleted && !gift,
                ..Default::default()
            }
        }
        (RowBody::YouTube(m), Platform::Youtube) => {
            let owner = m.badges.iter().any(|b| b.icon == "OWNER");
            let ok = youtube_can_mod(env.co.connected, m.menu.as_deref().unwrap_or(""), owner, &env.viewer_id, &env.viewer_login, &m.user.id, &m.user.name);
            // YouTube's own menu for the line says whether it can be removed, and a moderator may remove their own
            let delete = youtube_can_delete(env.co.connected, m.menu.as_deref().unwrap_or(""));
            RowMod { target: ModTarget { id: m.id.clone(), user_id: m.user.id.clone(), login: String::new(), name: m.user.name.clone(), menu: m.menu.clone() }, ban: ok, timeout: ok, delete, ..Default::default() }
        }
        _ => return None,
    };
    m.any().then_some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ChatStore;
    use agu_chat_core::protocol::SourceRef;
    use serde_json::json;

    fn row(platform: Platform, channel: &str, frame: serde_json::Value) -> Row {
        let src = SourceRef { platform, channel: channel.into() };
        let mut st = ChatStore::default();
        st.sync(&[src.clone()]);
        let sk = crate::store::source_key(&src);
        st.apply(&sk, &frame);
        (**st.rows().last().expect("a row")).clone()
    }

    fn twitch_env(ban: bool, delete: bool) -> ModEnv {
        ModEnv { platform: Some(Platform::Twitch), twitch: ModCaps { ban, warn: ban, delete, ..Default::default() }, viewer: ModViewer { id: "1".into(), ..Default::default() }, ..Default::default() }
    }

    fn chat(uid: &str, badges: &str) -> serde_json::Value {
        json!({"type": "chat", "msg": {"id": "m1", "ts": 1, "user": {"id": uid, "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": badges}})
    }

    #[test]
    fn a_twitch_mod_may_act_on_a_chatter_but_not_a_broadcaster_or_a_fellow_mod() {
        let env = twitch_env(true, true);
        let r = row_mod(&row(Platform::Twitch, "agu", chat("9", "")), &env).unwrap();
        assert!(r.ban && r.timeout && r.warn && r.delete && r.cmd);
        assert_eq!(r.target.user_id, "9");
        assert_eq!(r.target.id, "m1");
        let b = row_mod(&row(Platform::Twitch, "agu", chat("9", "broadcaster/1")), &env).unwrap();
        assert!(!b.ban && !b.timeout && !b.warn && !b.delete, "nobody acts on the broadcaster");
        let m = row_mod(&row(Platform::Twitch, "agu", chat("9", "moderator/1")), &env).unwrap();
        assert!(!m.ban && !m.timeout && !m.warn && !m.delete, "only the command buttons remain");
        assert!(m.cmd);
    }

    #[test]
    fn without_rights_there_is_nothing() {
        assert!(row_mod(&row(Platform::Twitch, "agu", chat("9", "")), &twitch_env(false, false)).is_none());
        // delete only
        let r = row_mod(&row(Platform::Twitch, "agu", chat("9", "")), &twitch_env(false, true)).unwrap();
        assert!(r.delete && !r.ban);
    }

    #[test]
    fn nobody_bans_themselves_but_may_delete_their_own_line() {
        let r = row_mod(&row(Platform::Twitch, "agu", chat("1", "")), &twitch_env(true, true)).unwrap();
        assert!(!r.ban && !r.timeout && !r.warn && r.delete);
    }

    #[test]
    fn a_deleted_line_offers_nothing_but_commands() {
        let mut deleted = chat("9", "");
        deleted["msg"]["deleted"] = json!(true);
        let r = row_mod(&row(Platform::Twitch, "agu", deleted), &twitch_env(true, true)).unwrap();
        assert!(!r.ban && !r.delete && r.cmd);
    }

    #[test]
    fn a_held_message_can_be_acted_on_but_not_deleted() {
        let held = json!({"type": "automod", "held": {"id": "h1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob"}, "text": "x", "emotes": "", "reason": "automod", "status": "held"}});
        let r = row(Platform::Twitch, "agu", held);
        if matches!(r.body, RowBody::Automod(_)) {
            let m = row_mod(&r, &twitch_env(true, true)).unwrap();
            assert!(m.ban && m.timeout && !m.delete);
            assert_eq!(m.target.id, "h1");
        }
    }

    #[test]
    fn kick_needs_the_account_to_moderate_and_spares_the_owner() {
        let frame = |id: &str, name: &str| json!({"type": "chat", "msg": {"id": "k1", "ts": 1, "text": "hi", "user": {"id": id, "login": name, "name": name, "color": ""}}});
        let env = ModEnv { platform: Some(Platform::Kick), co: CoCaps { connected: true, can_moderate: true, can_delete: true }, viewer_login: "somemod".into(), channel: "agu-tf".into(), ..Default::default() };
        let r = row_mod(&row(Platform::Kick, "agu-tf", frame("5", "viewer")), &env).unwrap();
        assert!(r.ban && r.timeout && r.delete && r.unban && !r.warn && !r.cmd);
        assert_eq!(r.target.user_id, "5");
        let owner = row_mod(&row(Platform::Kick, "agu-tf", frame("6", "Agu_TF")), &env);
        assert!(owner.map_or(true, |o| !o.ban && !o.timeout));
        let none = ModEnv { co: CoCaps::default(), ..env.clone() };
        assert!(row_mod(&row(Platform::Kick, "agu-tf", frame("5", "viewer")), &none).is_none());
        let delete_only = ModEnv { co: CoCaps { connected: true, can_moderate: false, can_delete: true }, ..env };
        let d = row_mod(&row(Platform::Kick, "agu-tf", frame("5", "viewer")), &delete_only).unwrap();
        assert!(d.delete && !d.ban);
    }

    #[test]
    fn rumble_has_mute_delete_and_pin_but_no_warn() {
        let env = ModEnv { platform: Some(Platform::Rumble), co: CoCaps { connected: true, can_moderate: true, can_delete: true }, viewer_login: "me".into(), channel: "streamer".into(), ..Default::default() };
        let f = json!({"type": "chat", "msg": {"id": "r1", "ts": 1, "text": "hi", "user": {"id": "7", "login": "viewer", "name": "Viewer"}, "badges": []}});
        let r = row_mod(&row(Platform::Rumble, "streamer", f), &env).unwrap();
        assert!(r.ban && r.timeout && r.delete && r.unban && r.pin && !r.warn);
    }

    #[test]
    fn rumble_lets_a_moderator_delete_any_line_including_their_own_and_the_owners() {
        let env = ModEnv { platform: Some(Platform::Rumble), co: CoCaps { connected: true, can_moderate: true, can_delete: true }, viewer_login: "me".into(), channel: "streamer".into(), ..Default::default() };
        let f = |login: &str, badges: serde_json::Value| json!({"type": "chat", "msg": {"id": "r1", "ts": 1, "text": "hi", "user": {"id": "7", "login": login, "name": login}, "badges": badges}});
        // my own line: delete and pin, but no mute
        let own = row_mod(&row(Platform::Rumble, "streamer", f("me", json!([]))), &env).unwrap();
        assert!(own.delete && own.pin && !own.ban && !own.timeout);
        // the owner's line, by name and by badge
        let owner = row_mod(&row(Platform::Rumble, "streamer", f("streamer", json!([]))), &env).unwrap();
        assert!(owner.delete && !owner.ban);
        let badge = row_mod(&row(Platform::Rumble, "streamer", f("someone", json!(["streamer"]))), &env).unwrap();
        assert!(badge.delete && !badge.timeout);
        // another moderator: delete yes, mute no (only the owner mutes a moderator)
        let other = row_mod(&row(Platform::Rumble, "streamer", f("modbob", json!(["moderator"]))), &env).unwrap();
        assert!(other.delete && !other.ban);
        // not a moderator: nothing; a deleted line: nothing
        assert!(row_mod(&row(Platform::Rumble, "streamer", f("me", json!([]))), &ModEnv { co: CoCaps::default(), ..env.clone() }).is_none());
        let mut gone = f("viewer", json!([]));
        gone["msg"]["deleted"] = json!(true);
        assert!(row_mod(&row(Platform::Rumble, "streamer", gone), &env).is_none());
    }

    #[test]
    fn youtube_lets_a_moderator_delete_their_own_line() {
        let env = ModEnv { platform: Some(Platform::Youtube), co: CoCaps { connected: true, can_moderate: true, can_delete: true }, viewer_id: "UC1".into(), ..Default::default() };
        let f = json!({"type": "chat", "messages": [{"id": "y1", "ts": 1, "text": "hi", "user": {"id": "UC1", "name": "Me"}, "badges": [], "parts": [], "menu": "m"}]});
        let r = row_mod(&row(Platform::Youtube, "vid", f), &env).unwrap();
        assert!(r.delete && !r.ban && !r.timeout);
    }

    #[test]
    fn youtube_needs_a_menu_and_a_login() {
        let f = |menu: serde_json::Value| json!({"type": "chat", "messages": [{"id": "y1", "ts": 1, "text": "hi", "user": {"id": "UC1", "name": "Viewer"}, "badges": [], "parts": [], "menu": menu}]});
        let env = ModEnv { platform: Some(Platform::Youtube), co: CoCaps { connected: true, can_moderate: true, can_delete: true }, ..Default::default() };
        let r = row_mod(&row(Platform::Youtube, "vid", f(json!("menuparams"))), &env).unwrap();
        assert!(r.ban && r.timeout && r.delete);
        assert_eq!(r.target.menu.as_deref(), Some("menuparams"));
        assert!(row_mod(&row(Platform::Youtube, "vid", f(json!(""))), &env).is_none());
        assert!(row_mod(&row(Platform::Youtube, "vid", f(json!("m"))), &ModEnv { co: CoCaps::default(), ..env }).is_none());
    }

    #[test]
    fn each_platform_has_its_own_timeout_choices() {
        assert_eq!(timeout_ladder(Platform::Twitch).len(), 9);
        assert_eq!(timeout_ladder(Platform::Kick)[0], ("1m", 60));
        assert_eq!(timeout_ladder(Platform::Rumble)[0], ("5m", 300));
        assert!(timeout_ladder(Platform::Youtube).is_empty());
    }

    #[test]
    fn which_accounts_see_the_toggles() {
        assert!(twitch_env(true, false).can_moderate());
        assert!(!twitch_env(false, false).can_moderate());
        let mut e = twitch_env(false, false);
        e.twitch.is_mod = Some(true);
        assert!(e.can_moderate());
        assert!(!e.can_set_modes());
        e.twitch.shield = true;
        assert!(e.can_set_modes());
        let yt = ModEnv { platform: Some(Platform::Youtube), co: CoCaps { connected: true, ..Default::default() }, ..Default::default() };
        assert!(yt.can_moderate() && !yt.can_set_modes());
        assert!(!ModEnv { platform: Some(Platform::X), ..Default::default() }.can_moderate());
    }
}
