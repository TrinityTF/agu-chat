//! Moderation for the phone, on the session: what the account may do in each channel (caps), a moderation action
//! (ban, timeout, warn, delete, unban) with the desktop's confirm rules, mod-mode command buttons, AutoMod
//! Allow / Deny, the user card, and the channel modes sheet. The requests and wording come from `agu-chat-model`
//! (`mod_actions`, `mod_row`, `card_web`, `channel_modes`), the card's layout from `card.rs`.
//!
//! A child of `session`: it reads and writes the session's private state directly.
use agu_chat_model::card_web::{parse_card, parse_comment, parse_messages, WebStatus};
use agu_chat_model::channel_modes::{parse_modes, with_current, Modes, FOLLOWER_MODE_OPTIONS, SLOW_MODE_OPTIONS};
use agu_chat_model::ignores::{ignore_login, is_user_ignored, toggle_ignored_user};
use agu_chat_model::input::slash::post_json_error;
use agu_chat_model::mod_actions::{automod_body, automod_result, confirm_text, plan, plan_result, AutomodResult, ModTarget};
use agu_chat_model::mod_buttons::{expand_template, TemplateVars};
use agu_chat_model::mod_caps::{badge_names, co_reach, fetch_co_caps, mod_reach, CoCaps, ModReach, ModViewer, RelayResponse, REACH_ALL};
use agu_chat_model::mod_row::{row_mod, ModEnv};
use agu_chat_model::twitch::parts::build_parts_in;
use agu_chat_model::types::{Row, RowRef};
use agu_chat_model::twitch::emotes::EmoteLookup;
use agu_chat_model::user_card::{parse_mod_user, sub_info, ModUser};

use super::*;
use crate::card::{card_json, messages_json, CardIn, CardUser};
use crate::view::parts_json;

/// Debuggable builds only: while `block` is on, moderation requests do not reach the relay (and so no platform);
/// a path with a canned answer gets it, anything else fails. Lets the screens be tried without a live account.
#[derive(Default)]
pub(super) struct DevRelay {
    pub block: bool,
    pub canned: Vec<(String, u16, Value)>,
}

fn me_id(accounts: &AccountsState, platform: Platform) -> String {
    accounts.get(&platform).and_then(|a| a.user.as_ref()).map(|u| u.id.clone()).unwrap_or_default()
}

fn account_login(accounts: &AccountsState, platform: Platform) -> String {
    accounts.get(&platform).and_then(|a| a.user.as_ref()).map(|u| if u.login.is_empty() { u.name.clone() } else { u.login.clone() }).unwrap_or_default()
}

/// What the account may do in a channel: None where nothing can be moderated at all (X, whispers).
pub(super) fn build_mod_env(source: &SourceRef, d: Option<&SrcData>, rules: &Rules, store: Option<&ChatStore>) -> Option<ModEnv> {
    let accounts = &rules.accounts;
    let platform = source.platform;
    let sk = source_key(source);
    match platform {
        Platform::X => None,
        Platform::Twitch => {
            if whisper::login_of(&source.channel).is_some() {
                return None;
            }
            let caps = d.map(|d| d.caps.clone()).unwrap_or_default();
            let me_login = rules.me_login().to_lowercase();
            let me = me_id(accounts, Platform::Twitch);
            let broadcaster = caps.is_broadcaster.unwrap_or(!me_login.is_empty() && source.channel.to_lowercase().trim_start_matches('#') == me_login);
            let lead = !me.is_empty()
                && store
                    .and_then(|s| {
                        s.rows().iter().rev().find_map(|r| match &r.body {
                            RowBody::Msg(m) if r.sk == sk && m.user.id == me => Some(badge_names(&m.badges).contains("lead_moderator")),
                            _ => None,
                        })
                    })
                    .unwrap_or(false);
            Some(ModEnv { platform: Some(platform), twitch: caps, viewer: ModViewer { id: me, broadcaster, lead }, viewer_login: me_login, channel: source.channel.clone(), ..Default::default() })
        }
        Platform::Youtube => {
            let connected = accounts.get(&Platform::Youtube).map(|a| a.connected).unwrap_or(false);
            Some(ModEnv {
                platform: Some(platform),
                co: CoCaps { connected, can_moderate: connected, can_delete: connected },
                viewer_login: account_login(accounts, platform),
                viewer_id: me_id(accounts, platform),
                channel: source.channel.clone(),
                ..Default::default()
            })
        }
        Platform::Kick | Platform::Rumble => Some(ModEnv { platform: Some(platform), co: d.map(|d| d.co.clone()).unwrap_or_default(), viewer_login: account_login(accounts, platform), channel: source.channel.clone(), ..Default::default() }),
    }
}

fn s_arg<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(|x| x.as_str()).unwrap_or("")
}

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn esc(s: &str) -> String {
    s.replace('%', "%25").replace('&', "%26")
}

fn modes_json(m: Option<&Modes>, shield: Option<bool>, caps: &ModCaps) -> Value {
    let fcur = m.map(|m| if m.follower_mode { m.follower_mode_duration.unwrap_or(0) } else { -1 }).unwrap_or(-1);
    let scur = m.map(|m| if m.slow_mode { m.slow_mode_wait_time.unwrap_or(0) } else { 0 }).unwrap_or(0);
    let fopts = with_current(&FOLLOWER_MODE_OPTIONS.iter().map(|(v, l)| (*v, l.to_string())).collect::<Vec<_>>(), fcur, |v| format!("{v}m"));
    let sopts = with_current(&SLOW_MODE_OPTIONS.iter().map(|s| (*s, if *s == 0 { "Off".to_string() } else { format!("{s}s") })).collect::<Vec<_>>(), scur, |v| format!("{v}s"));
    json!({
        "caps": {"modes": caps.modes, "shield": caps.shield, "terms": caps.terms},
        "modes": m.map(|m| json!({"subscriber": m.subscriber_mode, "emote": m.emote_mode, "unique": m.unique_chat_mode})),
        "shield": shield,
        "followerCur": fcur,
        "followerOptions": fopts.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>(),
        "slowCur": scur,
        "slowOptions": sopts.iter().map(|(v, l)| json!([v, l])).collect::<Vec<_>>(),
        "restricted": agu_chat_model::channel_modes::restricted(m, shield.unwrap_or(false)),
    })
}

impl Session {
    /// Every moderation request goes through here (see `DevRelay`).
    pub(super) async fn relay(&self, source: &SourceRef, path: &str, method: Option<&str>, body: Option<Value>) -> RelayResponse {
        {
            let d = self.inner.dev.lock().unwrap();
            if d.block {
                let hit = d.canned.iter().filter(|(p, _, _)| path.starts_with(p.as_str())).max_by_key(|(p, _, _)| p.len());
                return match hit {
                    Some((_, status, body)) => RelayResponse { ok: (200..300).contains(status), status: *status, body: body.clone() },
                    None => RelayResponse { ok: false, status: 0, body: json!("blocked in dev mode") },
                };
            }
        }
        relay_api(&self.inner.bridge, source, path, method, body).await
    }

    async fn relay_post(&self, source: &SourceRef, path: &str, body: Option<Value>, method: &str) -> Result<RelayResponse, String> {
        let r = self.relay(source, path, Some(method), body).await;
        if r.ok {
            Ok(r)
        } else {
            Err(post_json_error(&r))
        }
    }

    // ---- what the account may do ---------------------------------------------------------------

    /// `{sk}`: refresh what the account may do in a channel (on login and when a channel opens).
    pub(super) async fn load_caps(&self, sk: String, source: SourceRef) {
        if self.inner.st.lock().unwrap().src.get(&sk).map(|d| d.dev_caps).unwrap_or(false) {
            return;
        }
        match source.platform {
            Platform::Twitch if whisper::login_of(&source.channel).is_none() => {
                let caps = fetch_mod_caps(&self.inner.bridge, &source).await;
                if let Some(d) = self.inner.st.lock().unwrap().src.get_mut(&sk) {
                    d.caps = caps;
                }
            }
            Platform::Kick | Platform::Rumble => {
                let co = fetch_co_caps(&self.inner.bridge, &source).await;
                if let Some(d) = self.inner.st.lock().unwrap().src.get_mut(&sk) {
                    d.co = co;
                }
            }
            _ => {}
        }
        self.caps_changed(&sk);
    }

    /// The rows are drawn again with the new rights, and the app hears what the channel's header should offer.
    pub(super) fn caps_changed(&self, sk: &str) {
        let ev = {
            let mut st = self.inner.st.lock().unwrap();
            if !st.hub.is_held(sk) {
                return;
            }
            st.touch_sk(sk);
            st.caps_event(sk)
        };
        self.emit(vec![("caps", ev)]);
    }

    // ---- actions ---------------------------------------------------------------------------------

    /// `{sk, action, t, seconds?, reason?, quick?, confirmed?}`: ban / timeout / warn / delete / unban. Kick, Rumble
    /// and YouTube ask before a ban (`{confirm: {text}}`, answered by calling again with `confirmed`). Answers
    /// `{ok, toast?, warn}`.
    pub(super) async fn mod_action(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?;
        let source = self.source_of(sk)?;
        let action = str_arg(args, "action")?;
        let t = ModTarget::from_json(args.get("t").unwrap_or(&Value::Null));
        if !args.get("confirmed").and_then(|c| c.as_bool()).unwrap_or(false) {
            if let Some(text) = confirm_text(source.platform, action, &t) {
                return Ok(json!({"ok": false, "confirm": {"text": text}}));
            }
        }
        let seconds = args.get("seconds").and_then(|s| s.as_u64()).map(|s| s as u32);
        let p = match plan(source.platform, action, &t, seconds, s_arg(args, "reason"), args.get("quick").and_then(|q| q.as_bool()).unwrap_or(false)) {
            Ok(p) => p,
            Err(e) => return Ok(json!({"ok": false, "toast": e, "warn": true})),
        };
        let r = self.relay(&source, p.req.path, Some(p.req.method), Some(p.req.body.clone())).await;
        let (ok, toast) = plan_result(&p, &r);
        if ok && source.platform == Platform::Twitch && action == "delete" {
            let mut st = self.inner.st.lock().unwrap();
            for vk in st.hub.views_of(sk) {
                if let Some(v) = st.views.get_mut(&vk) {
                    v.store.mark_deleted(sk, &t.id);
                    v.dirty = true;
                }
            }
        }
        Ok(match toast {
            Some((t, warn)) => json!({"ok": ok, "toast": t, "warn": warn}),
            None => json!({"ok": ok}),
        })
    }

    /// `{sk, vk, template, t, text}`: a mod-mode command button. The template's {user} {name} {user-id} {msg-id}
    /// {channel} {msg} are filled in and the line goes out as the message box would send it (a slash command runs,
    /// `/clear` and `/raid` ask first). Answers like `compose.send`.
    pub(super) async fn mod_command(&self, args: &Value) -> Result<Value, String> {
        let (sk, vk) = (str_arg(args, "sk")?, str_arg(args, "vk")?);
        let source = self.source_of(sk)?;
        let t = ModTarget::from_json(args.get("t").unwrap_or(&Value::Null));
        let line = expand_template(
            s_arg(args, "template"),
            &TemplateVars { user: &t.login, name: Some(&t.name), user_id: Some(&t.user_id), msg_id: Some(&t.id), channel: Some(&source.channel), msg: Some(s_arg(args, "text")) },
        )
        .trim()
        .to_string();
        if line.is_empty() {
            return Ok(json!({"clear": false}));
        }
        self.compose_send(&json!({"vk": vk, "sks": [sk], "text": line})).await
    }

    /// `{sk, id, allow}`: AutoMod Allow / Deny. On success the row flips by itself when Twitch says so; a hold
    /// Twitch no longer has is marked expired here.
    pub(super) async fn mod_automod(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?;
        let source = self.source_of(sk)?;
        let id = str_arg(args, "id")?;
        let allow = args.get("allow").and_then(|a| a.as_bool()).unwrap_or(false);
        let r = self.relay(&source, "/api/mod/automod", Some("POST"), Some(automod_body(id, if allow { "allow" } else { "deny" }))).await;
        let (res, toast) = automod_result(&r);
        if res == AutomodResult::Gone {
            let mut st = self.inner.st.lock().unwrap();
            for vk in st.hub.views_of(sk) {
                if let Some(v) = st.views.get_mut(&vk) {
                    v.store.mark_automod_expired(sk, id);
                    v.dirty = true;
                }
            }
        }
        Ok(match toast {
            Some((t, warn)) => json!({"ok": res == AutomodResult::Ok, "toast": t, "warn": warn}),
            None => json!({"ok": true}),
        })
    }

    /// `{sk}` -> the channel's chat rules, for a warning's reasons.
    pub(super) async fn mod_rules(&self, args: &Value) -> Result<Value, String> {
        let source = self.source_of(str_arg(args, "sk")?)?;
        let r = self.relay(&source, "/api/chat/rules", None, None).await;
        let rules: Vec<String> = if r.ok { r.body.get("rules").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect()).unwrap_or_default() } else { vec![] };
        Ok(json!({"rules": rules}))
    }

    // ---- the user card ---------------------------------------------------------------------------

    /// `{sk, user: {id, login, name, avatar}}` -> the card (see `card.rs`), with the person's lines in this chat.
    pub(super) async fn card_info(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?.to_string();
        let source = self.source_of(&sk)?;
        let u = args.get("user").cloned().unwrap_or(Value::Null);
        let user = CardUser { id: s_arg(&u, "id").to_string(), login: s_arg(&u, "login").to_string(), name: s_arg(&u, "name").to_string(), avatar: s_arg(&u, "avatar").to_string() };
        let platform = source.platform;
        let whisper = platform == Platform::Twitch && whisper::login_of(&source.channel).is_some();

        // what the app already knows
        struct Known {
            caps: ModCaps,
            reach: ModReach,
            suspicious: Option<agu_chat_model::types::SuspiciousUser>,
            co_can: bool,
            sub: Option<agu_chat_model::user_card::SubInfo>,
            recent: Vec<Value>,
            total: usize,
            ignore: Option<bool>,
            target: Option<ModTarget>,
            last_rumble: bool,
        }
        let k = {
            let st = self.inner.st.lock().unwrap();
            let d = st.src.get(&sk);
            let vk = st.hub.views_of(&sk).into_iter().next();
            let store = vk.as_ref().and_then(|v| st.views.get(v)).map(|v| &v.store);
            let env = build_mod_env(&source, d, &st.rules, store).unwrap_or_default();
            let mine = |r: &Row| -> bool {
                r.sk == sk
                    && match &r.body {
                        RowBody::Msg(m) => m.user.id == user.id,
                        RowBody::Rumble(m) => m.user.id == user.id,
                        RowBody::Kick(m) => m.user.id == user.id && !m.text.is_empty(),
                        RowBody::YouTube(m) => m.user.id == user.id,
                        RowBody::X(m) => m.user.id == user.id,
                        _ => false,
                    }
            };
            let rows: Vec<RowRef> = store.map(|s| s.rows().to_vec()).unwrap_or_default();
            let recent_rows: Vec<RowRef> = rows.iter().rev().filter(|r| mine(r)).take(30).cloned().collect();
            let total = rows.iter().filter(|r| mine(r)).count();
            let latest = recent_rows.first().cloned();
            let me = me_id(&st.rules.accounts, Platform::Twitch);
            let reach = match platform {
                Platform::Kick | Platform::Rumble => {
                    let badges: Vec<String> = latest
                        .as_ref()
                        .map(|r| match &r.body {
                            RowBody::Kick(m) => m.badges.iter().map(|b| b.r#type.clone()).collect(),
                            RowBody::Rumble(m) => m.badges.clone(),
                            _ => vec![],
                        })
                        .unwrap_or_default();
                    let names: Vec<&str> = badges.iter().map(|b| b.as_str()).collect();
                    let target = if user.login.is_empty() { &user.name } else { &user.login };
                    co_reach(platform, &env.viewer_login, &source.channel, target, &names)
                }
                Platform::Twitch => {
                    if user.login.eq_ignore_ascii_case(&source.channel) {
                        ModReach { ban: false, timeout: false, warn: false, delete: false }
                    } else {
                        match latest.as_ref().and_then(|r| match &r.body {
                            RowBody::Msg(m) => Some(m.badges.clone()),
                            _ => None,
                        }) {
                            Some(b) => mod_reach(&env.viewer, &user.id, &b),
                            None if user.id == me && !me.is_empty() => ModReach { ban: false, timeout: false, warn: false, delete: true },
                            None => REACH_ALL,
                        }
                    }
                }
                _ => REACH_ALL,
            };
            let ctx = Ctx { src: &st.src, rules: &st.rules, defs: st.defs.clone(), book: &st.book };
            let mut renv = env_of(&sk, &ctx, store);
            renv.mods = None;
            let recent: Vec<Value> = recent_rows
                .iter()
                .map(|r| {
                    let mut v = row_json(r, &renv);
                    st.rules.decorate(&mut v, r);
                    v
                })
                .collect();
            let sub = latest.as_ref().and_then(|r| match &r.body {
                RowBody::Msg(m) => sub_info(&m.badges, &m.badge_info),
                _ => None,
            });
            let login = ignore_login(&user.login, &user.name);
            let my_login = if platform == Platform::Twitch { st.rules.me_login().to_string() } else { account_login(&st.rules.accounts, platform) };
            let is_me = (platform == Platform::Twitch && !me.is_empty() && user.id == me) || (!my_login.is_empty() && ignore_login(&my_login, "") == login);
            let can_ignore = !login.is_empty() && !is_me;
            Known {
                caps: d.map(|d| d.caps.clone()).unwrap_or_default(),
                reach,
                suspicious: store.and_then(|s| s.source(&sk)).and_then(|s| s.suspicious.get(&user.id).cloned()),
                co_can: env.co.can_moderate,
                sub,
                recent,
                total,
                ignore: can_ignore.then(|| is_user_ignored(&st.rules.settings.ignored_users, platform, &login)),
                target: latest.as_ref().and_then(|r| row_mod(r, &env)).map(|m| m.target),
                last_rumble: latest.as_ref().map(|r| matches!(r.body, RowBody::Rumble(_))).unwrap_or(false),
            }
        };

        // what the platform says about them
        let mut web = None;
        let mut web_status = WebStatus::Unknown;
        let mut web_error: Option<String> = None;
        let mut data: Option<ModUser> = None;
        let mut relogin = false;
        if platform == Platform::Twitch && !user.id.is_empty() {
            let login: String = if user.login.is_empty() { user.name.to_lowercase() } else { user.login.clone() }.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            if !whisper && !login.is_empty() {
                let r = self.relay(&source, &format!("/api/twitch/card?login={login}&id={}", digits(&user.id)), None, None).await;
                web_status = WebStatus::from_answer(r.status, &r.body);
                if r.ok {
                    web = Some(parse_card(&r.body));
                } else if !matches!(web_status, WebStatus::SignedOut | WebStatus::Expired) && r.status != 404 {
                    web_error = Some(format!("Twitch's mod view didn't load: {}", r.text()));
                }
            }
            // Helix: the account itself, and (for a broadcaster) the ban, when Twitch's card has no mod logs
            let helix = if web.as_ref().map(|w| w.logs.is_some() && w.followed_at.is_some()).unwrap_or(false) { None } else { Some(self.relay(&source, &format!("/api/mod/user?id={}", esc(&user.id)), None, None).await) };
            if let Some(w) = &web {
                let mut d = ModUser {
                    login: w.login.clone(),
                    display_name: w.display.clone(),
                    avatar: w.avatar.clone(),
                    created_at: w.created_at.clone(),
                    followed_at: w.followed_at.clone(),
                    ban: w.ban.as_ref().map(|b| (b.permanent, b.expires_at.clone(), b.reason.clone())),
                    ..Default::default()
                };
                if let Some(r) = helix.as_ref().filter(|r| r.ok) {
                    let h = parse_mod_user(&r.body);
                    d.ban = d.ban.or(h.ban);
                    d.followed_at = d.followed_at.or(h.followed_at);
                    d.follow_error = h.follow_error;
                    if d.created_at.is_empty() {
                        d.created_at = h.created_at;
                    }
                    d.ban_error = h.ban_error;
                }
                data = Some(d);
            } else if let Some(r) = helix {
                if r.status == 403 && r.text().contains("relogin") {
                    relogin = true;
                } else if r.status == 401 || r.status == 0 {
                    // logged out: the card just has no account info
                } else if !r.ok {
                    web_error = Some(format!("Couldn't load user info: {}", r.text()));
                } else {
                    data = Some(parse_mod_user(&r.body));
                }
            }
        } else if platform == Platform::Kick && !user.name.is_empty() {
            let name: String = user.name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
            let r = self.relay(&source, &format!("/api/kick/user?name={name}"), None, None).await;
            if r.ok {
                data = Some(parse_mod_user(&r.body));
            }
        }

        let c = CardIn {
            platform,
            whisper,
            user: &user,
            web: web.as_ref(),
            web_status,
            web_error: web_error.as_deref(),
            data: data.as_ref(),
            relogin,
            loading: false,
            caps: &k.caps,
            reach: k.reach,
            suspicious: k.suspicious.as_ref(),
            co_can_moderate: k.co_can,
            sub: k.sub,
            recent_total: k.total,
            ignore: k.ignore,
            target: k.target.as_ref(),
            last_is_rumble: k.last_rumble,
        };
        let mut v = card_json(&c);
        v["recent"] = Value::Array(k.recent);
        Ok(v)
    }

    /// `{sk, id, cursor?}` -> a page of Twitch's log of the person's lines.
    pub(super) async fn card_messages(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?;
        let source = self.source_of(sk)?;
        let mut path = format!("/api/twitch/card/messages?id={}", digits(s_arg(args, "id")));
        let cursor: String = s_arg(args, "cursor").chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '.')).collect();
        if !cursor.is_empty() {
            path.push_str(&format!("&cursor={cursor}"));
        }
        let emotes = self.inner.st.lock().unwrap().src.get(sk).and_then(|d| d.emotes.clone());
        let r = self.relay(&source, &path, None, None).await;
        if !r.ok {
            return Ok(json!({"items": [], "more": false, "cursor": null, "failed": true}));
        }
        let look = EmoteLookup::new(emotes.as_deref(), None);
        let page = parse_messages(&r.body);
        Ok(messages_json(&page, &|text, tag| parts_json(&build_parts_in(text, tag, look, None, ""), &look)))
    }

    /// `{sk, targetId, text}` / `{sk, commentId}` (delete): the shared mod comments.
    pub(super) async fn card_comment(&self, args: &Value, delete: bool) -> Result<Value, String> {
        let source = self.source_of(str_arg(args, "sk")?)?;
        if delete {
            let r = self.relay(&source, "/api/twitch/card/comment", Some("DELETE"), Some(json!({"commentId": s_arg(args, "commentId")}))).await;
            return Ok(if r.ok { json!({"ok": true}) } else { json!({"ok": false, "toast": format!("Couldn't delete the comment: {}", r.text()), "warn": true}) });
        }
        let r = self.relay(&source, "/api/twitch/card/comment", Some("POST"), Some(json!({"targetId": s_arg(args, "targetId"), "text": s_arg(args, "text")}))).await;
        if !r.ok {
            return Ok(json!({"ok": false, "toast": format!("Couldn't add the comment: {}", r.text()), "warn": true}));
        }
        let c = parse_comment(&r.body["comment"]);
        Ok(json!({"ok": true, "comment": {"id": c.id, "text": c.text, "by": c.by, "stamp": agu_chat_model::user_card::fmt_stamp(agu_chat_model::card_web::ts_ms(&c.ts)), "mine": true}}))
    }

    /// `{platform, login, on}`: ignore / unignore a person (the same list as Settings -> Ignores).
    pub(super) async fn card_ignore(&self, args: &Value) -> Result<Value, String> {
        let platform = platform_of(args)?;
        let login = str_arg(args, "login")?;
        let on = args.get("on").and_then(|o| o.as_bool()).unwrap_or(true);
        let cur = self.inner.st.lock().unwrap().rules.settings.ignored_users.clone();
        let next = toggle_ignored_user(&cur, platform, &ignore_login(login, ""), on);
        let _ = self.inner.bridge.settings_set(json!({"ignoredUsers": next})).await;
        Ok(json!({"toast": if on { format!("Ignoring {login}") } else { format!("No longer ignoring {login}") }, "warn": false}))
    }

    // ---- channel modes ---------------------------------------------------------------------------

    fn caps_of(&self, sk: &str) -> ModCaps {
        self.inner.st.lock().unwrap().src.get(sk).map(|d| d.caps.clone()).unwrap_or_default()
    }

    /// `{sk, fresh?}` -> the channel's modes and what the account may change.
    pub(super) async fn modes_get(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?;
        let source = self.source_of(sk)?;
        let caps = self.caps_of(sk);
        let fresh = args.get("fresh").and_then(|f| f.as_bool()).unwrap_or(false);
        let m = if caps.modes {
            let r = self.relay(&source, &format!("/api/chat/settings{}", if fresh { "?fresh=1" } else { "" }), None, None).await;
            r.ok.then(|| parse_modes(&r.body))
        } else {
            None
        };
        let sh = if caps.shield {
            let r = self.relay(&source, "/api/chat/shield", None, None).await;
            r.ok.then(|| r.body.get("is_active").and_then(|x| x.as_bool()).unwrap_or(false))
        } else {
            None
        };
        Ok(modes_json(m.as_ref(), sh, &caps))
    }

    /// `{sk, body}`: change a room setting; answers the settings after it (or `{error}`).
    pub(super) async fn modes_patch(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?;
        let source = self.source_of(sk)?;
        let caps = self.caps_of(sk);
        Ok(match self.relay_post(&source, "/api/chat/settings", args.get("body").cloned(), "POST").await {
            Ok(r) => modes_json(Some(&parse_modes(&r.body)), None, &caps),
            Err(e) => json!({"error": if e.is_empty() { "update failed".to_string() } else { e }}),
        })
    }

    /// `{sk, on}`: Shield Mode.
    pub(super) async fn modes_shield(&self, args: &Value) -> Result<Value, String> {
        let source = self.source_of(str_arg(args, "sk")?)?;
        let on = args.get("on").and_then(|o| o.as_bool()).unwrap_or(false);
        Ok(match self.relay_post(&source, "/api/chat/shield", Some(json!({"is_active": on})), "PUT").await {
            Ok(r) => json!({"shield": r.body.get("is_active").and_then(|x| x.as_bool()).unwrap_or(on)}),
            Err(e) => json!({"error": if e.is_empty() { "update failed".to_string() } else { e }}),
        })
    }

    /// `{sk}` -> the blocked terms; `{sk, text}` adds one; `{sk, id}` removes one.
    pub(super) async fn terms(&self, args: &Value, op: &str) -> Result<Value, String> {
        let source = self.source_of(str_arg(args, "sk")?)?;
        let list = |r: &RelayResponse| -> Value {
            Value::Array(r.body.get("terms").and_then(|t| t.as_array()).map(|a| a.iter().take(100).map(|t| json!({"id": s_arg(t, "id"), "text": s_arg(t, "text")})).collect()).unwrap_or_default())
        };
        Ok(match op {
            "list" => {
                let r = self.relay(&source, "/api/chat/terms", None, None).await;
                if r.ok { json!({"terms": list(&r)}) } else { json!({"error": post_json_error(&r)}) }
            }
            "add" => match self.relay_post(&source, "/api/chat/terms", Some(json!({"text": s_arg(args, "text")})), "POST").await {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"error": e}),
            },
            _ => match self.relay_post(&source, &format!("/api/chat/terms?id={}", esc(s_arg(args, "id"))), None, "DELETE").await {
                Ok(_) => json!({"ok": true}),
                Err(e) => json!({"error": e}),
            },
        })
    }

    // ---- debuggable builds ------------------------------------------------------------------------

    /// `dev.caps {sk, twitch?: {ban, warn, delete, ...}, co?: {...}}`: pretend the account may moderate here, and keep
    /// every moderation request away from the platforms.
    pub(super) fn dev_caps(&self, args: &Value) -> Result<Value, String> {
        let sk = str_arg(args, "sk")?.to_string();
        {
            let mut st = self.inner.st.lock().unwrap();
            let b = |o: &Value, k: &str| o.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            let d = st.src.entry(sk.clone()).or_default();
            d.dev_caps = true;
            if let Some(t) = args.get("twitch") {
                d.caps = ModCaps {
                    ban: b(t, "ban"),
                    warn: b(t, "warn"),
                    delete: b(t, "delete"),
                    modes: b(t, "modes"),
                    shield: b(t, "shield"),
                    terms: b(t, "terms"),
                    automod: b(t, "automod"),
                    pin: b(t, "pin"),
                    is_mod: Some(true),
                    is_broadcaster: t.get("broadcaster").and_then(|v| v.as_bool()),
                    logged_in: Some(true),
                    ..Default::default()
                };
            }
            if let Some(c) = args.get("co") {
                d.co = CoCaps { connected: true, can_moderate: b(c, "canModerate"), can_delete: b(c, "canDelete") };
            }
        }
        self.inner.dev.lock().unwrap().block = true;
        self.caps_changed(&sk);
        Ok(Value::Null)
    }

    /// `dev.relay {path, status?, body}`: a canned relay answer for the paths that start with `path`.
    pub(super) fn dev_relay(&self, args: &Value) -> Result<Value, String> {
        let mut d = self.inner.dev.lock().unwrap();
        d.block = true;
        d.canned.retain(|(p, _, _)| p != str_arg(args, "path").unwrap_or(""));
        d.canned.push((str_arg(args, "path")?.to_string(), args.get("status").and_then(|s| s.as_u64()).unwrap_or(200) as u16, args.get("body").cloned().unwrap_or(Value::Null)));
        Ok(Value::Null)
    }
}

impl State {
    /// What the channel's header offers: `{sk, mod, modes}`.
    pub(super) fn caps_event(&self, sk: &str) -> Value {
        let Some(source) = self.hub.source(sk) else { return json!({"sk": sk, "mod": false, "modes": false}) };
        let store = self.hub.views_of(sk).into_iter().next().and_then(|v| self.views.get(&v)).map(|v| &v.store);
        let env = build_mod_env(&source, self.src.get(sk), &self.rules, store);
        json!({"sk": sk, "mod": env.as_ref().map(|e| e.can_moderate()).unwrap_or(false), "modes": env.as_ref().map(|e| e.can_set_modes()).unwrap_or(false)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_are_listed_with_the_current_value_among_the_options() {
        let caps = ModCaps { modes: true, shield: true, terms: true, ..Default::default() };
        let m = parse_modes(&json!({"emote_mode": true, "follower_mode": true, "follower_mode_duration": 20, "slow_mode": true, "slow_mode_wait_time": 15, "subscriber_mode": false, "unique_chat_mode": false}));
        let v = modes_json(Some(&m), Some(true), &caps);
        assert_eq!(v["followerCur"], 20);
        assert!(v["followerOptions"].as_array().unwrap().iter().any(|o| o == &json!([20, "20m"])), "a value set elsewhere joins the list");
        assert_eq!(v["slowCur"], 15);
        assert!(v["slowOptions"].as_array().unwrap().iter().any(|o| o == &json!([15, "15s"])));
        assert_eq!(v["modes"]["emote"], true);
        assert_eq!(v["restricted"], true);
        let off = modes_json(Some(&Modes::default()), Some(false), &caps);
        assert_eq!((off["followerCur"].clone(), off["slowCur"].clone(), off["restricted"].clone()), (json!(-1), json!(0), json!(false)));
        assert_eq!(off["followerOptions"][0], json!([-1, "Off"]));
        assert_eq!(off["slowOptions"][0], json!([0, "Off"]));
    }

    #[test]
    fn ids_and_queries_are_cleaned() {
        assert_eq!(digits("12a3"), "123");
        assert_eq!(esc("a&b%"), "a%26b%25");
    }

    #[test]
    fn the_account_is_a_mod_where_twitch_says_so_and_not_in_a_whisper() {
        let rules = Rules::default();
        let src = SourceRef { platform: Platform::Twitch, channel: "agu".into() };
        let d = SrcData { caps: ModCaps { ban: true, ..Default::default() }, ..Default::default() };
        let e = build_mod_env(&src, Some(&d), &rules, None).unwrap();
        assert!(e.can_moderate());
        assert!(build_mod_env(&SourceRef { platform: Platform::X, channel: "x".into() }, None, &rules, None).is_none());
        assert!(!build_mod_env(&src, None, &rules, None).unwrap().can_moderate());
        let whisper = SourceRef { platform: Platform::Twitch, channel: whisper::channel_for("bob") };
        assert!(build_mod_env(&whisper, Some(&d), &rules, None).is_none());
    }
}
