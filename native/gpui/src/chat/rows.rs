//! Port of src/renderer/chat/rows/* (RowFrame, TwitchRows, CoChatRows, env), chat/parts.tsx and the text half
//! of chat/chatText.tsx. React components become *builders*: a row is turned once into a `BuiltRow` (inline
//! items + the few box styles around them), cached by the pane, and rendered into GPUI elements each frame by
//! `render.rs`. The inline items flow through `inline.rs`.
//!
//! Round 3: a name or @mention opens the user card (`RowAction::Card`), the Ban / Warn buttons the popover.
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use agu_chat_core::protocol::{HighlightRule, ModButton, Platform, Settings, SourceRef};
use gpui::{FontWeight, Hsla};
use serde_json::Value;

use super::chat_text::{as_link, mention_word, text_mentions_login};
use super::highlights::{parse_hex, rule_row_style};
use super::inline::*;
use super::mod_buttons::describe_mod_button;
use super::mod_caps::{badge_names, mod_reach, CoCaps, ModCaps, ModViewer};
use super::twitch::badges::{parse_badge_list, BadgeMap, BADGE_PX, BADGE_VMARGIN};
use super::twitch::colors::{name_color, readable_color};
use super::twitch::cosmetics::{paint_color_at, ChatterStyle, SevenTvDefs};
use super::twitch::emotes::*;
use super::twitch::events::{builtin_redeem, notice_kind, ChatEventKind, DEFAULT_EVENTS};
use super::twitch::parts::{build_parts, split_keep_ws, EmoteSource, KnownChatter, MsgPart};
use super::twitch::reply_mention::drop_reply_mention;
use super::types::*;
use super::ui_util::*;
use crate::icons;
use crate::theme;

/// Line height as a multiple of the font size (Chatterino-dense).
pub const LINE_HEIGHT: f32 = 1.45;
const EMOTE_MAX_ASPECT: f32 = 2.4;

pub fn emote_scale(size: SizeOpt) -> f32 {
    match size {
        SizeOpt::Small => 1.45,
        SizeOpt::Medium => 1.9,
        SizeOpt::Large => 2.5,
    }
}

pub fn emote_px(font_px: f32, size: SizeOpt) -> f32 {
    (font_px * emote_scale(size)).round()
}

/// (height px, max width, vertical margin): a tall emote must not inflate the line box.
pub fn emote_box(font_px: f32, size: SizeOpt) -> (f32, f32, f32) {
    let px = emote_px(font_px, size);
    (px, (px * EMOTE_MAX_ASPECT).round(), (-((px - font_px * LINE_HEIGHT) / 2.0).round()).min(0.0))
}

pub fn twitch_emote_url(id: &str, size: SizeOpt) -> String {
    let d = match size {
        SizeOpt::Small => "1.0",
        SizeOpt::Medium => "2.0",
        SizeOpt::Large => "3.0",
    };
    format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/{d}")
}

// ---- environments -----------------------------------------------------------------------------------

#[derive(Clone)]
pub struct TwitchEnv {
    pub source: SourceRef,
    pub badge_map: Arc<BadgeMap>,
    pub cheermotes: Option<Arc<super::twitch::cheermotes::Cheermotes>>,
    pub emotes: Option<Arc<EmoteSets>>,
    pub seven_defs: Option<Arc<super::twitch::cosmetics::SevenTvDefs>>,
    pub caps: ModCaps,
    pub viewer: ModViewer,
    pub me_login_lower: String,
}

#[derive(Clone)]
pub struct CoEnv {
    pub source: SourceRef,
    pub caps: CoCaps,
    /// The conn frame says this account can post here (Kick replies need it).
    pub can_send: bool,
    pub rumble_badges: RumbleBadgeDefs,
    pub rumble_emotes: RumbleEmotes,
}

pub struct RowCtx<'a> {
    pub rs: &'a Settings,
    /// settings.showPlatformMarks: each row says where it came from.
    pub show_platform: bool,
    /// Chatterino "mod mode": inline mod buttons at the start of every row.
    pub mod_buttons: bool,
    pub mod_list: &'a [ModButton],
}

#[derive(Clone, Debug)]
pub struct ReplyTarget {
    pub sk: String,
    pub platform: Platform,
    pub id: String,
    pub user: String,
    pub text: String,
}

/// The person a name click opens the card for (UserCard's `RowUser` with the avatar the platform gave).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardUser {
    pub id: String,
    pub login: String,
    pub name: String,
    pub avatar: String,
}

/// What a click on a row's button does; the pane runs it.
#[derive(Clone, Debug)]
pub enum RowAction {
    Copy(String),
    Reply(ReplyTarget),
    Pin { sk: String, id: String },
    Delete { sk: String, id: String },
    QuickTimeout { sk: String, msg: Box<ChatMsg>, seconds: u32 },
    TwitchCommand { sk: String, msg: Box<ChatMsg>, template: String },
    Popover { sk: String, msg: Box<ChatMsg>, kind: &'static str },
    Card { sk: String, user: CardUser },
    Automod { sk: String, id: String, allow: bool },
    RumbleMod { sk: String, action: &'static str, msg: Box<RumbleMsg>, seconds: u32 },
    KickMod { sk: String, action: &'static str, msg: Box<KickMsg>, seconds: Option<u32> },
}

// ---- built rows -------------------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Hover {
    pub copy: Option<String>,
    pub reply: Option<ReplyTarget>,
    pub pin: Option<(String, String)>,
    pub delete: Option<RowAction>,
}

#[derive(Clone)]
pub struct FrameSpec {
    pub content: Rc<InlineContent>,
    pub actions: Vec<RowAction>,
    pub alt: bool,
    /// explicit background (highlight tints, Super Chats); wins over the alternating shade
    pub bg: Option<Hsla>,
    /// tint of the "highlighted message" redemption, only when no other style applies
    pub tint_class: Option<Hsla>,
    pub bar: Option<Hsla>,
    pub border: Option<Hsla>,
    pub margin_y: f32,
    pub label: Option<(String, Hsla)>,
    pub before: Option<String>,
    pub hover: Hover,
}

#[derive(Clone)]
pub struct EventSpec {
    pub content: Rc<InlineContent>,
    pub amount: Option<f64>,
    pub is_bits: bool,
    pub inner: Option<FrameSpec>,
}

#[derive(Clone)]
pub struct NoticeSpec {
    pub system: Rc<InlineContent>,
    pub text: Option<Rc<InlineContent>>,
    pub raid: bool,
}

#[derive(Clone)]
pub struct FeedSpec {
    pub time: Option<String>,
    pub text: String,
    pub base: f32,
}

#[derive(Clone)]
pub struct AutomodSpec {
    pub sk: String,
    pub user: CardUser,
    pub id: String,
    pub header: Rc<InlineContent>,
    pub chip: String,
    pub body: Rc<InlineContent>,
    pub status: String,
    pub resolved_line: String,
    pub can_resolve: bool,
    pub base: f32,
}

#[derive(Clone)]
pub enum BuiltRow {
    Frame(FrameSpec),
    Event(EventSpec),
    Notice(NoticeSpec),
    Feed(FeedSpec),
    Automod(AutomodSpec),
}

impl BuiltRow {
    pub fn urls(&self) -> Vec<String> {
        match self {
            BuiltRow::Frame(f) => f.content.urls(),
            BuiltRow::Event(e) => e.inner.as_ref().map(|f| f.content.urls()).unwrap_or_default(),
            BuiltRow::Notice(n) => n.text.as_ref().map(|t| t.urls()).unwrap_or_default(),
            _ => vec![],
        }
    }
}

// ---- inline item helpers ----------------------------------------------------------------------------

struct Flow {
    items: Vec<Item>,
    base: f32,
}

impl Flow {
    fn new(base: f32) -> Self {
        Flow { items: Vec::new(), base }
    }
    fn text(&mut self, t: &str, color: Hsla) {
        if !t.is_empty() {
            self.items.push(Item::Text(TextItem::new(t.to_string(), self.base, color)));
        }
    }
    fn push(&mut self, it: Item) {
        self.items.push(it);
    }
    fn finish(self) -> Rc<InlineContent> {
        InlineContent::new(self.items, self.base, LINE_HEIGHT)
    }
}

fn emote_item(url: String, tip: Rc<Tip>, overlays: Vec<SharedStr>, font_px: f32, size: SizeOpt) -> Item {
    let (px, max_w, vm) = emote_box(font_px, size);
    Item::Image(ImageItem { url: url.into(), max_w, max_h: px, fixed: false, wrap: true, vmargin: vm, margin_x: 0.0, valign: VAlign::Middle, tip: Some(tip), overlays, radius: 0.0, act: None })
}

type SharedStr = gpui::SharedString;

fn badge_item(url: &str, title: &str) -> Item {
    Item::Image(ImageItem {
        url: url.to_string().into(),
        max_w: BADGE_PX,
        max_h: BADGE_PX,
        fixed: true,
        wrap: false,
        vmargin: BADGE_VMARGIN,
        margin_x: 0.0,
        valign: VAlign::BoxCenter,
        tip: None,
        overlays: vec![],
        radius: 0.0,
        act: None,
    })
    .with_title(title)
}

trait WithTitle {
    fn with_title(self, t: &str) -> Self;
}
impl WithTitle for Item {
    /// Badges have a `title` tooltip in the DOM (the native title attribute); shown through the emote tip layer.
    fn with_title(mut self, t: &str) -> Self {
        if let Item::Image(i) = &mut self {
            if !t.is_empty() {
                i.tip = Some(Rc::new(Tip { img: "".into(), name: t.to_string().into(), label: "".into() }));
            }
        }
        self
    }
}

fn link_item(href: &str, text: &str, base: f32) -> Item {
    Item::Text(TextItem::new(text.to_string(), base, token(theme::SKY_400)).act(Act::Url(href.to_string().into()), true))
}

/// MessageText: the message's parts as inline items.
fn message_items(f: &mut Flow, parts: &[MsgPart], size: SizeOpt, emotes: Option<&EmoteSets>, text_color: Hsla, strike: bool, readable_mentions: bool, mut mention_acts: Option<(&mut Vec<RowAction>, &str)>) {
    let base = f.base;
    for p in parts {
        match p {
            MsgPart::Text(t) => {
                let mut ti = TextItem::new(t.clone(), base, text_color).strike(strike);
                if strike {
                    ti.color = with_alpha(text_color, 0.5);
                }
                f.push(Item::Text(ti));
            }
            MsgPart::Mention { name, color, login, user_id } => {
                let c = if color.is_empty() { text_color } else if readable_mentions { hex(&readable_color(color)) } else { hex(color) };
                let mut t = TextItem::new(format!("@{name}"), base, c).weight(FontWeight::SEMIBOLD).strike(strike);
                // clickable only on the live message rows: it opens the card of who is mentioned
                if let Some((acts, sk)) = mention_acts.as_mut() {
                    acts.push(RowAction::Card { sk: sk.to_string(), user: CardUser { id: user_id.clone(), login: login.clone(), name: name.clone(), avatar: String::new() } });
                    t = t.act(Act::Tag(acts.len() as u32 - 1), true);
                }
                f.push(Item::Text(t));
            }
            MsgPart::Gif { url, .. } => f.push(Item::Gif(GifItem { url: url.clone().into() })),
            MsgPart::Cheer { name, prefix, amount, color, url } => {
                // twitch.tv: the tier image at emote height, then the amount, bold, in the tier's colour
                let tip = Rc::new(Tip { img: url.clone().into(), name: name.clone().into(), label: format!("{prefix} cheermote").into() });
                f.push(emote_item(url.clone(), tip, vec![], base, size));
                let c = if color.is_empty() { text_color } else { hex(color) };
                f.push(Item::Text(TextItem::new(format!("{amount}"), base, c).weight(FontWeight::BOLD).strike(strike)));
            }
            MsgPart::Link { href, text } => f.push(link_item(href, text, base)),
            MsgPart::Emote { source, id, name, overlays } => {
                let entry = match source {
                    EmoteSource::Twitch => None,
                    _ => emotes.and_then(|e| e.all.get(name)).cloned(),
                };
                let url = emote_part_url(*source, id, size, entry.as_deref());
                let tip = Rc::new(emote_tip(*source, id, name, false, entry.as_deref()));
                let ov: Vec<SharedStr> = overlays
                    .iter()
                    .map(|o| {
                        let oe = emotes.and_then(|e| e.all.get(&o.name)).cloned();
                        emote_part_url(o.source, &o.id, size, oe.as_deref()).into()
                    })
                    .collect();
                f.push(emote_item(url, tip, ov, base, size));
            }
        }
    }
}

/// The parts of a message as inline items (used by the pinned card).
pub fn message_flow(base: f32, parts: &[MsgPart], size: SizeOpt, emotes: Option<&EmoteSets>, color: Hsla) -> Vec<Item> {
    let mut f = Flow::new(base);
    message_items(&mut f, parts, size, emotes, color, false, true, None);
    f.items
}

fn emote_part_url(source: EmoteSource, id: &str, size: SizeOpt, entry: Option<&ProviderEmote>) -> String {
    match source {
        EmoteSource::Twitch => twitch_emote_url(id, size),
        _ => entry.map(|e| provider_emote_url(e, size)).unwrap_or_default(),
    }
}

fn emote_tip(source: EmoteSource, id: &str, name: &str, zero_width: bool, entry: Option<&ProviderEmote>) -> Tip {
    let (img, label) = match source {
        EmoteSource::Twitch => (format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/3.0"), "Twitch".to_string()),
        EmoteSource::Provider(p) => (entry.map(provider_tooltip_url).unwrap_or_default(), format!("{}{}", p.label(), if zero_width { " zero-width" } else { "" })),
    };
    Tip { img: img.into(), name: name.to_string().into(), label: label.into() }
}

/// TextRun (chatText.tsx): links become links, "@name" a bold mention, whitespace kept.
fn text_run_items(f: &mut Flow, text: &str, color: Hsla) {
    let base = f.base;
    for w in split_keep_ws(text) {
        if w.chars().all(|c| c.is_whitespace()) {
            f.text(w, color);
            continue;
        }
        if let Some(l) = as_link(w) {
            f.text(&l.lead, color);
            f.push(link_item(&l.href, &l.text, base));
            f.text(&l.trail, color);
            continue;
        }
        if w.starts_with('@') {
            if let Some((_, name, trail)) = mention_word(w) {
                f.push(Item::Text(TextItem::new(format!("@{name}"), base, token(theme::TEXT)).weight(FontWeight::SEMIBOLD)));
                f.text(trail, color);
                continue;
            }
        }
        f.text(w, color);
    }
}

// ---- RowFrame pieces --------------------------------------------------------------------------------

pub(crate) fn icon_for_mod(icon: &str) -> &'static str {
    match icon {
        "ban" => icons::MINUS_CIRCLE,
        "clock" => icons::CLOCK,
        "trash" => icons::TRASH,
        "shield" => icons::SHIELD,
        "warning" => icons::WARNING,
        "flag" => icons::FLAG,
        "eyeoff" => icons::EYE_OFF,
        "pin" => icons::PIN,
        "check" => icons::CHECK,
        _ => icons::CHAT,
    }
}

/// `modButtonTone`: hover colour by action.
fn mod_tone(b: &ModButton) -> Hsla {
    match b.action.as_str() {
        "timeout" | "warn" => token(theme::AMBER_400),
        "command" => token(theme::ACCENT_SOFT),
        _ => token(theme::RED_400),
    }
}

/// The inline mod buttons: `can` hides the ones this account can't use on this row; `run` maps a button to an
/// action. Clicks index into `actions`.
fn mod_button_items(f: &mut Flow, buttons: &[ModButton], can: &dyn Fn(&ModButton) -> bool, run: &dyn Fn(&ModButton) -> RowAction, actions: &mut Vec<RowAction>) {
    let shown: Vec<&ModButton> = buttons.iter().filter(|b| can(b)).collect();
    let n = shown.len();
    for (i, b) in shown.into_iter().enumerate() {
        let hb = f.base * 1.15;
        let face = if !b.label.is_empty() {
            ButtonFace::Label(b.label.clone().into(), f.base * 0.72)
        } else {
            ButtonFace::Icon(icon_for_mod(&b.icon), 12.0)
        };
        let w = match &face {
            ButtonFace::Label(_, fs) => (b.label.chars().count() as f32 * fs * 0.62 + 2.0).max(hb),
            ButtonFace::Icon(..) => hb,
        };
        actions.push(run(b));
        f.push(Item::Button(ButtonItem {
            face,
            w,
            h: hb,
            color: token(theme::FAINT),
            hover_color: mod_tone(b),
            hover_bg: token(theme::RAISED_2),
            down: 0.18 * f.base,
            radius: 4.0,
            margin_r: if i + 1 == n { 4.0 } else { 1.0 },
            act: Act::Tag(actions.len() as u32 - 1),
            tip: Some(describe_mod_button(b).into()),
        }));
    }
}

/// timestamp + platform mark: the start of every RowFrame line.
fn frame_prefix(f: &mut Flow, ctx: &RowCtx, ts: i64, platform: Platform) {
    if ctx.rs.timestamps {
        f.push(Item::Text(TextItem::new(fmt_time(ts, ctx.rs.timestamp_seconds), f.base * 0.85, token(theme::FAINT))));
        f.push(Item::Gap(6.0));
    }
    if ctx.show_platform {
        // 0.95em, drawn on whole pixels (the svg rasterizer rounds its box up, which made it look a pixel bigger)
        let sz = (f.base * 0.95).round();
        f.push(Item::Icon(IconItem {
            path: platform_icon_path(platform),
            size: sz,
            color: token(theme::platform_color(platform)),
            // twitch: an svg inside an inline-flex span, whose baseline is the svg's bottom; the other marks are bare
            // svgs with `align-[-0.12em]`
            valign: VAlign::BottomAt(if platform == Platform::Twitch { 0.0 } else { 0.12 * f.base }),
            margin_x: 0.0,
        }));
        f.push(Item::Gap(4.0));
    }
}

fn platform_icon_path(p: Platform) -> &'static str {
    match p {
        Platform::Twitch => icons::TWITCH,
        Platform::Kick => icons::KICK,
        Platform::Youtube => icons::YOUTUBE,
        Platform::Rumble => icons::RUMBLE,
        Platform::X => icons::PLATFORM_X,
    }
}

fn frame(content: Rc<InlineContent>, actions: Vec<RowAction>, ctx: &RowCtx, ord: u64) -> FrameSpec {
    FrameSpec {
        content,
        actions,
        alt: ctx.rs.alternate_rows && ord % 2 == 1,
        bg: None,
        tint_class: None,
        bar: None,
        border: None,
        margin_y: 0.0,
        label: None,
        before: None,
        hover: Hover::default(),
    }
}

fn rgb_a(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    gpui::Rgba { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }.into()
}

fn rule_style(rule: &HighlightRule) -> (Hsla, Hsla) {
    let s = rule_row_style(&rule.color);
    (rgb_a(s.bar.0, s.bar.1, s.bar.2, 1.0), rgb_a(s.background.0, s.background.1, s.background.2, s.background.3))
}

// ---- Twitch message ---------------------------------------------------------------------------------

struct Highlight {
    bar: Option<Hsla>,
    bg: Option<Hsla>,
    label: Option<(String, Hsla)>,
}

fn row_highlight(msg: &ChatMsg, mentions_me: bool, susp: Option<&str>, rule: Option<&HighlightRule>) -> Highlight {
    let h = |bar: (u8, u8, u8), bg: (u8, u8, u8, f32), label: &str, lc: u32| Highlight {
        bar: Some(rgb_a(bar.0, bar.1, bar.2, 1.0)),
        bg: Some(rgb_a(bg.0, bg.1, bg.2, bg.3)),
        label: Some((label.to_string(), token(lc))),
    };
    if susp == Some("restricted") {
        return h((0xff, 0x5c, 0x1a), (255, 92, 26, 0.10), "RESTRICTED SUSPICIOUS USER", theme::ORANGE_300);
    }
    if susp == Some("active_monitoring") {
        return h((0xff, 0x8c, 0x00), (255, 140, 0, 0.10), "MONITORED SUSPICIOUS USER", theme::ORANGE_300);
    }
    if mentions_me {
        return h((0xff, 0x3b, 0x3b), (255, 30, 30, 0.12), "MENTIONS YOU", theme::RED_300);
    }
    if let Some(r) = rule {
        let (bar, bg) = rule_style(r);
        return Highlight { bar: Some(bar), bg: Some(bg), label: None };
    }
    if msg.first_msg {
        return h((0x91, 0x47, 0xff), (145, 71, 255, 0.08), "FIRST MESSAGE", theme::PURPLE_300);
    }
    if msg.returning {
        return h((0x2b, 0x8f, 0xef), (43, 143, 239, 0.08), "RETURNING CHATTER", theme::SKY_300);
    }
    Highlight { bar: None, bg: None, label: None }
}

pub struct TwitchInput<'a> {
    pub row: &'a Row,
    pub msg: &'a ChatMsg,
    pub ctx: &'a RowCtx<'a>,
    pub env: &'a TwitchEnv,
    pub known: &'a HashMap<String, KnownChatter>,
    pub susp: Option<&'a str>,
    pub rule: Option<&'a HighlightRule>,
    pub style: Option<&'a ChatterStyle>,
    pub defs: Option<&'a SevenTvDefs>,
}

/// A name with a 7TV paint: per-character colours across the width.
fn paint_gradient(paint: &super::twitch::cosmetics::SevenTvPaint, fallback: Hsla) -> Option<Rc<dyn Fn(f32) -> Hsla>> {
    // Image-only paints have no colour to evaluate: keep the plain name colour.
    if paint_color_at(paint, 0.5).is_none() {
        return None;
    }
    let paint = paint.clone();
    Some(Rc::new(move |t| match paint_color_at(&paint, t) {
        Some((r, g, b, a)) => gpui::Rgba { r, g, b, a }.into(),
        None => fallback,
    }))
}

pub fn twitch_message_row(inp: &TwitchInput) -> BuiltRow {
    let TwitchInput { row, msg, ctx, env, known, susp, rule, style, defs } = inp;
    let rs = ctx.rs;
    let base = rs.font_size;
    let size = SizeOpt::parse(&rs.emote_size);
    let cosmetics_on = rs.seven_tv_cosmetics;
    let paint = if cosmetics_on { style.and_then(|s| s.paint_id.as_ref()).and_then(|id| defs.and_then(|d| d.paints.get(id))) } else { None };
    let seven_badge = if cosmetics_on { style.and_then(|s| s.badge_id.clone()).and_then(|id| defs.and_then(|d| d.badges.get(&id)).map(|b| (id, b.clone()))) } else { None };

    let color = hex(&name_color(&msg.user, rs.readable_colors));
    let mentions_me = text_mentions_login(&msg.text, &env.me_login_lower);
    let hl = row_highlight(msg, mentions_me, *susp, *rule);
    let built = build_parts(&msg.text, &msg.emotes, env.emotes.as_deref(), Some(known), msg.gifs.as_deref().unwrap_or(""));
    let parts = match &msg.reply {
        Some(r) => drop_reply_mention(built, &r.user),
        None => built,
    };
    // cheermotes only on a message that carries bits
    let parts = match (&env.cheermotes, msg.bits.filter(|b| *b != 0.0)) {
        (Some(cm), Some(_)) => super::twitch::cheermotes::apply(parts, cm, rs.animated_emotes, emote_box(rs.font_size, size).0),
        _ => parts,
    };
    let reach = mod_reach(&env.viewer, &msg.user.id, &msg.badges);
    let caps = &env.caps;
    let can_ban = caps.ban && reach.ban;
    let can_timeout = caps.ban && reach.timeout;
    let can_delete = caps.delete && reach.delete && !msg.deleted;
    let struck = msg.deleted && rs.deleted_messages == "strike";
    let is_mod = caps.ban || caps.delete || caps.warn;
    let sk = row.sk.clone();

    let mut f = Flow::new(base);
    let mut actions: Vec<RowAction> = Vec::new();
    if ctx.mod_buttons {
        let can = |b: &ModButton| match b.action.as_str() {
            "ban" => can_ban && !msg.deleted,
            "timeout" => can_timeout && !msg.deleted,
            "warn" => caps.warn && reach.warn && !msg.deleted,
            "delete" => can_delete,
            _ => is_mod,
        };
        let run = |b: &ModButton| match b.action.as_str() {
            "ban" => RowAction::Popover { sk: sk.clone(), msg: Box::new((*msg).clone()), kind: "ban" },
            "timeout" => RowAction::QuickTimeout { sk: sk.clone(), msg: Box::new((*msg).clone()), seconds: b.seconds },
            "warn" => RowAction::Popover { sk: sk.clone(), msg: Box::new((*msg).clone()), kind: "warn" },
            "delete" => RowAction::Delete { sk: sk.clone(), id: msg.id.clone() },
            _ => RowAction::TwitchCommand { sk: sk.clone(), msg: Box::new((*msg).clone()), template: b.command.clone() },
        };
        mod_button_items(&mut f, ctx.mod_list, &can, &run, &mut actions);
    }
    frame_prefix(&mut f, ctx, msg.ts, Platform::Twitch);

    // the name button: badges, 7TV badge, name, 4px apart
    let mut first = true;
    let gap = |f: &mut Flow, first: &mut bool| {
        if !*first {
            f.push(Item::Gap(4.0));
        }
        *first = false;
    };
    if rs.show_badges {
        let list = parse_badge_list(&msg.badges, &env.badge_map);
        if !list.is_empty() {
            gap(&mut f, &mut first);
            for (i, b) in list.iter().enumerate() {
                if i > 0 {
                    f.push(Item::Gap(2.0));
                }
                f.push(badge_item(&b.url, &b.title));
            }
        }
        if let Some((id, def)) = &seven_badge {
            gap(&mut f, &mut first);
            let title = if def.tooltip.is_empty() { &def.name } else { &def.tooltip };
            f.push(badge_item(&format!("https://cdn.7tv.app/badge/{id}/3x"), title));
        }
    }
    gap(&mut f, &mut first);
    actions.push(RowAction::Card { sk: sk.clone(), user: CardUser { id: msg.user.id.clone(), login: msg.user.login.clone(), name: msg.user.name.clone(), avatar: String::new() } });
    let card_tag = actions.len() as u32 - 1;
    let mut name = TextItem::new(msg.user.name.clone(), base, color).weight(FontWeight::SEMIBOLD).act(Act::Tag(card_tag), true);
    if let Some(p) = paint {
        name.gradient = paint_gradient(p, color);
        name.paint = super::paint_text::PaintSpec::from_paint(p).map(Rc::new);
    }
    f.push(Item::Text(name));
    if !msg.action {
        f.push(Item::Text(TextItem::new(":", base, token(theme::MUTED))));
    }
    f.push(Item::Text(TextItem::new(" ", base, token(theme::TEXT))));
    if msg.action {
        // /me: the message takes the name's style and medium weight
        let mark = f.items.len();
        message_items(&mut f, &parts, size, env.emotes.as_deref(), color, struck, rs.readable_colors, Some((&mut actions, &sk)));
        for it in f.items[mark..].iter_mut() {
            if let Item::Text(t) = it {
                t.weight = FontWeight::MEDIUM;
                if let Some(p) = paint {
                    t.gradient = paint_gradient(p, color);
                }
            }
        }
    } else {
        message_items(&mut f, &parts, size, env.emotes.as_deref(), token(theme::TEXT), struck, rs.readable_colors, Some((&mut actions, &sk)));
    }

    let content = f.finish();
    let mut spec = frame(content, actions, ctx, row.ord);
    let highlighted = msg.msg_id.as_deref() == Some("highlighted-message");
    spec.bar = hl.bar;
    spec.bg = hl.bg;
    spec.label = hl.label;
    if highlighted && spec.bg.is_none() {
        spec.tint_class = Some(token_a(theme::PURPLE_500, 0.10));
    }
    spec.before = msg.reply.as_ref().map(|r| format!("↩ replying to @{}: {}", r.user, r.text));
    spec.hover = Hover {
        copy: Some(msg.text.clone()),
        reply: Some(ReplyTarget { sk: sk.clone(), platform: Platform::Twitch, id: msg.id.clone(), user: msg.user.name.clone(), text: msg.text.clone() }),
        pin: caps.pin.then(|| (sk.clone(), msg.id.clone())),
        delete: can_delete.then(|| RowAction::Delete { sk: sk.clone(), id: msg.id.clone() }),
    };

    // wrapped in the redeem / cheer frame when it is one
    let ev = &DEFAULT_EVENTS;
    let builtin = msg.msg_id.as_deref().and_then(builtin_redeem);
    let is_bits = msg.reward.as_ref().map(|r| r.bits.unwrap_or(0.0) != 0.0).unwrap_or(false) || builtin.map(|b| b.1).unwrap_or(false);
    let frame_color = hex(&name_color(&msg.user, rs.readable_colors));
    if (msg.reward_id.is_some() || builtin.is_some()) && !msg.deleted && (if is_bits { ev.cheers } else { ev.redeems }) {
        let reward_title = builtin.map(|b| b.0.to_string()).or_else(|| msg.reward.as_ref().map(|r| r.title.clone()));
        let amount = if builtin.is_some() { None } else { msg.reward.as_ref().and_then(|r| r.bits.filter(|b| *b != 0.0).or(r.cost)) };
        return BuiltRow::Event(event_frame(&msg.user.name, frame_color, "redeemed", reward_title, amount, is_bits, if is_bits { "cheer" } else { "redeem" }, Some(spec), base));
    }
    if let Some(b) = msg.bits.filter(|b| *b != 0.0) {
        if ev.cheers && !msg.deleted {
            return BuiltRow::Event(event_frame(&msg.user.name, frame_color, "cheered", None, Some(b), true, "cheer", Some(spec), base));
        }
    }
    BuiltRow::Frame(spec)
}

#[allow(clippy::too_many_arguments)]
fn event_frame(user: &str, color: Hsla, verb: &str, reward: Option<String>, amount: Option<f64>, is_bits_hint: bool, tone: &str, inner: Option<FrameSpec>, base: f32) -> EventSpec {
    let mut f = Flow::new(base);
    f.push(Item::Text(TextItem::new(user.to_string(), base, color).weight(FontWeight::SEMIBOLD)));
    f.push(Item::Text(TextItem::new(format!(" {verb}"), base, token(theme::MUTED))));
    if let Some(r) = reward {
        f.push(Item::Text(TextItem::new(" ", base, token(theme::MUTED))));
        f.push(Item::Text(TextItem::new(r, base, token(theme::TEXT)).weight(FontWeight::SEMIBOLD)));
    }
    let is_bits = is_bits_hint || (tone == "cheer" && amount.is_none());
    EventSpec { content: f.finish(), amount, is_bits, inner }
}

// ---- notices ----------------------------------------------------------------------------------------

pub fn notice_row(row: &Row, n: &Notice, ctx: &RowCtx, env: &TwitchEnv) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let size = SizeOpt::parse(&rs.emote_size);
    if (n.msg_id == "redeem" || n.msg_id == "powerup") && n.reward.is_some() {
        // A redeem with no message: the same header as one with a message, just nothing under it.
        let r = n.reward.as_ref().unwrap();
        let bits = r.bits.unwrap_or(0.0) != 0.0;
        let amount = if bits { r.bits } else { r.cost };
        let color = hex(&name_color(&n.user, rs.readable_colors));
        return BuiltRow::Event(event_frame(&n.user.name, color, "redeemed", Some(r.title.clone()), amount, bits, if bits { "cheer" } else { "redeem" }, None, base));
    }
    let raid = notice_kind(&n.msg_id, n.source_msg_id.as_deref()) == Some(ChatEventKind::Raids);
    let mut sys = Flow::new(base);
    sys.push(Item::Text(TextItem::new(n.system.clone(), base, token(if raid { theme::ORANGE_200 } else { theme::PURPLE_200 })).weight(FontWeight::SEMIBOLD)));
    let text = if n.text.is_empty() {
        None
    } else {
        let parts = build_parts(&n.text, n.emotes.as_deref().unwrap_or(""), env.emotes.as_deref(), None, "");
        let mut f = Flow::new(base);
        message_items(&mut f, &parts, size, env.emotes.as_deref(), token(theme::MUTED), false, rs.readable_colors, None);
        Some(f.finish())
    };
    let _ = row;
    BuiltRow::Notice(NoticeSpec { system: sys.finish(), text, raid })
}

pub fn feed_row(f: &ModFeedEntry, ctx: &RowCtx) -> BuiltRow {
    let time = ctx.rs.timestamps.then(|| fmt_time(f.ts, false));
    BuiltRow::Feed(FeedSpec { time, text: f.text.clone(), base: ctx.rs.font_size })
}

pub fn automod_row(row: &Row, h: &HeldMsg, ctx: &RowCtx, env: &TwitchEnv) -> BuiltRow {
    let base = ctx.rs.font_size;
    let chip = if h.reason == "blocked_term" {
        "Blocked term".to_string()
    } else if let Some(c) = &h.category {
        format!("{c}{}", h.level.map(|l| format!(" (level {})", l as i64)).unwrap_or_default())
    } else {
        "AutoMod".to_string()
    };
    let hs = base * 0.92;
    let mut head = Flow::new(hs);
    head.push(Item::Text(TextItem::new("AutoMod held a message from ", hs, token(theme::AMBER_200))));
    head.push(Item::Text(TextItem::new(h.user.name.clone(), hs, token(theme::AMBER_200)).weight(FontWeight::SEMIBOLD).act(Act::Tag(0), true)));
    let mut body = Flow::new(base);
    body.push(Item::Text(TextItem::new(h.text.clone(), base, token(theme::TEXT))));
    let resolved = match h.status.as_str() {
        "approved" => format!("Allowed by {}", h.resolved_by.clone().filter(|s| !s.is_empty()).unwrap_or("?".into())),
        "denied" => format!("Denied by {}", h.resolved_by.clone().filter(|s| !s.is_empty()).unwrap_or("?".into())),
        _ => "Expired (no action taken)".to_string(),
    };
    BuiltRow::Automod(AutomodSpec {
        sk: row.sk.clone(),
        user: CardUser { id: h.user.id.clone(), login: h.user.login.clone(), name: h.user.name.clone(), avatar: String::new() },
        id: h.id.clone(),
        header: head.finish(),
        chip,
        body: body.finish(),
        status: h.status.clone(),
        resolved_line: resolved,
        can_resolve: env.caps.automod,
        base,
    })
}

// ---- co-chat rows -----------------------------------------------------------------------------------

fn name_color_of(c: &str, readable: bool) -> Hsla {
    if c.is_empty() {
        token(theme::MUTED)
    } else if readable {
        hex(&readable_color(c))
    } else {
        hex(c)
    }
}

fn plain_emote(f: &mut Flow, src: String, alt: &str, tip_label: &str, rs: &Settings) {
    let size = SizeOpt::parse(&rs.emote_size);
    let (px, max_w, vm) = emote_box(rs.font_size, size);
    f.push(Item::Image(ImageItem {
        url: src.clone().into(),
        max_w,
        max_h: px,
        fixed: false,
        wrap: false,
        vmargin: vm,
        margin_x: 1.0,
        valign: VAlign::Middle,
        tip: Some(Rc::new(Tip { img: src.into(), name: alt.to_string().into(), label: tip_label.to_string().into() })),
        overlays: vec![],
        radius: 0.0,
        act: None,
    }));
}

fn co_frame_style(spec: &mut FrameSpec, rule: Option<&HighlightRule>) {
    if let Some(r) = rule {
        let (bar, bg) = rule_style(r);
        spec.bar = Some(bar);
        spec.bg = Some(bg);
    }
}

fn simple_badge_chip(f: &mut Flow, label: &str, fg: Hsla, bg: Hsla, glyph: &str) {
    // `inline-flex h-[1.1em] min-w-[1.1em] ... rounded px-[3px] text-[10px] font-bold`
    f.push(Item::Pill(PillItem { text: glyph.to_string().into(), size: 10.0, color: fg, weight: FontWeight::BOLD, bg, pad_x: 3.0, margin_r: 4.0, radius: 4.0 }));
    let _ = label;
}

fn rant_tone(cents: f64) -> (Hsla, Hsla) {
    let d = cents / 100.0;
    let (c, hexv) = if d >= 100.0 {
        (0xff2056, 0)
    } else if d >= 50.0 {
        (0xff6900, 0)
    } else if d >= 20.0 {
        (0xfe9a00, 0)
    } else if d >= 10.0 {
        (0xefb100, 0)
    } else if d >= 5.0 {
        (0x7ccf00, 0)
    } else if d >= 2.0 {
        (0x00bc7d, 0)
    } else {
        (0x00bcff, 0)
    };
    let _ = hexv;
    (token_a(c, 0.5), token_a(c, 0.15))
}

fn money(cents: f64) -> String {
    let d = cents / 100.0;
    if d.fract() == 0.0 { format!("${}", d as i64) } else { format!("${d:.2}") }
}

fn json_str<'a>(v: &'a Value, path: &str) -> Option<&'a str> {
    v.pointer(path).and_then(|x| x.as_str())
}

fn rumble_badge_icon(def: &Value) -> Option<String> {
    let icons = def.get("icons")?.as_object()?;
    let path = icons.get("96").or_else(|| icons.get("48")).or_else(|| icons.values().next())?.as_str()?;
    Some(if path.starts_with("http") { path.to_string() } else { format!("https://rumble.com{path}") })
}

fn co_badge_img(url: &str, title: &str, base: f32) -> Item {
    let s = base * 1.1;
    Item::Image(ImageItem {
        url: url.to_string().into(),
        max_w: s,
        max_h: s,
        fixed: true,
        wrap: false,
        vmargin: 0.0,
        margin_x: 0.0,
        valign: VAlign::BottomAt(0.2 * base),
        tip: Some(Rc::new(Tip { img: "".into(), name: title.to_string().into(), label: "".into() })),
        overlays: vec![],
        radius: 0.0,
        act: None,
    })
}

pub fn rumble_row(row: &Row, m: &RumbleMsg, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let rant = m.rant.as_ref().filter(|r| r.cents > 0.0);
    let striking = m.deleted && rs.deleted_messages == "strike";
    let can_mod = env.caps.can_moderate && !m.deleted;
    let sk = row.sk.clone();
    let mut f = Flow::new(base);
    let mut actions = Vec::new();
    if ctx.mod_buttons && can_mod {
        let can = |b: &ModButton| b.action != "command" && b.action != "warn";
        let run = |b: &ModButton| RowAction::RumbleMod { sk: sk.clone(), action: if b.action == "ban" { "ban" } else if b.action == "timeout" { "timeout" } else { "delete" }, msg: Box::new(m.clone()), seconds: b.seconds };
        mod_button_items(&mut f, ctx.mod_list, &can, &run, &mut actions);
    }
    frame_prefix(&mut f, ctx, m.ts, Platform::Rumble);
    if rs.show_badges {
        for id in &m.badges {
            if let Some(def) = env.rumble_badges.get(id) {
                if let Some(url) = rumble_badge_icon(def) {
                    let title = json_str(def, "/label/en").unwrap_or(id);
                    f.push(co_badge_img(&url, title, base));
                    f.push(Item::Gap(4.0));
                }
            }
        }
    }
    actions.push(RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } });
    f.push(Item::Text(TextItem::new(if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() }, base, name_color_of(&m.user.color, rs.readable_colors)).weight(FontWeight::SEMIBOLD).act(Act::Tag(actions.len() as u32 - 1), true)));
    if let Some(ch) = &m.channel {
        if !m.user.login.is_empty() && ch.name != m.user.login {
            f.push(Item::Gap(4.0));
            f.push(Item::Text(TextItem::new(format!("({})", m.user.login), 10.0, token(theme::FAINT))));
        }
    }
    f.push(Item::Text(TextItem::new(": ", base, token(theme::MUTED))));
    if let Some(r) = rant {
        f.push(Item::Pill(PillItem { text: money(r.cents).into(), size: 11.0, color: token(theme::WHITE), weight: FontWeight::BOLD, bg: token_a(theme::BLACK, 0.3), pad_x: 4.0, margin_r: 4.0, radius: 4.0 }));
    }
    let col = if striking { token(theme::FAINT) } else { token(theme::TEXT) };
    // :name: tokens resolved against the chat's own emote list
    rumble_text(&mut f, &m.text, &env.rumble_emotes, col, striking, rs);
    let mut spec = frame(f.finish(), actions, ctx, row.ord);
    if let Some(r) = rant {
        let (border, bg) = rant_tone(r.cents);
        spec.border = Some(border);
        spec.bg = Some(bg);
        spec.margin_y = 2.0;
        if let Some(rule) = rule {
            spec.bar = Some(rule_style(rule).0);
        }
    } else {
        co_frame_style(&mut spec, rule);
    }
    spec.hover = Hover { copy: Some(m.text.clone()), delete: can_mod.then(|| RowAction::RumbleMod { sk: sk.clone(), action: "delete", msg: Box::new(m.clone()), seconds: 300 }), ..Default::default() };
    BuiltRow::Frame(spec)
}

/// Port of `splitRumbleEmotes` (CoChatRows.tsx): `:name:` tokens (RUMBLE_TOKEN = /:([A-Za-z0-9_+.-]{1,60}):/g)
/// resolved against the chat's emote list; a token with no emote stays text, and like the global regex the
/// scan then resumes AFTER its closing colon (so in "a:b:c:" a missing ":b:" never makes ":c:" a token).
#[derive(Debug, PartialEq)]
pub(crate) enum RumblePart<'a> {
    Text(&'a str),
    Emote { name: &'a str, file: String },
}

pub(crate) fn split_rumble_emotes<'a>(text: &'a str, emotes: &RumbleEmotes) -> Vec<RumblePart<'a>> {
    if text.is_empty() {
        return vec![];
    }
    if emotes.is_empty() {
        return vec![RumblePart::Text(text)];
    }
    let is_tok = |c: char| c.is_ascii_alphanumeric() || "_+.-".contains(c);
    let mut parts = Vec::new();
    let mut last = 0;
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b':' {
            let rest = &text[i + 1..];
            let len = rest.find(|c: char| !is_tok(c)).unwrap_or(rest.len());
            if (1..=60).contains(&len) && rest[len..].starts_with(':') {
                let name = &rest[..len];
                let end = i + 1 + len + 1;
                if let Some(file) = emotes.get(name).and_then(|e| e.get("file")).and_then(|v| v.as_str()) {
                    if i > last {
                        parts.push(RumblePart::Text(&text[last..i]));
                    }
                    parts.push(RumblePart::Emote { name, file: file.to_string() });
                    last = end;
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    if parts.is_empty() {
        return vec![RumblePart::Text(text)];
    }
    if last < text.len() {
        parts.push(RumblePart::Text(&text[last..]));
    }
    parts
}

fn rumble_text(f: &mut Flow, text: &str, emotes: &RumbleEmotes, col: Hsla, strike: bool, rs: &Settings) {
    for p in split_rumble_emotes(text, emotes) {
        match p {
            RumblePart::Text(t) => run_text(f, t, col, strike),
            RumblePart::Emote { name, file } => plain_emote(f, file, &format!(":{name}:"), "Rumble", rs),
        }
    }
}

fn run_text(f: &mut Flow, text: &str, col: Hsla, strike: bool) {
    let mark = f.items.len();
    text_run_items(f, text, col);
    if strike {
        for it in f.items[mark..].iter_mut() {
            if let Item::Text(t) = it {
                t.strike = true;
            }
        }
    }
}

fn split_kick(text: &str) -> Vec<(bool, String, String)> {
    // (is_emote, id-or-text, name)
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[emote:") {
        let tail = &rest[i + 7..];
        let digits = tail.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && tail[digits..].starts_with(':') {
            let after = &tail[digits + 1..];
            if let Some(close) = after.find(']') {
                let name = &after[..close];
                if (1..=80).contains(&name.chars().count()) {
                    if i > 0 {
                        out.push((false, rest[..i].to_string(), String::new()));
                    }
                    out.push((true, tail[..digits].to_string(), name.to_string()));
                    rest = &after[close + 1..];
                    continue;
                }
            }
        }
        out.push((false, rest[..i + 7].to_string(), String::new()));
        rest = tail;
    }
    if !rest.is_empty() {
        out.push((false, rest.to_string(), String::new()));
    }
    out
}

fn kick_badge_style(t: &str) -> (&'static str, u32) {
    match t {
        "broadcaster" => ("◉", 0xfb2c36),
        "moderator" => ("⚔", 0x00bcff),
        "verified" => ("✓", 0x51a2ff),
        "vip" => ("◆", 0xe12afb),
        "og" => ("★", 0x00bc7d),
        "founder" => ("⚑", 0xfe9a00),
        "subscriber" => ("★", 0x7ccf00),
        "sub_gifter" => ("♥", 0xad46ff),
        "staff" => ("⚙", 0xd4d4d8),
        "trainee" => ("◐", 0x00d5be),
        "bot" => ("⌬", 0x9f9fa9),
        _ => ("", 0xffffff),
    }
}

pub fn kick_row(row: &Row, m: &KickMsg, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let striking = m.deleted && rs.deleted_messages == "strike";
    let can_mod = env.caps.can_moderate && !m.user.id.is_empty() && !m.deleted;
    let can_delete = env.caps.can_delete && !m.deleted;
    let sk = row.sk.clone();
    let mut f = Flow::new(base);
    let mut actions = Vec::new();
    if ctx.mod_buttons && (can_mod || can_delete) {
        let can = |b: &ModButton| if b.action == "delete" { can_delete } else { b.action != "command" && b.action != "warn" && can_mod };
        let run = |b: &ModButton| RowAction::KickMod { sk: sk.clone(), action: match b.action.as_str() { "ban" => "ban", "timeout" => "timeout", _ => "delete" }, msg: Box::new(m.clone()), seconds: (b.action == "timeout").then_some(b.seconds) };
        mod_button_items(&mut f, ctx.mod_list, &can, &run, &mut actions);
    }
    frame_prefix(&mut f, ctx, m.ts, Platform::Kick);
    if rs.show_badges {
        for b in &m.badges {
            let label = if b.count > 0.0 { format!("{} ({})", if b.text.is_empty() { &b.r#type } else { &b.text }, b.count as i64) } else if b.text.is_empty() { b.r#type.clone() } else { b.text.clone() };
            if let Some(u) = b.url.as_ref().filter(|u| !u.is_empty()) {
                f.push(co_badge_img(u, &label, base));
                f.push(Item::Gap(4.0));
            } else {
                let (glyph, col) = kick_badge_style(&b.r#type);
                let g = if glyph.is_empty() { b.text.chars().chain(b.r#type.chars()).take(3).collect::<String>() } else { glyph.to_string() };
                simple_badge_chip(&mut f, &label, token(col), token_a(col, 0.2), &g);
            }
        }
    }
    actions.push(RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } });
    f.push(Item::Text(TextItem::new(if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() }, base, name_color_of(&m.user.color, rs.readable_colors)).weight(FontWeight::SEMIBOLD).act(Act::Tag(actions.len() as u32 - 1), true)));
    f.push(Item::Text(TextItem::new(": ", base, token(theme::MUTED))));
    if let Some(r) = &m.reply {
        f.push(Item::Pill(PillItem { text: format!("↩ {}", r.name).into(), size: 10.0, color: token(theme::FAINT), weight: FontWeight::NORMAL, bg: token_a(theme::WHITE, 0.05), pad_x: 4.0, margin_r: 4.0, radius: 4.0 }));
    }
    let col = if striking { token(theme::FAINT) } else if m.deleted && rs.deleted_messages != "show" { token(theme::FAINT) } else { token(theme::TEXT) };
    for (is_emote, a, name) in split_kick(&m.text) {
        if is_emote {
            plain_emote(&mut f, format!("https://files.kick.com/emotes/{a}/fullsize"), &name, "Kick", rs);
        } else {
            run_text(&mut f, &a, col, striking);
        }
    }
    let mut spec = frame(f.finish(), actions, ctx, row.ord);
    co_frame_style(&mut spec, rule);
    spec.hover = Hover {
        copy: Some(m.text.clone()),
        reply: (env.can_send && !m.deleted).then(|| ReplyTarget { sk: sk.clone(), platform: Platform::Kick, id: m.id.clone(), user: if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() }, text: m.text.clone() }),
        delete: can_delete.then(|| RowAction::KickMod { sk: sk.clone(), action: "delete", msg: Box::new(m.clone()), seconds: None }),
        pin: None,
    };
    BuiltRow::Frame(spec)
}

pub fn youtube_row(row: &Row, m: &YouTubeMsg, ctx: &RowCtx, rule: Option<&HighlightRule>) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let paid = m.paid.as_ref().filter(|p| !p.amount.is_empty());
    let mut f = Flow::new(base);
    let mut actions = Vec::new();
    frame_prefix(&mut f, ctx, m.ts, Platform::Youtube);
    if rs.show_badges {
        for b in &m.badges {
            if !b.url.is_empty() {
                f.push(co_badge_img(&b.url, &b.label, base));
                f.push(Item::Gap(4.0));
            } else {
                let (glyph, col) = match b.icon.as_str() {
                    "OWNER" => ("◉", 0xfe9a00),
                    "MODERATOR" => ("⚔", 0x00bcff),
                    "VERIFIED" => ("✓", 0xe4e4e7),
                    _ => ("", 0xa1a1aa),
                };
                let g = if glyph.is_empty() { b.label.chars().chain(b.icon.chars()).take(3).collect::<String>() } else { glyph.to_string() };
                simple_badge_chip(&mut f, &b.label, token(col), token_a(col, 0.2), &g);
            }
        }
    }
    actions.push(RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: String::new(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } });
    let name_col = with_alpha(token(theme::TEXT), 0.8);
    f.push(Item::Text(TextItem::new(if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() }, base, name_col).weight(FontWeight::SEMIBOLD).act(Act::Tag(0), true)));
    f.push(Item::Text(TextItem::new(": ", base, token(theme::MUTED))));
    if let Some(p) = paid {
        f.push(Item::Pill(PillItem { text: p.amount.clone().into(), size: 11.0, color: token(theme::WHITE), weight: FontWeight::BOLD, bg: token_a(theme::BLACK, 0.3), pad_x: 4.0, margin_r: 4.0, radius: 4.0 }));
    }
    if m.notice.as_deref() == Some("membership") {
        f.push(Item::Pill(PillItem { text: "member".into(), size: 10.0, color: token(0x5ee9b5), weight: FontWeight::SEMIBOLD, bg: token_a(0x00bc7d, 0.2), pad_x: 4.0, margin_r: 4.0, radius: 4.0 }));
    }
    if paid.map(|p| p.sticker).unwrap_or(false) && m.parts.is_empty() {
        f.push(Item::Text(TextItem::new("sent a Super Sticker", base, token(theme::MUTED)).italic(true)));
    } else {
        for p in &m.parts {
            if p.t == "emoji" {
                plain_emote(&mut f, p.u.clone(), &p.a, if p.custom { "YouTube member emote" } else { "YouTube" }, rs);
            } else {
                f.text(&p.v, token(theme::TEXT));
            }
        }
    }
    let mut spec = frame(f.finish(), actions, ctx, row.ord);
    if let Some(p) = paid {
        // Super Chats carry YouTube's own tier colour; a rule only adds its bar.
        spec.bg = (!p.bg.is_empty()).then(|| css_color(&p.bg));
        spec.border = Some(token_a(theme::WHITE, 0.10));
        spec.margin_y = 2.0;
        if let Some(r) = rule {
            spec.bar = Some(rule_style(r).0);
        }
    } else {
        co_frame_style(&mut spec, rule);
    }
    spec.hover = Hover { copy: Some(m.text.clone()), ..Default::default() };
    BuiltRow::Frame(spec)
}

pub fn x_row(row: &Row, m: &XMsg, ctx: &RowCtx, rule: Option<&HighlightRule>) -> BuiltRow {
    let base = ctx.rs.font_size;
    let mut f = Flow::new(base);
    frame_prefix(&mut f, ctx, m.ts, Platform::X);
    f.push(Item::Text(TextItem::new(if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() }, base, with_alpha(token(theme::TEXT), 0.8)).weight(FontWeight::SEMIBOLD).act(Act::Tag(0), true)));
    f.push(Item::Text(TextItem::new(": ", base, token(theme::MUTED))));
    text_run_items(&mut f, &m.text, token(theme::TEXT));
    let mut spec = frame(f.finish(), vec![RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } }], ctx, row.ord);
    co_frame_style(&mut spec, rule);
    spec.hover = Hover { copy: Some(m.text.clone()), ..Default::default() };
    BuiltRow::Frame(spec)
}

/// Just for the badge-name test below and the pinned card.
pub fn has_badge(tag: &str, name: &str) -> bool {
    badge_names(tag).contains(name)
}

#[allow(dead_code)]
fn _unused() -> (u8, u8, u8) {
    parse_hex("#000000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rumble_emotes_split_like_the_global_regex() {
        let emotes: RumbleEmotes = serde_json::from_value(serde_json::json!({ "r+cooldoge": { "file": "a.png" }, "c": { "file": "c.png" } })).unwrap();
        use RumblePart::*;
        assert_eq!(split_rumble_emotes("hey from rumble :r+cooldoge: yo", &emotes), vec![Text("hey from rumble "), Emote { name: "r+cooldoge", file: "a.png".into() }, Text(" yo")]);
        // a miss consumes its closing colon, so ":c:" is never formed from the tail of ":b:"
        assert_eq!(split_rumble_emotes("a:b:c:", &emotes), vec![Text("a:b:c:")]);
        assert_eq!(split_rumble_emotes("::c::", &emotes), vec![Text(":"), Emote { name: "c", file: "c.png".into() }, Text(":")]);
        assert_eq!(split_rumble_emotes("time 10:30: ok :c:", &emotes), vec![Text("time 10:30: ok "), Emote { name: "c", file: "c.png".into() }]);
        assert_eq!(split_rumble_emotes("no list :c:", &RumbleEmotes::new()), vec![Text("no list :c:")]);
        assert_eq!(split_rumble_emotes("", &emotes), vec![]);
    }

    #[test]
    fn emote_box_matches_the_css_numbers() {
        // 13px font, medium: 13*1.9 = 24.7 -> 25; vMargin = -round((25 - 18.85)/2) = -3
        let (px, max_w, vm) = emote_box(13.0, SizeOpt::Medium);
        assert_eq!((px, max_w, vm), (25.0, 60.0, -3.0));
        let (px, _, vm) = emote_box(13.0, SizeOpt::Small);
        assert_eq!(px, 19.0);
        assert_eq!(vm, 0.0);
    }

    #[test]
    fn kick_tokens_split() {
        let v = split_kick("gg [emote:37226:KEKW] lol");
        assert_eq!(v.len(), 3);
        assert!(v[1].0 && v[1].1 == "37226" && v[1].2 == "KEKW");
    }

    #[test]
    fn money_formats() {
        assert_eq!(money(500.0), "$5");
        assert_eq!(money(250.0), "$2.50");
    }
}
