//! What the Android screen draws for one row: the shared chat model's `Row` flattened into plain JSON (names,
//! colours, badge and emote image URLs, message parts), so the Kotlin side only lays it out.
use std::collections::HashMap;

use agu_chat_model::twitch::badges::{parse_badge_list, BadgeMap};
use agu_chat_model::twitch::colors::{fallback_color, name_color, readable_color};
use agu_chat_model::twitch::emotes::{provider_emote_url_at, EmoteLookup, EmoteSets};
use agu_chat_model::twitch::parts::{build_kick_parts_in, build_parts_in, EmoteSource, KnownChatter, MsgPart, Overlay};
use agu_chat_model::types::{ChatUser, Row, RowBody};
use serde_json::{json, Value};

/// Image density of emotes and badges: phones draw at 2.6x-3.5x, so the largest size the CDNs offer.
const DENSITY: usize = 3;

/// What a source's rows are drawn against.
#[derive(Default, Clone)]
pub struct Env {
    pub badges: std::sync::Arc<BadgeMap>,
    pub emotes: Option<std::sync::Arc<EmoteSets>>,
    pub known: HashMap<String, KnownChatter>,
}

pub fn twitch_emote_url(id: &str) -> String {
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/3.0")
}

fn kick_emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

fn emote_url(source: EmoteSource, id: &str, name: &str, look: &EmoteLookup) -> String {
    match source {
        EmoteSource::Twitch => twitch_emote_url(id),
        EmoteSource::Kick => kick_emote_url(id),
        EmoteSource::Provider(_) => look.get(name).map(|e| provider_emote_url_at(e, DENSITY)).unwrap_or_default(),
    }
}

fn provider_label(source: EmoteSource) -> &'static str {
    match source {
        EmoteSource::Twitch => "Twitch",
        EmoteSource::Kick => "Kick",
        EmoteSource::Provider(p) => p.label(),
    }
}

fn overlays_json(overlays: &[Overlay], look: &EmoteLookup) -> Value {
    Value::Array(overlays.iter().map(|o| json!({"name": o.name, "url": emote_url(o.source, &o.id, &o.name, look)})).collect())
}

pub fn parts_json(parts: &[MsgPart], look: &EmoteLookup) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|p| match p {
                MsgPart::Text(s) => json!({"t": "text", "s": s}),
                MsgPart::Emote { source, id, name, overlays } => json!({
                    "t": "emote", "name": name, "url": emote_url(*source, id, name, look),
                    "provider": provider_label(*source), "overlays": overlays_json(overlays, look),
                }),
                MsgPart::Emoji { text, overlays } => json!({"t": "emoji", "s": text, "overlays": overlays_json(overlays, look)}),
                MsgPart::Mention { login, name, color, .. } => json!({"t": "mention", "s": format!("@{name}"), "login": login, "color": readable_color(if color.is_empty() { fallback_color(login) } else { color })}),
                MsgPart::Link { href, text } => json!({"t": "link", "s": text, "url": href}),
                MsgPart::Gif { url, alt, .. } => json!({"t": "gif", "url": url, "alt": alt}),
                MsgPart::Cheer { name, amount, color, url, .. } => json!({"t": "cheer", "name": name, "amount": amount, "color": color, "url": url}),
            })
            .collect(),
    )
}

fn text_parts(s: &str) -> Value {
    if s.is_empty() {
        json!([])
    } else {
        json!([{"t": "text", "s": s}])
    }
}

fn user_json(u: &ChatUser) -> Value {
    json!({"id": u.id, "login": u.login, "name": if u.name.is_empty() { &u.login } else { &u.name }, "color": name_color(u, true)})
}

fn plain_user(id: &str, login: &str, name: &str, color: &str) -> Value {
    let c = if color.is_empty() { fallback_color(if login.is_empty() { name } else { login }).to_string() } else { color.to_string() };
    json!({"id": id, "login": login, "name": if name.is_empty() { login } else { name }, "color": readable_color(&c)})
}

fn badges_json(tag: &str, map: &BadgeMap) -> Value {
    Value::Array(parse_badge_list(tag, map).into_iter().filter(|b| !b.url.is_empty()).map(|b| json!({"url": b.url, "title": b.title})).collect())
}

/// One row as the Android list draws it. `kind` is "msg" (a chat line), "notice" (a system line, maybe with the
/// user's message under it), "modfeed" or "automod" (a held message).
pub fn row_json(r: &Row, env: &Env) -> Value {
    let look = EmoteLookup::new(env.emotes.as_deref(), None);
    let mut v = json!({"key": r.key, "sk": r.sk, "platform": r.platform.as_str(), "ts": r.ts, "backlog": r.backlog});
    let o = v.as_object_mut().unwrap();
    let mut put = |k: &str, val: Value| {
        o.insert(k.to_string(), val);
    };
    match &r.body {
        RowBody::Msg(m) => {
            let parts = build_parts_in(&m.text, &m.emotes, look, Some(&env.known), m.gifs.as_deref().unwrap_or(""));
            put("kind", json!("msg"));
            put("id", json!(m.id));
            put("user", user_json(&m.user));
            put("badges", badges_json(&m.badges, &env.badges));
            put("parts", parts_json(&parts, &look));
            put("action", json!(m.action));
            put("firstMsg", json!(m.first_msg));
            put("deleted", json!(m.deleted));
            put("deletedNote", json!(m.deleted_note));
            if let Some(rp) = &m.reply {
                put("reply", json!({"user": rp.user, "text": rp.text, "parentId": rp.parent_id, "threadId": rp.thread_id}));
            }
            if let Some(rw) = &m.reward {
                put("system", json!(format!("Redeemed {}", rw.title)));
            }
            if let Some(b) = m.bits {
                put("bits", json!(b));
            }
        }
        RowBody::Notice(n) => {
            put("kind", json!("notice"));
            put("id", json!(n.id));
            put("system", json!(n.system));
            if !n.text.is_empty() {
                let parts = build_parts_in(&n.text, n.emotes.as_deref().unwrap_or(""), look, Some(&env.known), "");
                put("user", user_json(&n.user));
                put("badges", badges_json(&n.badges, &env.badges));
                put("parts", parts_json(&parts, &look));
            }
            put("deleted", json!(n.deleted));
        }
        RowBody::ModFeed(f) => {
            put("kind", json!("modfeed"));
            put("system", json!(f.text));
        }
        RowBody::Automod(h) => {
            let parts = build_parts_in(&h.text, &h.emotes, look, None, "");
            put("kind", json!("automod"));
            put("id", json!(h.id));
            put("user", plain_user(&h.user.id, &h.user.login, &h.user.name, ""));
            put("parts", parts_json(&parts, &look));
            put("system", json!(format!("Held by AutoMod: {}", h.reason)));
            put("status", json!(h.status));
        }
        RowBody::Kick(k) => {
            let parts = build_kick_parts_in(&k.text, look);
            put("kind", json!(if k.notice.is_some() && k.kicks.is_none() { "notice" } else { "msg" }));
            put("id", json!(k.id));
            put("user", plain_user(&k.user.id, &k.user.login, &k.user.name, &k.user.color));
            put("badges", Value::Array(k.badges.iter().filter(|b| !b.hidden).filter_map(|b| b.url.as_ref().map(|u| json!({"url": u, "title": b.label}))).collect()));
            put("parts", parts_json(&parts, &look));
            put("action", json!(k.action));
            put("deleted", json!(k.deleted));
            if let Some(rp) = &k.reply {
                put("reply", json!({"user": rp.name, "text": rp.text, "parentId": rp.id}));
            }
            if let Some(g) = &k.kicks {
                put("system", json!(format!("{} sent {} KICKs", k.user.name, g.amount)));
            } else if let Some(h) = &k.host {
                put("system", json!(format!("{} raided with {} viewers", k.user.name, h.viewers)));
            }
        }
        RowBody::YouTube(y) => {
            let parts: Vec<Value> = y
                .parts
                .iter()
                .map(|p| match p.t.as_str() {
                    "emoji" | "emote" if !p.u.is_empty() => json!({"t": "emote", "name": p.v, "url": p.u, "provider": "YouTube", "overlays": []}),
                    "link" => json!({"t": "link", "s": p.v, "url": p.u}),
                    _ => json!({"t": "text", "s": p.v}),
                })
                .collect();
            put("kind", json!(if y.notice.is_some() && y.parts.is_empty() { "notice" } else { "msg" }));
            put("id", json!(y.id));
            put("user", plain_user(&y.user.id, "", &y.user.name, ""));
            put("badges", Value::Array(y.badges.iter().filter(|b| !b.url.is_empty()).map(|b| json!({"url": b.url, "title": b.label})).collect()));
            put("parts", if parts.is_empty() { text_parts(&y.text) } else { Value::Array(parts) });
            if let Some(paid) = &y.paid {
                put("system", json!(paid.amount));
            } else if let Some(m) = &y.member {
                put("system", json!(m.header));
            } else if let Some(n) = &y.notice {
                put("system", json!(n));
            }
        }
        RowBody::Rumble(m) => {
            put("kind", json!("msg"));
            put("id", json!(m.id));
            put("user", plain_user(&m.user.id, &m.user.login, &m.user.name, &m.user.color));
            put("parts", text_parts(&m.text));
            put("action", json!(m.action));
            put("deleted", json!(m.deleted));
            if let Some(r) = &m.rant {
                put("system", json!(format!("Rant ${:.2}", r.cents / 100.0)));
            }
        }
        RowBody::X(m) => {
            put("kind", json!("msg"));
            put("id", json!(m.id));
            put("user", plain_user(&m.user.id, &m.user.login, &m.user.name, ""));
            put("parts", text_parts(&m.text));
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_model::store::ChatStore;
    use agu_chat_core::protocol::{Platform, SourceRef};

    #[test]
    fn a_twitch_line_has_its_name_colour_and_twitch_emotes() {
        let src = SourceRef { platform: Platform::Twitch, channel: "a_g_u__".into() };
        let mut st = ChatStore::default();
        st.sync(&[src.clone()]);
        let sk = agu_chat_model::store::source_key(&src);
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "m1", "ts": 1, "user": {"id": "1", "login": "kappa_fan", "name": "Kappa_Fan", "color": "#000000"}, "text": "hi Kappa", "emotes": "25:3-7", "badges": ""}}));
        let row = st.rows().last().expect("a row").clone();
        let v = row_json(&row, &Env::default());
        assert_eq!(v["kind"], "msg");
        assert_eq!(v["user"]["name"], "Kappa_Fan");
        assert_ne!(v["user"]["color"], "#000000", "black names are lifted for the dark list");
        let parts = v["parts"].as_array().unwrap();
        assert!(parts.iter().any(|p| p["t"] == "emote" && p["url"] == twitch_emote_url("25")), "{parts:?}");
    }
}
