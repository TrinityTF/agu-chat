//! What the Android screen draws for one row: the shared chat model's `Row` flattened into plain JSON (names,
//! colours, badge and emote image URLs, message parts, the words of a notice), so the Kotlin side only lays it out.
//! This is the desktop's `gpui/chat/rows.rs` without the layout: the same row kinds, the same wording.
use std::collections::HashMap;
use std::sync::Arc;

use agu_chat_core::link_preview::match_url;
use agu_chat_core::protocol::{AccountsState, HighlightRule, Settings};
use agu_chat_model::chat_text::{as_link, split_rumble_emotes, sub_split, RumblePart};
use agu_chat_model::highlights::{compile_highlights, match_input_of, rule_row_style, Highlighter};
use agu_chat_model::ignores::{apply_ignores, compile_ignores, filter_ignored, CompiledIgnores, IgnoreCache};
use agu_chat_model::mod_row::{row_mod, timeout_ladder, ModEnv};
use agu_chat_model::mentions::{is_own, kick_me, me_login_lower, row_mentions_me};
use agu_chat_model::search::{match_row, SearchQuery};
use agu_chat_model::shown::visible_rows;
use agu_chat_model::twitch::badges::{parse_badge_list, BadgeMap};
use agu_chat_model::twitch::cheermotes::{self, Cheermotes};
use agu_chat_model::twitch::colors::{fallback_color, name_color, readable_color};
use agu_chat_model::twitch::cosmetics::{decode_rgba, kick_key, ChatterStyle, SevenTvDefs, SevenTvPaint, StyleBook, Want};
use agu_chat_model::twitch::emoji::emoji_names;
use agu_chat_model::twitch::emotes::{provider_emote_url_at, provider_tooltip_url, seven_tv_emote, EmoteLookup, EmoteSets, SizeOpt};
use agu_chat_model::twitch::events::{builtin_redeem, notice_id, notice_kind, ChatEventKind};
use agu_chat_model::twitch::parts::{build_kick_parts_in, build_parts_in, split_keep_ws, split_kick, EmoteSource, KnownChatter, MsgPart, Overlay};
use agu_chat_model::twitch::reply_mention::drop_reply_mention;
use agu_chat_model::types::{ChatMsg, ChatUser, Notice, Row, RowBody, RowRef, RumbleBadgeDefs, RumbleEmotes, YouTubeMsg, YouTubePaid};
use serde_json::{json, Value};

/// Image density of emotes and badges: phones draw at 2.6x-3.5x, so the largest size the CDNs offer.
const DENSITY: usize = 3;

/// The display settings the Rust side needs while building a row (the rest are applied by the Kotlin side).
#[derive(Clone)]
pub struct Look {
    pub show_badges: bool,
    pub readable_colors: bool,
    pub cosmetics: bool,
    pub animated_emotes: bool,
    pub font_size: f32,
    pub emote_size: String,
}

impl Default for Look {
    fn default() -> Self {
        Look { show_badges: true, readable_colors: true, cosmetics: true, animated_emotes: true, font_size: 13.0, emote_size: "medium".into() }
    }
}

impl Look {
    pub fn of(s: &Settings) -> Look {
        Look { show_badges: s.show_badges, readable_colors: s.readable_colors, cosmetics: s.seven_tv_cosmetics, animated_emotes: s.animated_emotes, font_size: s.font_size, emote_size: s.emote_size.clone() }
    }

    /// The emote box's height at the desktop's sizes (a multiple of the font size).
    pub fn emote_px(&self) -> f32 {
        let scale = match SizeOpt::parse(&self.emote_size) {
            SizeOpt::Small => 1.45,
            SizeOpt::Medium => 1.9,
            SizeOpt::Large => 2.5,
        };
        (self.font_size * scale).round()
    }
}

/// What a source's rows are drawn against.
#[derive(Clone, Default)]
pub struct Env {
    pub badges: Arc<BadgeMap>,
    pub emotes: Option<Arc<EmoteSets>>,
    pub known: HashMap<String, KnownChatter>,
    pub cheermotes: Option<Arc<Cheermotes>>,
    pub look: Look,
    /// the 7TV paint and badge definitions, and what is known of each chatter's style
    pub defs: Option<Arc<SevenTvDefs>>,
    pub book: Option<Arc<StyleBook>>,
    pub rumble_badges: RumbleBadgeDefs,
    pub rumble_emotes: RumbleEmotes,
    /// what the account may do to this channel's chatters (None: no moderation in this channel)
    pub mods: Option<ModEnv>,
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

/// The popup of an emote: its big picture, its name and where it is from.
fn emote_tip(source: EmoteSource, id: &str, name: &str, look: &EmoteLookup) -> (String, String) {
    match source {
        EmoteSource::Twitch => (twitch_emote_url(id), "Twitch".into()),
        EmoteSource::Kick => (kick_emote_url(id), "Kick".into()),
        EmoteSource::Provider(p) => {
            let big = look.get(name).map(|e| provider_tooltip_url(e)).unwrap_or_default();
            let label = if p == agu_chat_model::twitch::emotes::EmoteProvider::SevenTv && look.is_personal(name) { "7TV Personal".to_string() } else { p.label().to_string() };
            (big, label)
        }
    }
}

pub fn parts_json(parts: &[MsgPart], look: &EmoteLookup) -> Value {
    Value::Array(
        parts
            .iter()
            .map(|p| match p {
                MsgPart::Text(s) => json!({"t": "text", "s": s}),
                MsgPart::Emote { source, id, name, overlays } => {
                    let (tip, label) = emote_tip(*source, id, name, look);
                    json!({
                        "t": "emote", "name": name, "url": emote_url(*source, id, name, look), "tip": tip,
                        "provider": provider_label(*source), "label": label, "overlays": overlays_json(overlays, look),
                    })
                }
                MsgPart::Emoji { text, overlays } => {
                    let (name, short) = emoji_names(text);
                    let label = match short {
                        Some(s) => format!(":{s}: \u{b7} Emoji"),
                        None => "Emoji".to_string(),
                    };
                    json!({"t": "emoji", "s": text, "tipName": name, "tipLabel": label, "overlays": overlays_json(overlays, look)})
                }
                MsgPart::Mention { login, name, color, .. } => json!({"t": "mention", "s": format!("@{name}"), "login": login, "color": readable_color(if color.is_empty() { fallback_color(login) } else { color })}),
                MsgPart::Link { href, text } => json!({"t": "link", "s": text, "url": href}),
                MsgPart::Gif { url, alt, .. } => json!({"t": "gif", "url": url, "alt": alt}),
                MsgPart::Cheer { name, prefix, amount, color, url } => json!({"t": "cheer", "name": name, "label": format!("{prefix} cheermote"), "amount": amount, "color": color, "url": url}),
            })
            .collect(),
    )
}

/// Plain text with its links picked out (Rumble, X, YouTube and a Kick raid's note have no other structure).
fn text_parts(s: &str) -> Value {
    let mut out: Vec<Value> = Vec::new();
    let mut run = String::new();
    for w in split_keep_ws(s) {
        match as_link(w) {
            Some(l) if !w.chars().all(|c| c.is_whitespace()) => {
                run.push_str(&l.lead);
                if !run.is_empty() {
                    out.push(json!({"t": "text", "s": std::mem::take(&mut run)}));
                }
                out.push(json!({"t": "link", "s": l.text, "url": l.href}));
                run.push_str(&l.trail);
            }
            _ => run.push_str(w),
        }
    }
    if !run.is_empty() {
        out.push(json!({"t": "text", "s": run}));
    }
    Value::Array(out)
}

fn hex6(c: (u8, u8, u8)) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

/// A paint colour (`decode_rgba`) as the `#aarrggbb` Android reads.
fn argb(c: u32) -> String {
    let (r, g, b, a) = decode_rgba(c);
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", q(a), q(r), q(g), q(b))
}

/// A CSS colour as YouTube sends it (`rgba(230,81,0,.5)`, `#rrggbbaa`, `#rrggbb`) as `#aarrggbb`.
pub fn css_argb(color: &str) -> Option<String> {
    let t = color.trim();
    if let Some(inner) = t.strip_prefix("rgba(").or_else(|| t.strip_prefix("rgb(")).and_then(|r| r.strip_suffix(')')) {
        let v: Vec<f32> = inner.split([',', '/', ' ']).filter(|p| !p.is_empty()).filter_map(|p| p.trim_end_matches('%').parse().ok()).collect();
        if v.len() >= 3 {
            let a = (v.get(3).copied().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0).round() as u8;
            return Some(format!("#{:02x}{:02x}{:02x}{:02x}", a, v[0] as u8, v[1] as u8, v[2] as u8));
        }
    }
    let h = t.strip_prefix('#')?;
    match h.len() {
        8 => u32::from_str_radix(h, 16).ok().map(|n| format!("#{:02x}{:06x}", n & 0xff, n >> 8)),
        6 => u32::from_str_radix(h, 16).ok().map(|n| format!("#ff{n:06x}")),
        3 => {
            let e: String = h.chars().flat_map(|c| [c, c]).collect();
            u32::from_str_radix(&e, 16).ok().map(|n| format!("#ff{n:06x}"))
        }
        _ => None,
    }
}

/// A 7TV paint for Compose: a gradient (colour stops, an angle), a solid colour, or only a picture (then the name
/// keeps its own colour), and its drop shadows.
pub fn paint_json(p: &SevenTvPaint) -> Value {
    let gradient = (p.function == "LINEAR_GRADIENT" || p.function == "RADIAL_GRADIENT") && !p.stops.is_empty();
    json!({
        "function": p.function,
        "color": p.color.map(argb),
        "angle": p.angle,
        "repeat": p.repeat,
        "shape": p.shape,
        "stops": if gradient { p.stops.iter().map(|s| json!({"at": s.at, "color": argb(s.color)})).collect::<Vec<_>>() } else { vec![] },
        "shadows": p.shadows.iter().map(|s| json!({"x": s.x, "y": s.y, "radius": s.radius, "color": argb(s.color)})).collect::<Vec<_>>(),
        "usable": gradient || p.color.is_some(),
    })
}

/// The key a chatter's 7TV style is looked up by: a Twitch user id, `kick:<id>` for Kick.
pub fn style_key(r: &Row) -> String {
    match &r.body {
        RowBody::Msg(m) => m.user.id.clone(),
        RowBody::Notice(n) if !n.text.is_empty() => n.user.id.clone(),
        RowBody::Kick(k) => kick_key(&k.user.id),
        _ => String::new(),
    }
}

fn style_of(env: &Env, key: &str) -> Option<ChatterStyle> {
    if !env.look.cosmetics || key.is_empty() {
        return None;
    }
    match env.book.as_ref()?.want(key) {
        Want::Known(s) => s,
        Want::Queued | Want::Waiting => None,
    }
}

/// `{"id","login","name","color"}` plus the chatter's 7TV `paint` when they have one.
fn user_json(u: &ChatUser, env: &Env, style: Option<&ChatterStyle>) -> Value {
    let mut v = json!({"id": u.id, "login": u.login, "name": if u.name.is_empty() { &u.login } else { &u.name }, "color": name_color(u, env.look.readable_colors)});
    if let Some(p) = style.and_then(|s| s.paint_id.as_ref()).and_then(|id| env.defs.as_ref().and_then(|d| d.paints.get(id))) {
        let pj = paint_json(p);
        if pj["usable"] == true {
            v["paint"] = pj;
        }
    }
    v
}

fn plain_user(id: &str, login: &str, name: &str, color: &str, readable: bool) -> Value {
    let c = if color.is_empty() { fallback_color(if login.is_empty() { name } else { login }).to_string() } else { color.to_string() };
    json!({"id": id, "login": login, "name": if name.is_empty() { login } else { name }, "color": if readable { readable_color(&c) } else { c }})
}

fn badges_json(tag: &str, env: &Env, style: Option<&ChatterStyle>) -> Value {
    if !env.look.show_badges {
        return json!([]);
    }
    let mut list: Vec<Value> = parse_badge_list(tag, &env.badges).into_iter().filter(|b| !b.url.is_empty()).map(|b| json!({"url": b.url, "title": b.title})).collect();
    if let Some((id, def)) = style.and_then(|s| s.badge_id.clone()).and_then(|id| env.defs.as_ref().and_then(|d| d.badges.get(&id)).map(|b| (id, b.clone()))) {
        list.push(json!({"url": format!("https://cdn.7tv.app/badge/{id}/3x"), "title": if def.tooltip.is_empty() { def.name } else { def.tooltip }}));
    }
    Value::Array(list)
}

/// Kick's own chips for badge types without bundled art.
fn kick_badge_style(t: &str) -> (&'static str, &'static str) {
    match t {
        "broadcaster" => ("\u{25c9}", "#fb2c36"),
        "moderator" => ("\u{2694}", "#00bcff"),
        "verified" => ("\u{2713}", "#51a2ff"),
        "vip" => ("\u{25c6}", "#e12afb"),
        "og" => ("\u{2605}", "#00bc7d"),
        "founder" => ("\u{2691}", "#fe9a00"),
        "subscriber" => ("\u{2605}", "#7ccf00"),
        "sub_gifter" => ("\u{2665}", "#ad46ff"),
        "staff" => ("\u{2699}", "#d4d4d8"),
        "trainee" => ("\u{25d0}", "#00d5be"),
        "bot" => ("\u{232c}", "#9f9fa9"),
        _ => ("", "#ffffff"),
    }
}

fn chip(label: &str, glyph: &str, color: &str) -> Value {
    json!({"title": label, "glyph": glyph, "color": color})
}

fn rumble_badge_icon(def: &Value) -> Option<String> {
    let icons = def.get("icons")?.as_object()?;
    let path = icons.get("96").or_else(|| icons.get("48")).or_else(|| icons.values().next())?.as_str()?;
    Some(if path.starts_with("http") { path.to_string() } else { format!("https://rumble.com{path}") })
}

/// "$5" or "$5.50"
fn money(cents: f64) -> String {
    let d = cents / 100.0;
    if d.fract() == 0.0 {
        format!("${}", d as i64)
    } else {
        format!("${d:.2}")
    }
}

/// A Rumble rant's colour by its size.
fn rant_color(cents: f64) -> &'static str {
    let d = cents / 100.0;
    if d >= 100.0 {
        "#ff2056"
    } else if d >= 50.0 {
        "#ff6900"
    } else if d >= 20.0 {
        "#fe9a00"
    } else if d >= 10.0 {
        "#efb100"
    } else if d >= 5.0 {
        "#7ccf00"
    } else if d >= 2.0 {
        "#00bc7d"
    } else {
        "#00bcff"
    }
}

/// 12,345
pub fn thousands(n: f64) -> String {
    let s = format!("{}", n.abs().round() as i64);
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0.0 {
        format!("-{out}")
    } else {
        out
    }
}

/// "1 viewer", "118 viewers"
fn viewers_text(n: f64) -> String {
    format!("{} {}", thousands(n), if n == 1.0 { "viewer" } else { "viewers" })
}

/// The colour of the bits icon by amount (the tiers of twitch.tv's bits).
fn bits_color(amount: f64) -> &'static str {
    if amount >= 10000.0 {
        "#f43021"
    } else if amount >= 5000.0 {
        "#0099fe"
    } else if amount >= 1000.0 {
        "#1db2a5"
    } else if amount >= 100.0 {
        "#9c3ee8"
    } else {
        "#979797"
    }
}

/// An announcement's own colour.
fn announce_color(c: &str) -> &'static str {
    match c.to_ascii_uppercase().as_str() {
        "BLUE" => "#00d6d5",
        "GREEN" => "#00db84",
        "ORANGE" => "#ffb31a",
        _ => "#9146ff",
    }
}

/// The first link in a row's parts that one of the preview providers can make a card for.
fn link_of(parts: &Value) -> Option<Value> {
    parts.as_array()?.iter().filter(|p| p["t"] == "link").find_map(|p| {
        let url = p["url"].as_str()?;
        match_url(url).map(|t| json!({"href": url, "key": t.key, "provider": t.provider.label()}))
    })
}

/// A header for a redeem or a cheer: who did what, and for how much (the right side of the frame).
fn event_json(verb: &str, reward: Option<String>, amount: Option<f64>, bits: bool) -> Value {
    json!({
        "verb": verb, "reward": reward, "amount": amount, "amountText": amount.map(thousands),
        "bits": bits, "bitsColor": bits_color(amount.unwrap_or(1.0)),
    })
}

fn removed(deleted: bool, note: Option<&String>, default: Option<&str>) -> (bool, Value) {
    (deleted, if deleted { note.map(|n| json!(n)).or_else(|| default.map(|d| json!(d))).unwrap_or(Value::Null) } else { Value::Null })
}

fn twitch_msg_fields(o: &mut serde_json::Map<String, Value>, m: &ChatMsg, env: &Env, look: &EmoteLookup, style: Option<&ChatterStyle>) {
    let built = build_parts_in(&m.text, &m.emotes, *look, Some(&env.known), m.gifs.as_deref().unwrap_or(""));
    let built = match &m.reply {
        Some(r) => drop_reply_mention(built, &r.user),
        None => built,
    };
    // cheermotes only on a message that carries bits
    let parts = match (&env.cheermotes, m.bits.filter(|b| *b != 0.0)) {
        (Some(cm), Some(_)) => cheermotes::apply(built, cm, env.look.animated_emotes, env.look.emote_px()),
        _ => built,
    };
    let pj = parts_json(&parts, look);
    if !m.deleted {
        if let Some(l) = link_of(&pj) {
            o.insert("link".into(), l);
        }
    }
    o.insert("kind".into(), json!("msg"));
    o.insert("id".into(), json!(m.id));
    o.insert("user".into(), user_json(&m.user, env, style));
    o.insert("badges".into(), badges_json(&m.badges, env, style));
    o.insert("parts".into(), pj);
    o.insert("action".into(), json!(m.action));
    o.insert("firstMsg".into(), json!(m.first_msg));
    o.insert("returning".into(), json!(m.returning));
    o.insert("highlighted".into(), json!(m.msg_id.as_deref() == Some("highlighted-message")));
    let (d, note) = removed(m.deleted, m.deleted_note.as_ref(), Some("Deleted"));
    o.insert("deleted".into(), json!(d));
    o.insert("deletedNote".into(), note);
    if let Some(rp) = &m.reply {
        o.insert("reply".into(), json!({"user": rp.user, "text": rp.text, "parentId": rp.parent_id, "threadId": rp.thread_id}));
    }
    if let Some(b) = m.bits {
        o.insert("bits".into(), json!(b));
    }
    // a redeem or a cheer is drawn in its own frame, with the message under the header
    if !m.deleted {
        let builtin = m.msg_id.as_deref().and_then(builtin_redeem);
        let is_bits = m.reward.as_ref().map(|r| r.bits.unwrap_or(0.0) != 0.0).unwrap_or(false) || builtin.map(|b| b.1).unwrap_or(false);
        if m.reward_id.is_some() || builtin.is_some() {
            let title = builtin.map(|b| b.0.to_string()).or_else(|| m.reward.as_ref().map(|r| r.title.clone()));
            let amount = if builtin.is_some() { None } else { m.reward.as_ref().and_then(|r| r.bits.filter(|b| *b != 0.0).or(r.cost)) };
            o.insert("event".into(), event_json("redeemed", title, amount, is_bits));
        } else if let Some(b) = m.bits.filter(|b| *b != 0.0) {
            o.insert("event".into(), event_json("cheered", None, Some(b), true));
        }
    }
}

/// The chat message a Twitch notice's attached text is drawn as (the sender's badges, colour and name).
fn notice_line(n: &Notice) -> ChatMsg {
    ChatMsg {
        id: n.id.clone(),
        ts: n.ts,
        user: ChatUser { name: if n.user.name.is_empty() { n.user.login.clone() } else { n.user.name.clone() }, ..n.user.clone() },
        badges: n.badges.clone(),
        badge_info: n.badge_info.clone(),
        text: n.text.clone(),
        emotes: n.emotes.clone().unwrap_or_default(),
        deleted: n.deleted,
        deleted_note: n.deleted_note.clone(),
        ..Default::default()
    }
}

fn seventv_notice(n: &Notice, put: &mut dyn FnMut(&str, Value)) {
    let s = n.seventv.as_ref().unwrap();
    let url = if s.emote_id.is_empty() { String::new() } else { provider_emote_url_at(&seven_tv_emote(&json!({"id": s.emote_id, "name": s.name})), DENSITY) };
    let verb = match s.action.as_str() {
        "added" => "added the 7TV emote",
        "removed" => "removed the 7TV emote",
        "renamed" => "renamed the 7TV emote",
        _ => "switched the channel's 7TV emote set to",
    };
    put("notice", json!({"seventv": {"actor": s.actor, "verb": verb, "url": url, "name": s.name, "oldName": s.old_name, "action": s.action}}));
}

fn notice_fields(o: &mut serde_json::Map<String, Value>, n: &Notice, env: &Env, look: &EmoteLookup, style: Option<&ChatterStyle>) {
    o.insert("kind".into(), json!("notice"));
    o.insert("id".into(), json!(n.id));
    o.insert("system".into(), json!(n.system));
    let mut put = |k: &str, v: Value| {
        o.insert(k.to_string(), v);
    };
    if n.seventv.is_some() {
        seventv_notice(n, &mut put);
        return;
    }
    // a redeem with no message: the same header as one with a message, nothing under it
    if (n.msg_id == "redeem" || n.msg_id == "powerup") && n.reward.is_some() {
        let r = n.reward.as_ref().unwrap();
        let bits = r.bits.unwrap_or(0.0) != 0.0;
        put("user", user_json(&n.user, env, style));
        put("event", event_json("redeemed", Some(r.title.clone()), if bits { r.bits } else { r.cost }, bits));
        if !n.text.is_empty() {
            put("parts", parts_json(&build_parts_in(&n.text, n.emotes.as_deref().unwrap_or(""), *look, Some(&env.known), ""), look));
        }
        return;
    }
    let kind = notice_kind(&n.msg_id, n.source_msg_id.as_deref());
    let raid = kind == Some(ChatEventKind::Raids);
    let id = notice_id(&n.msg_id, n.source_msg_id.as_deref());
    let mut notice = json!({"raid": raid, "sentence": n.system, "reply": false});
    match (matches!(kind, Some(ChatEventKind::Subs | ChatEventKind::Gifts)), sub_split(&n.system, &n.user)) {
        (true, Some((rest, bold))) => {
            // like 7TV: an icon, the name on its own line, then Twitch's sentence without it
            let mystery = id.ends_with("submysterygift");
            notice["icon"] = json!(match kind {
                Some(ChatEventKind::Gifts) => "gift",
                _ if n.system.contains("with Prime") => "crown",
                _ => "star",
            });
            notice["iconSize"] = json!(if mystery { 24 } else { 16 });
            notice["head"] = json!(if n.user.name.is_empty() { &n.user.login } else { &n.user.name });
            notice["headColor"] = json!(if mystery { "#e4e4e7".to_string() } else { name_color(&n.user, env.look.readable_colors) });
            notice["sentence"] = json!(rest);
            notice["bold"] = json!([bold.0, bold.1]);
            notice["reply"] = json!(true);
        }
        _ => {
            if kind == Some(ChatEventKind::Announcements) {
                notice["bar"] = json!(argb_of_hex(announce_color(n.param_color.as_deref().unwrap_or(""))));
                notice["announce"] = json!(true);
            }
        }
    }
    put("notice", notice);
    if !n.text.is_empty() {
        // the attached message is the sender's chat line under the sentence
        let line = notice_line(n);
        let parts = parts_json(&build_parts_in(&line.text, &line.emotes, *look, Some(&env.known), ""), look);
        if !n.deleted {
            if let Some(l) = link_of(&parts) {
                put("link", l);
            }
        }
        put("user", user_json(&line.user, env, style));
        put("badges", badges_json(&line.badges, env, style));
        put("parts", parts);
        let (d, note) = removed(n.deleted, n.deleted_note.as_ref(), Some("Deleted"));
        put("deleted", json!(d));
        put("deletedNote", note);
    }
}

fn argb_of_hex(h: &str) -> String {
    format!("#ff{}", h.trim_start_matches('#'))
}

/// A YouTube line's parts: emoji pictures, links picked out of the text.
fn youtube_parts(y: &YouTubeMsg) -> Value {
    let parts: Vec<Value> = y
        .parts
        .iter()
        .flat_map(|p| -> Vec<Value> {
            if p.t == "emoji" && !p.u.is_empty() {
                let label = if p.custom { "YouTube member emote" } else { "YouTube" };
                let name = if p.a.is_empty() { p.v.clone() } else { p.a.clone() };
                vec![json!({"t": "emote", "name": name, "url": p.u, "tip": p.u, "provider": "YouTube", "label": label, "overlays": []})]
            } else {
                text_parts(&p.v).as_array().cloned().unwrap_or_default()
            }
        })
        .collect();
    if parts.is_empty() {
        text_parts(&y.text)
    } else {
        Value::Array(parts)
    }
}

fn youtube_badges(y: &YouTubeMsg, show: bool) -> Value {
    if !show {
        return json!([]);
    }
    Value::Array(
        y.badges
            .iter()
            .map(|b| {
                if !b.url.is_empty() {
                    return json!({"url": b.url, "title": b.label});
                }
                let (glyph, color) = match b.icon.as_str() {
                    "OWNER" => ("\u{25c9}", "#fe9a00"),
                    "MODERATOR" => ("\u{2694}", "#00bcff"),
                    "VERIFIED" => ("\u{2713}", "#e4e4e7"),
                    _ => ("", "#a1a1aa"),
                };
                let g: String = if glyph.is_empty() { b.label.chars().chain(b.icon.chars()).take(3).collect() } else { glyph.to_string() };
                chip(&b.label, &g, color)
            })
            .collect(),
    )
}

fn youtube_fields(o: &mut serde_json::Map<String, Value>, y: &YouTubeMsg, env: &Env) {
    let name = if y.user.name.is_empty() { "unknown".to_string() } else { y.user.name.clone() };
    let user = plain_user(&y.user.id, "", &name, "", false);
    let parts = youtube_parts(y);
    let paid: Option<&YouTubePaid> = y.paid.as_ref().filter(|p| !p.amount.is_empty());
    o.insert("id".into(), json!(y.id));
    o.insert("user".into(), user.clone());
    if let Some(p) = paid {
        // a Super Chat / Super Sticker, drawn like a sub: the name, "Sent a Super Chat for $5", the message, a Reply
        // button; the left bar takes the Super Chat's own tier colour
        let colour = [&p.bg, &p.header_bg].into_iter().find(|c| !c.is_empty()).and_then(|c| css_argb(c));
        o.insert("kind".into(), json!("notice"));
        o.insert(
            "notice".into(),
            json!({
                "icon": "coins", "iconSize": 16, "head": name, "headColor": "#e4e4e7", "sentence": format!("{} {}", if p.sticker { "Sent a Super Sticker for" } else { "Sent a Super Chat for" }, p.amount),
                "bold": [if p.sticker { 25 } else { 22 }, if p.sticker { 25 + p.amount.len() } else { 22 + p.amount.len() }],
                "bar": colour, "reply": true, "raid": false, "superChat": true,
            }),
        );
        o.insert("system".into(), json!(p.amount));
        if parts.as_array().is_some_and(|a| !a.is_empty()) {
            o.insert("parts".into(), parts.clone());
        }
        return;
    }
    if y.notice.as_deref() == Some("membership") {
        let member = y.member.clone().unwrap_or_default();
        let has_message = y.member.is_some() && member.has_message && !y.parts.is_empty();
        let sub = if y.member.is_some() { member.sub.clone() } else { y.text.clone() };
        let (sentence, bold) = if !member.header.is_empty() {
            let s = if sub.is_empty() { member.header.clone() } else { format!("{} \u{b7} {sub}", member.header) };
            (s, [0, member.header.len()])
        } else {
            let s = if sub.is_empty() { "New member".to_string() } else { sub };
            let n = s.len();
            (s, [0, n])
        };
        o.insert("kind".into(), json!("notice"));
        o.insert("notice".into(), json!({"icon": "star", "iconSize": 16, "head": name, "headColor": "#2ba640", "sentence": sentence, "bold": bold, "reply": true, "raid": false, "member": true}));
        o.insert("system".into(), json!(sentence));
        if has_message {
            o.insert("badges".into(), youtube_badges(y, env.look.show_badges));
            o.insert("parts".into(), parts);
        }
        return;
    }
    o.insert("kind".into(), json!(if y.notice.is_some() && y.parts.is_empty() { "notice" } else { "msg" }));
    o.insert("badges".into(), youtube_badges(y, env.look.show_badges));
    if let Some(l) = link_of(&parts) {
        o.insert("link".into(), l);
    }
    o.insert("parts".into(), parts);
    if let Some(n) = &y.notice {
        o.insert("system".into(), json!(n));
    }
}

fn kick_fields(o: &mut serde_json::Map<String, Value>, k: &agu_chat_model::types::KickMsg, env: &Env, look: &EmoteLookup, style: Option<&ChatterStyle>) {
    let name = if k.user.name.is_empty() { k.user.login.clone() } else { k.user.name.clone() };
    o.insert("id".into(), json!(k.id));
    let mut user = plain_user(&k.user.id, &k.user.login, &k.user.name, &k.user.color, env.look.readable_colors);
    if let Some(p) = style.and_then(|s| s.paint_id.as_ref()).and_then(|id| env.defs.as_ref().and_then(|d| d.paints.get(id))) {
        let pj = paint_json(p);
        if pj["usable"] == true {
            user["paint"] = pj;
        }
    }
    o.insert("user".into(), user);
    if let Some(h) = &k.host {
        // a raid, like Twitch's raid notice: "<name> has raided with 118 viewers."
        o.insert("kind".into(), json!("notice"));
        let sentence = format!("{name} has raided with {}.", viewers_text(h.viewers.max(0.0)));
        o.insert("system".into(), json!(sentence));
        o.insert("notice".into(), json!({"raid": true, "sentence": sentence, "reply": false}));
        if !h.message.is_empty() {
            o.insert("parts".into(), text_parts(&h.message));
        }
        return;
    }
    let parts = Value::Array(
        build_kick_parts_in(&k.text, *look)
            .iter()
            .flat_map(|p| match p {
                MsgPart::Text(t) => text_parts(t).as_array().cloned().unwrap_or_default(),
                _ => parts_json(std::slice::from_ref(p), look).as_array().cloned().unwrap_or_default(),
            })
            .collect(),
    );
    o.insert("kind".into(), json!("msg"));
    let mut badges: Vec<Value> = Vec::new();
    if env.look.show_badges {
        // what kick.com's chat draws: the user's selected badges, at most four, in Kick's order
        for b in k.badges.iter().filter(|b| !b.hidden) {
            let label = if b.label.is_empty() { agu_chat_core::kick::badges::badge_label(&b.r#type, &b.text, b.count) } else { b.label.clone() };
            if let Some(u) = agu_chat_core::kick::badges::badge_url(&b.r#type, b.count, b.url.as_deref()) {
                badges.push(json!({"url": u, "title": label}));
            } else {
                let (glyph, col) = kick_badge_style(&b.r#type);
                let g: String = if glyph.is_empty() { b.text.chars().chain(b.r#type.chars()).take(3).collect() } else { glyph.to_string() };
                badges.push(chip(&label, &g, col));
            }
        }
        if let Some((id, def)) = style.and_then(|s| s.badge_id.clone()).and_then(|id| env.defs.as_ref().and_then(|d| d.badges.get(&id)).map(|b| (id, b.clone()))) {
            badges.push(json!({"url": format!("https://cdn.7tv.app/badge/{id}/3x"), "title": if def.tooltip.is_empty() { def.name } else { def.tooltip }}));
        }
    }
    o.insert("badges".into(), Value::Array(badges));
    if !k.deleted {
        if let Some(l) = link_of(&parts) {
            o.insert("link".into(), l);
        }
    }
    o.insert("parts".into(), parts);
    o.insert("action".into(), json!(k.action));
    o.insert("deleted".into(), json!(k.deleted));
    if let Some(rp) = &k.reply {
        let text: String = split_kick(&rp.text).into_iter().map(|(emote, a, nm)| if emote { nm } else { a }).collect();
        o.insert("reply".into(), json!({"user": rp.name, "text": text, "parentId": rp.id}));
    }
    // a KICKs gift, in the frame of a Twitch cheer, with the sender's message under it
    if let Some(g) = &k.kicks {
        let mut ev = event_json("sent", if g.name.is_empty() { None } else { Some(g.name.clone()) }, Some(g.amount), false);
        ev["kicks"] = json!(true);
        ev["image"] = json!(g.image.clone().filter(|u| !u.is_empty()));
        o.insert("event".into(), ev);
    }
}

/// One row as the Android list draws it. `kind` is "msg" (a chat line), "notice" (a system line, maybe with the
/// user's message under it), "modfeed" or "automod" (a held message).
pub fn row_json(r: &Row, env: &Env) -> Value {
    let look = EmoteLookup::new(env.emotes.as_deref(), None);
    let mut v = json!({"key": r.key, "sk": r.sk, "platform": r.platform.as_str(), "ts": r.ts, "ord": r.ord, "backlog": r.backlog});
    let o = v.as_object_mut().unwrap();
    let key = style_key(r);
    let style = style_of(env, &key);
    match &r.body {
        RowBody::Msg(m) => twitch_msg_fields(o, m, env, &look, style.as_ref()),
        RowBody::Notice(n) => notice_fields(o, n, env, &look, style.as_ref()),
        RowBody::ModFeed(f) => {
            o.insert("kind".into(), json!("modfeed"));
            o.insert("system".into(), json!(f.text));
        }
        RowBody::Automod(h) => {
            let parts = build_parts_in(&h.text, &h.emotes, look, None, "");
            o.insert("kind".into(), json!("automod"));
            o.insert("id".into(), json!(h.id));
            o.insert("user".into(), plain_user(&h.user.id, &h.user.login, &h.user.name, "", env.look.readable_colors));
            o.insert("parts".into(), parts_json(&parts, &look));
            o.insert("system".into(), json!("AutoMod held a message"));
            o.insert("status".into(), json!(h.status));
            let chip_text = if h.reason == "blocked_term" {
                "Blocked term".to_string()
            } else if let Some(c) = &h.category {
                format!("{c}{}", h.level.map(|l| format!(" (level {})", l as i64)).unwrap_or_default())
            } else {
                "AutoMod".to_string()
            };
            o.insert("chip".into(), json!(chip_text));
            let by = h.resolved_by.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| "?".into());
            o.insert(
                "resolved".into(),
                json!(match h.status.as_str() {
                    "held" => Value::Null,
                    "approved" => json!(format!("Allowed by {by}")),
                    "denied" => json!(format!("Denied by {by}")),
                    _ => json!("Expired (no action taken)"),
                }),
            );
        }
        RowBody::Kick(k) => kick_fields(o, k, env, &look, style.as_ref()),
        RowBody::YouTube(y) => youtube_fields(o, y, env),
        RowBody::Rumble(m) => {
            o.insert("kind".into(), json!("msg"));
            o.insert("id".into(), json!(m.id));
            let mut user = plain_user(&m.user.id, &m.user.login, &m.user.name, &m.user.color, env.look.readable_colors);
            if let Some(ch) = &m.channel {
                if !m.user.login.is_empty() && ch.name != m.user.login {
                    user["login"] = json!(m.user.login);
                    user["alias"] = json!(format!("({})", m.user.login));
                }
            }
            o.insert("user".into(), user);
            let mut badges: Vec<Value> = Vec::new();
            if env.look.show_badges {
                for id in &m.badges {
                    if let Some(def) = env.rumble_badges.get(id) {
                        if let Some(url) = rumble_badge_icon(def) {
                            badges.push(json!({"url": url, "title": def.pointer("/label/en").and_then(|t| t.as_str()).unwrap_or(id)}));
                        }
                    }
                }
            }
            o.insert("badges".into(), Value::Array(badges));
            // :name: tokens resolved against the chat's own emote list
            let mut parts: Vec<Value> = Vec::new();
            for p in split_rumble_emotes(&m.text, &env.rumble_emotes) {
                match p {
                    RumblePart::Text(t) => parts.extend(text_parts(t).as_array().cloned().unwrap_or_default()),
                    RumblePart::Emote { name, file } => parts.push(json!({"t": "emote", "name": format!(":{name}:"), "url": file, "tip": file, "provider": "Rumble", "label": "Rumble", "overlays": []})),
                }
            }
            let parts = Value::Array(parts);
            if !m.deleted {
                if let Some(l) = link_of(&parts) {
                    o.insert("link".into(), l);
                }
            }
            o.insert("parts".into(), parts);
            o.insert("action".into(), json!(m.action));
            o.insert("deleted".into(), json!(m.deleted));
            o.insert("firstMsg".into(), json!(m.first_msg));
            if let Some(rant) = m.rant.as_ref().filter(|r| r.cents > 0.0) {
                o.insert("rant".into(), json!({"text": money(rant.cents), "color": rant_color(rant.cents)}));
            }
        }
        RowBody::X(m) => {
            o.insert("kind".into(), json!("msg"));
            o.insert("id".into(), json!(m.id));
            o.insert("user".into(), plain_user(&m.user.id, &m.user.login, &m.user.name, "", false));
            let parts = text_parts(&m.text);
            if let Some(l) = link_of(&parts) {
                o.insert("link".into(), l);
            }
            o.insert("parts".into(), parts);
        }
    }
    // the picture on the user card of a platform that has no badge lookup of its own
    let avatar = match &r.body {
        RowBody::Rumble(m) => m.user.avatar.as_str(),
        RowBody::Kick(m) => m.user.avatar.as_str(),
        RowBody::YouTube(m) => m.user.avatar.as_str(),
        RowBody::X(m) => m.user.avatar.as_str(),
        _ => "",
    };
    if !avatar.is_empty() {
        if let Some(u) = o.get_mut("user").and_then(|u| u.as_object_mut()) {
            u.insert("avatar".into(), json!(avatar));
        }
    }
    if let Some(me) = &env.mods {
        if let Some(m) = row_mod(r, me) {
            let t = &m.target;
            o.insert("mod".into(), json!({"t": {"id": t.id, "uid": t.user_id, "login": t.login, "name": t.name, "menu": t.menu}, "ban": m.ban, "timeout": m.timeout, "warn": m.warn, "delete": m.delete, "unban": m.unban, "cmd": m.cmd, "ladder": timeout_ladder(r.platform).iter().map(|(l, s)| json!([l, s])).collect::<Vec<_>>()}));
        }
        if matches!(r.body, RowBody::Automod(_)) {
            o.insert("canResolve".into(), json!(me.twitch.automod));
        }
    }
    v
}

/// The user's highlight rules, ignores and account names, compiled: what decides how a row is shown.
pub struct Rules {
    pub settings: Settings,
    pub accounts: AccountsState,
    highlighter: Highlighter,
    ignores: CompiledIgnores,
    me_login: String,
    kick_me: Vec<String>,
}

impl Default for Rules {
    fn default() -> Self {
        Rules::new(Settings::default(), AccountsState::new())
    }
}

impl Rules {
    pub fn new(settings: Settings, accounts: AccountsState) -> Rules {
        Rules {
            highlighter: compile_highlights(&settings.highlight_rules),
            ignores: compile_ignores(&settings.ignored_users, &settings.ignored_phrases),
            me_login: me_login_lower(&accounts),
            kick_me: kick_me(&accounts),
            settings,
            accounts,
        }
    }

    pub fn me_login(&self) -> &str {
        &self.me_login
    }

    pub fn kick_me(&self) -> &[String] {
        &self.kick_me
    }

    pub fn is_own(&self, r: &Row) -> bool {
        is_own(r, &self.accounts)
    }

    /// The first rule a line matches (never your own lines).
    pub fn rule_of(&self, r: &Row) -> Option<HighlightRule> {
        if self.highlighter.size() == 0 || self.is_own(r) {
            return None;
        }
        match_input_of(r).and_then(|i| self.highlighter.matches(&i).cloned())
    }

    pub fn hidden(&self, r: &RowRef) -> bool {
        apply_ignores(r, &self.ignores).is_none()
    }

    /// What a list shows of a store's rows: the event toggles, then ignores (hidden lines go, words are rewritten),
    /// then the tab's search.
    pub fn shown(&self, rows: &[RowRef], cache: &mut IgnoreCache, search: Option<&SearchQuery>) -> Vec<RowRef> {
        let rows = filter_ignored(&visible_rows(rows, &self.settings), &self.ignores, cache);
        match search {
            Some(q) if !(q.from.is_empty() && q.words.is_empty()) => rows.into_iter().filter(|r| match_row(r, q)).collect(),
            _ => rows,
        }
    }

    pub fn look(&self) -> Look {
        Look::of(&self.settings)
    }

    /// The row's highlight fields: `highlight: {color, alpha}` for a matched rule, `mention: true` for a line that
    /// names you (drawn red, ahead of a rule, like the desktop).
    pub fn decorate(&self, v: &mut Value, r: &Row) {
        let o = v.as_object_mut().unwrap();
        if row_mentions_me(r, &self.me_login, &self.kick_me) {
            o.insert("mention".into(), json!(true));
        }
        if let Some(rule) = self.rule_of(r) {
            let s = rule_row_style(&rule.color);
            o.insert("highlight".into(), json!({"color": hex6(s.bar), "alpha": s.background.3}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::{Platform, SourceRef};
    use agu_chat_model::search::parse_search;
    use agu_chat_model::store::ChatStore;

    fn store_for(platform: Platform, channel: &str) -> (ChatStore, String) {
        let src = SourceRef { platform, channel: channel.into() };
        let mut st = ChatStore::default();
        st.sync(&[src.clone()]);
        let sk = agu_chat_model::store::source_key(&src);
        (st, sk)
    }

    fn json_of(st: &ChatStore, env: &Env) -> Value {
        row_json(st.rows().last().expect("a row"), env)
    }

    fn parts_text(v: &Value) -> String {
        v["parts"].as_array().unwrap().iter().filter_map(|p| p["s"].as_str()).collect()
    }

    #[test]
    fn a_twitch_line_has_its_name_colour_and_twitch_emotes() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "m1", "ts": 1, "user": {"id": "1", "login": "kappa_fan", "name": "Kappa_Fan", "color": "#000000"}, "text": "hi Kappa", "emotes": "25:3-7", "badges": ""}}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["kind"], "msg");
        assert_eq!(v["user"]["name"], "Kappa_Fan");
        assert_ne!(v["user"]["color"], "#000000", "black names are lifted for the dark list");
        let parts = v["parts"].as_array().unwrap();
        let e = parts.iter().find(|p| p["t"] == "emote").expect("an emote");
        assert_eq!(e["url"], twitch_emote_url("25"));
        assert_eq!(e["label"], "Twitch");
        assert_eq!(e["name"], "Kappa");
    }

    fn twitch_row(id: &str, login: &str, text: &str) -> RowRef {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": id, "ts": 1, "user": {"id": "9", "login": login, "name": login, "color": ""}, "text": text, "emotes": "", "badges": ""}}));
        st.rows().last().expect("a row").clone()
    }

    fn rules(patch: Value, me: &str) -> Rules {
        let mut s: Settings = serde_json::from_value(patch).unwrap();
        s.highlight_sound_focused = true;
        let mut acc = AccountsState::new();
        if !me.is_empty() {
            acc.insert(Platform::Twitch, serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": me, "name": me}})).unwrap());
        }
        Rules::new(s, acc)
    }

    fn flagged(rules: &Rules, r: &RowRef) -> Value {
        let mut v = row_json(r, &Env::default());
        rules.decorate(&mut v, r);
        v
    }

    #[test]
    fn a_matching_rule_gives_the_row_its_colour() {
        let rl = rules(json!({"highlightRules": [{"id": "r", "enabled": true, "kind": "phrase", "pattern": "gg", "caseSensitive": false, "color": "#ff9f1a", "sound": true, "soundFile": "", "flash": false}]}), "");
        let hit = flagged(&rl, &twitch_row("1", "bob", "well played gg"));
        assert_eq!(hit["highlight"]["color"], "#ff9f1a");
        assert!(hit["highlight"]["alpha"].as_f64().unwrap() > 0.0);
        let miss = flagged(&rl, &twitch_row("2", "bob", "hello"));
        assert!(miss.get("highlight").is_none() && miss.get("mention").is_none());
    }

    #[test]
    fn your_twitch_name_marks_the_row_a_mention_but_your_own_lines_get_no_rule() {
        let rl = rules(json!({"highlightRules": [{"id": "r", "enabled": true, "kind": "phrase", "pattern": "hi", "caseSensitive": false, "color": "#2bd4a4", "sound": false, "soundFile": "", "flash": false}]}), "a_g_u__");
        let v = flagged(&rl, &twitch_row("1", "bob", "hi @A_G_U__"));
        assert_eq!(v["mention"], true);
        let own = flagged(&rl, &twitch_row("2", "a_g_u__", "hi chat"));
        assert!(own.get("highlight").is_none(), "no rule on your own line");
    }

    #[test]
    fn ignored_users_vanish_and_ignored_words_are_rewritten() {
        let rl = rules(json!({"ignoredUsers": [{"platform": "twitch", "login": "spammer"}], "ignoredPhrases": [{"pattern": "darn", "regex": false, "caseSensitive": false, "action": "replace", "replacement": "***"}]}), "");
        let rows = vec![twitch_row("1", "spammer", "buy followers"), twitch_row("2", "bob", "well darn it"), twitch_row("3", "bob", "fine")];
        let shown = rl.shown(&rows, &mut IgnoreCache::default(), None);
        assert_eq!(shown.len(), 2, "the ignored user's line is hidden");
        let v = row_json(&shown[0], &Env::default());
        assert_eq!(parts_text(&v), "well *** it");
    }

    #[test]
    fn a_search_keeps_the_matching_lines_and_from_picks_one_person() {
        let rl = Rules::default();
        let rows = vec![twitch_row("1", "alice", "good morning"), twitch_row("2", "bob", "good night"), twitch_row("3", "alice", "bye")];
        let q = parse_search("good");
        assert_eq!(rl.shown(&rows, &mut IgnoreCache::default(), Some(&q)).len(), 2);
        let q = parse_search("from:alice");
        assert_eq!(rl.shown(&rows, &mut IgnoreCache::default(), Some(&q)).len(), 2);
        let q = parse_search("from:alice bye");
        assert_eq!(rl.shown(&rows, &mut IgnoreCache::default(), Some(&q)).len(), 1);
        assert_eq!(rl.shown(&rows, &mut IgnoreCache::default(), Some(&parse_search("  "))).len(), 3);
    }

    fn notice_frame(st: &mut ChatStore, sk: &str, n: Value) {
        st.apply(sk, &json!({"type": "chatnotice", "notice": n}));
    }

    #[test]
    fn a_sub_notice_has_its_icon_name_and_bold_word_and_a_reply_button() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        notice_frame(&mut st, &sk, json!({"id": "n1", "ts": 1, "system": "Popo_Master subscribed with Prime.", "msgId": "sub", "user": {"id": "5", "login": "popo_master", "name": "Popo_Master", "color": "#ff0000"}, "text": "", "emotes": "", "badges": ""}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["kind"], "notice");
        let n = &v["notice"];
        assert_eq!(n["icon"], "crown", "Prime");
        assert_eq!(n["head"], "Popo_Master");
        assert_eq!(n["sentence"], "Subscribed with Prime.");
        assert_eq!(n["bold"], json!([0, 10]));
        assert_eq!(n["reply"], true);
        assert!(v.get("parts").is_none());
    }

    #[test]
    fn a_gift_says_who_gave_and_who_got_it_and_a_bomb_has_the_big_gift() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        notice_frame(&mut st, &sk, json!({"id": "g1", "ts": 1, "system": "KnubbeTV gifted a Tier 1 sub to necix7135i!", "msgId": "subgift", "user": {"id": "5", "login": "knubbetv", "name": "KnubbeTV"}, "text": "", "emotes": "", "badges": ""}));
        let v = json_of(&st, &Env::default());
        let n = &v["notice"];
        assert_eq!((n["icon"].as_str(), n["head"].as_str()), (Some("gift"), Some("KnubbeTV")));
        let s = n["sentence"].as_str().unwrap();
        let b = n["bold"].as_array().unwrap();
        assert_eq!(&s[b[0].as_u64().unwrap() as usize..b[1].as_u64().unwrap() as usize], "necix7135i");
        notice_frame(&mut st, &sk, json!({"id": "g2", "ts": 2, "system": "KnubbeTV is gifting 5 Tier 1 Subs to EsfandTV's community!", "msgId": "submysterygift", "giftId": "c1", "user": {"id": "5", "login": "knubbetv", "name": "KnubbeTV"}, "text": "", "emotes": "", "badges": ""}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["notice"]["iconSize"], 24);
        assert_eq!(v["notice"]["headColor"], "#e4e4e7", "a bomb's name is not coloured");
    }

    #[test]
    fn a_raid_is_orange_and_a_message_with_a_sub_is_the_senders_chat_line() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        notice_frame(&mut st, &sk, json!({"id": "r1", "ts": 1, "system": "20 raiders from Foo have joined!", "msgId": "raid", "user": {"id": "5", "login": "foo", "name": "Foo"}, "text": "", "emotes": "", "badges": ""}));
        assert_eq!(json_of(&st, &Env::default())["notice"]["raid"], true);
        notice_frame(&mut st, &sk, json!({"id": "s1", "ts": 2, "system": "Bob subscribed at Tier 1. They've subscribed for 3 months!", "msgId": "resub", "user": {"id": "6", "login": "bob", "name": "Bob", "color": "#00ff00"}, "text": "thanks https://youtu.be/dQw4w9WgXcQ", "emotes": "", "badges": ""}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["user"]["name"], "Bob");
        assert_eq!(v["notice"]["icon"], "star");
        assert_eq!(v["link"]["key"], "youtube:dQw4w9WgXcQ", "a link in the attached message gets a card");
        assert!(parts_text(&v).starts_with("thanks "));
    }

    #[test]
    fn an_announcement_carries_its_colour() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        notice_frame(&mut st, &sk, json!({"id": "a1", "ts": 1, "system": "", "msgId": "announcement", "paramColor": "GREEN", "user": {"id": "5", "login": "mod", "name": "Mod"}, "text": "Stream starts soon", "emotes": "", "badges": "moderator/1"}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["notice"]["announce"], true);
        assert_eq!(v["notice"]["bar"], "#ff00db84");
    }

    #[test]
    fn bits_and_redeems_get_their_header_and_cheermotes_use_the_tier_image() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "b1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "Cheer100 nice", "emotes": "", "badges": "", "bits": 100}}));
        let cm = agu_chat_model::twitch::cheermotes::parse_cheermotes(&json!({"data": [{"prefix": "Cheer", "tiers": [{"min_bits": 1, "color": "#979797", "images": {"dark": {"animated": {"2": "https://x/animated/1/2"}, "static": {"2": "https://x/static/1/2"}}}}, {"min_bits": 100, "color": "#9c3ee8", "images": {"dark": {"animated": {"2": "https://x/animated/100/2"}, "static": {"2": "https://x/static/100/2"}}}}]}]}));
        let env = Env { cheermotes: Some(Arc::new(cm)), ..Default::default() };
        let v = json_of(&st, &env);
        assert_eq!(v["event"]["verb"], "cheered");
        assert_eq!(v["event"]["amountText"], "100");
        assert_eq!(v["event"]["bits"], true);
        let c = v["parts"].as_array().unwrap().iter().find(|p| p["t"] == "cheer").expect("a cheermote");
        assert_eq!(c["amount"], 100);
        assert_eq!(c["color"], "#9c3ee8");
        assert!(c["url"].as_str().unwrap().contains("/100/"));
        // a redeem with a message
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "d1", "ts": 2, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": "", "msgId": "highlighted-message"}}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["event"]["reward"], "Highlight My Message");
        assert_eq!(v["highlighted"], true);
    }

    #[test]
    fn first_time_and_returning_chatters_are_flagged() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "f1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": "", "firstMsg": true}}));
        assert_eq!(json_of(&st, &Env::default())["firstMsg"], true);
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "f2", "ts": 2, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": "", "returning": true}}));
        assert_eq!(json_of(&st, &Env::default())["returning"], true);
    }

    #[test]
    fn a_removed_line_keeps_its_text_and_says_why() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "x1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "rude words", "emotes": "", "badges": ""}}));
        st.apply(&sk, &json!({"type": "clearchat", "userId": "9", "login": "bob", "duration": 600}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["deleted"], true);
        assert_eq!(v["deletedNote"], "Timed out (10m)");
        assert_eq!(parts_text(&v), "rude words");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "x2", "ts": 2, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "more", "emotes": "", "badges": ""}}));
        st.apply(&sk, &json!({"type": "clearmsg", "id": "x2"}));
        assert_eq!(json_of(&st, &Env::default())["deletedNote"], "Deleted");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "x3", "ts": 3, "user": {"id": "10", "login": "ann", "name": "Ann", "color": ""}, "text": "all gone", "emotes": "", "badges": ""}}));
        st.apply(&sk, &json!({"type": "clearchat"}));
        assert_eq!(json_of(&st, &Env::default())["deletedNote"], "Chat cleared");
    }

    #[test]
    fn a_painted_name_carries_its_gradient_and_the_7tv_badge() {
        let defs = agu_chat_model::twitch::cosmetics::defs_from_raw(&json!({
            "paints": [{"id": "p1", "name": "Fire", "function": "LINEAR_GRADIENT", "color": null, "angle": 90, "repeat": false, "image_url": null,
                         "stops": [{"at": 0, "color": -16776961}, {"at": 1, "color": -65281}], "shadows": [{"x_offset": 1, "y_offset": 2, "radius": 3, "color": 255}]},
                        {"id": "p2", "name": "Pic", "function": "URL", "color": null, "angle": 0, "repeat": false, "image_url": "https://x/y.png", "stops": []}],
            "badges": [{"id": "b1", "name": "Dev", "tooltip": "7TV Dev"}]
        }));
        let book = Arc::new(StyleBook::default());
        let env = Env { defs: Some(Arc::new(defs)), book: Some(book.clone()), ..Default::default() };
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "p1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": ""}}));
        assert!(json_of(&st, &env)["user"].get("paint").is_none(), "unknown until 7TV answers");
        assert_eq!(book.next_job().as_deref(), Some("9"), "the chatter was asked for");
        book.finish("9", Some(Some(ChatterStyle { paint_id: Some("p1".into()), badge_id: Some("b1".into()), ..Default::default() })));
        let v = json_of(&st, &env);
        let p = &v["user"]["paint"];
        assert_eq!(p["function"], "LINEAR_GRADIENT");
        assert_eq!(p["stops"].as_array().unwrap().len(), 2);
        assert_eq!(p["stops"][0]["color"], "#ffff0000", "7TV's signed rgba as #aarrggbb");
        assert_eq!(p["shadows"][0]["radius"], 3.0);
        let badges = v["badges"].as_array().unwrap();
        assert_eq!(badges.last().unwrap()["title"], "7TV Dev");
        // a picture-only paint keeps the plain colour; the setting off draws no paint at all
        book.finish("8", Some(Some(ChatterStyle { paint_id: Some("p2".into()), ..Default::default() })));
        let off = Env { look: Look { cosmetics: false, ..Look::default() }, ..env.clone() };
        assert!(json_of(&st, &off)["user"].get("paint").is_none());
    }

    #[test]
    fn emotes_carry_their_tooltip_zero_width_overlays_and_emoji_names() {
        let mut sets = EmoteSets::default();
        let mk = |id: &str, name: &str, zw: bool| agu_chat_model::twitch::emotes::seven_tv_emote(&json!({"id": id, "name": name, "flags": if zw { 1 } else { 0 }, "data": {"id": id, "name": name, "flags": if zw { 1 } else { 0 }, "animated": true, "host": {"url": format!("//cdn.7tv.app/emote/{id}")}}}));
        sets.all.insert("Wide".into(), mk("A", "Wide", false));
        sets.all.insert("Hat".into(), mk("B", "Hat", true));
        let env = Env { emotes: Some(Arc::new(sets)), ..Default::default() };
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "e1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "Wide Hat \u{1F600}", "emotes": "", "badges": ""}}));
        let v = json_of(&st, &env);
        let parts = v["parts"].as_array().unwrap();
        let e = parts.iter().find(|p| p["t"] == "emote").unwrap();
        assert_eq!(e["label"], "7TV");
        assert_eq!(e["overlays"].as_array().unwrap().len(), 1, "the zero-width emote stacks on the one before it");
        assert!(e["tip"].as_str().unwrap().contains("cdn.7tv.app/emote/A"));
        let emoji = parts.iter().find(|p| p["t"] == "emoji").unwrap();
        assert!(emoji["tipLabel"].as_str().unwrap().ends_with("Emoji"));
    }

    #[test]
    fn a_7tv_change_notice_names_who_did_what() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        notice_frame(&mut st, &sk, json!({"id": "7tv-1-1", "ts": 1, "msgId": "seventv", "system": "A_g_u__ added the 7TV emote PotatWind", "seventv": {"actor": "A_g_u__", "action": "added", "emoteId": "E1", "name": "PotatWind", "oldName": ""}}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["notice"]["seventv"]["verb"], "added the 7TV emote");
        assert_eq!(v["notice"]["seventv"]["name"], "PotatWind");
        assert!(v["notice"]["seventv"]["url"].as_str().unwrap().contains("E1"));
    }

    fn kick_frame(extra: Value) -> Value {
        let mut m = json!({"id": "k1", "ts": 1, "text": "hello", "user": {"id": "3", "login": "kim", "name": "Kim", "color": "#00ff00"}, "badges": []});
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        json!({"type": "chat", "msg": m})
    }

    #[test]
    fn kick_gifts_and_raids_use_the_desktop_wording() {
        let (mut st, sk) = store_for(Platform::Kick, "agu");
        st.apply(&sk, &kick_frame(json!({"id": "k2", "notice": "kicks", "text": "gg", "kicks": {"id": "g", "name": "Rage Quit", "amount": 1500, "tier": "", "image": "https://x/coin.png"}})));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["event"]["verb"], "sent");
        assert_eq!(v["event"]["amountText"], "1,500");
        assert_eq!(v["event"]["reward"], "Rage Quit");
        assert_eq!(v["event"]["image"], "https://x/coin.png");
        assert_eq!(parts_text(&v), "gg");
        st.apply(&sk, &kick_frame(json!({"id": "k3", "notice": "host", "text": "", "host": {"viewers": 118, "message": "hi from raid"}})));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["kind"], "notice");
        assert_eq!(v["notice"]["sentence"], "Kim has raided with 118 viewers.");
        assert_eq!(v["notice"]["raid"], true);
        st.apply(&sk, &kick_frame(json!({"id": "k4", "notice": "host", "text": "", "host": {"viewers": 1, "message": ""}})));
        assert!(json_of(&st, &Env::default())["notice"]["sentence"].as_str().unwrap().ends_with("1 viewer."));
    }

    fn youtube_frame(extra: Value) -> Value {
        let mut m = json!({"id": "y1", "ts": 1, "user": {"id": "UC1", "name": "@Fan", "avatar": ""}, "badges": [], "parts": [{"t": "text", "v": "great stream"}], "text": "great stream"});
        for (k, v) in extra.as_object().unwrap() {
            m[k] = v.clone();
        }
        json!({"type": "chat", "messages": [m]})
    }

    #[test]
    fn super_chats_and_memberships_are_notices_in_their_own_colour() {
        let (mut st, sk) = store_for(Platform::Youtube, "UCabc");
        st.apply(&sk, &youtube_frame(json!({"paid": {"amount": "$5.00", "bg": "rgba(230,81,0,.5)", "headerBg": "", "sticker": false}})));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["kind"], "notice");
        let n = &v["notice"];
        assert_eq!(n["sentence"], "Sent a Super Chat for $5.00");
        assert_eq!(n["bar"], "#80e65100", "YouTube's tier colour, alpha first");
        assert_eq!(n["icon"], "coins");
        assert_eq!(n["reply"], true);
        let s = n["sentence"].as_str().unwrap();
        let b = n["bold"].as_array().unwrap();
        assert_eq!(&s[b[0].as_u64().unwrap() as usize..b[1].as_u64().unwrap() as usize], "$5.00");
        assert_eq!(parts_text(&v), "great stream");
        st.apply(&sk, &youtube_frame(json!({"id": "y2", "parts": [], "text": "", "paid": {"amount": "$2.00", "bg": "#1e88e5", "headerBg": "", "sticker": true}})));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["notice"]["sentence"], "Sent a Super Sticker for $2.00");
        let s = v["notice"]["sentence"].as_str().unwrap();
        let b = v["notice"]["bold"].as_array().unwrap();
        assert_eq!(&s[b[0].as_u64().unwrap() as usize..b[1].as_u64().unwrap() as usize], "$2.00");
        st.apply(&sk, &youtube_frame(json!({"id": "y3", "notice": "membership", "member": {"header": "Member for 12 months", "sub": "Knight", "hasMessage": true}})));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["notice"]["sentence"], "Member for 12 months \u{b7} Knight");
        assert_eq!(v["notice"]["head"], "@Fan");
        assert_eq!(parts_text(&v), "great stream", "a milestone's message stays under it");
    }

    #[test]
    fn rumble_rants_and_x_lines_are_plain_with_their_links() {
        let (mut st, sk) = store_for(Platform::Rumble, "agu");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "r1", "ts": 1, "user": {"id": "1", "login": "rum", "name": "Rum", "color": "#ffffff"}, "text": "wow https://youtu.be/dQw4w9WgXcQ", "badges": [], "rant": {"cents": 500, "duration": 60, "expiresAt": 0}}}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["rant"]["text"], "$5");
        assert_eq!(v["rant"]["color"], "#7ccf00");
        assert_eq!(v["link"]["provider"], "YouTube");
        let (mut st, sk) = store_for(Platform::X, "123");
        st.apply(&sk, &json!({"type": "chat", "messages": [{"id": "x1", "ts": 1, "user": {"id": "1", "login": "xx", "name": "Xx"}, "text": "gm https://x.com/jack/status/20"}]}));
        assert_eq!(json_of(&st, &Env::default())["link"]["provider"], "X");
    }

    #[test]
    fn css_colours_become_android_argb() {
        assert_eq!(css_argb("rgba(230,81,0,.5)").as_deref(), Some("#80e65100"));
        assert_eq!(css_argb("#1e88e5").as_deref(), Some("#ff1e88e5"));
        assert_eq!(css_argb("#00ff0080").as_deref(), Some("#8000ff00"));
        assert_eq!(css_argb("#fff").as_deref(), Some("#ffffffff"));
        assert_eq!(css_argb("blue"), None);
    }

    #[test]
    fn thousands_and_money_read_like_the_desktop() {
        assert_eq!(thousands(1234567.0), "1,234,567");
        assert_eq!(thousands(12.0), "12");
        assert_eq!(money(500.0), "$5");
        assert_eq!(money(550.0), "$5.50");
        assert_eq!(bits_color(100.0), "#9c3ee8");
    }

    #[test]
    fn a_link_card_is_asked_for_only_the_first_previewable_link() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "l1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "see https://example.com/a and https://clips.twitch.tv/AwkwardHelplessSalamanderSwiftRage then https://youtu.be/dQw4w9WgXcQ", "emotes": "", "badges": ""}}));
        let v = json_of(&st, &Env::default());
        assert_eq!(v["link"]["key"], "twitch-clip:AwkwardHelplessSalamanderSwiftRage");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "l2", "ts": 2, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "no card https://twitch.tv/esfandtv", "emotes": "", "badges": ""}}));
        assert!(json_of(&st, &Env::default()).get("link").is_none());
    }

    #[test]
    fn the_emote_box_follows_the_font_and_the_size_setting() {
        assert_eq!(Look::default().emote_px(), 25.0);
        assert_eq!(Look { font_size: 20.0, emote_size: "large".into(), ..Look::default() }.emote_px(), 50.0);
    }

    fn mod_env(ban: bool, delete: bool, automod: bool) -> Env {
        let twitch = agu_chat_model::mod_caps::ModCaps { ban, warn: ban, delete, automod, ..Default::default() };
        Env { mods: Some(ModEnv { platform: Some(Platform::Twitch), twitch, viewer: agu_chat_model::mod_caps::ModViewer { id: "1".into(), ..Default::default() }, ..Default::default() }), ..Env::default() }
    }

    #[test]
    fn a_line_carries_what_a_moderator_may_do_to_it() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "m1", "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "hi", "emotes": "", "badges": ""}}));
        let v = json_of(&st, &mod_env(true, true, false));
        assert_eq!(v["mod"]["t"], json!({"id": "m1", "uid": "9", "login": "bob", "name": "Bob", "menu": null}));
        assert_eq!((v["mod"]["ban"].clone(), v["mod"]["timeout"].clone(), v["mod"]["warn"].clone(), v["mod"]["delete"].clone()), (json!(true), json!(true), json!(true), json!(true)));
        // no rights, no `mod`
        assert!(json_of(&st, &mod_env(false, false, false)).get("mod").is_none());
        assert!(json_of(&st, &Env::default()).get("mod").is_none());
    }

    #[test]
    fn a_held_message_says_whether_allow_and_deny_work() {
        let (mut st, sk) = store_for(Platform::Twitch, "a_g_u__");
        st.apply(&sk, &json!({"type": "automod", "held": {"id": "h1", "ts": 5, "user": {"id": "9", "login": "x", "name": "X"}, "text": "bad", "emotes": "", "reason": "automod", "status": "held"}}));
        let on = json_of(&st, &mod_env(true, true, true));
        assert_eq!(on["kind"], "automod");
        assert_eq!(on["canResolve"], true);
        assert_eq!(on["mod"]["delete"], false, "a held message is not in chat yet");
        assert_eq!(json_of(&st, &mod_env(true, true, false))["canResolve"], false);
    }

    #[test]
    fn the_picture_of_a_co_chat_user_comes_with_the_line() {
        let (mut st, sk) = store_for(Platform::Kick, "agu");
        st.apply(&sk, &json!({"type": "chat", "msg": {"id": "k1", "ts": 1, "text": "hi", "user": {"id": "3", "login": "kim", "name": "Kim", "color": "", "avatar": "https://a/kim.png"}}}));
        assert_eq!(json_of(&st, &Env::default())["user"]["avatar"], "https://a/kim.png");
    }
}
