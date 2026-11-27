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

use super::chat_text::{as_link, mention_word, split_rumble_emotes, sub_split, text_mentions_login, RumblePart};
use super::highlights::{parse_hex, rule_row_style};
use super::inline::*;
use super::link_card::{first_link, LinkCard};
use super::pane_render::thousands;
use super::mod_buttons::describe_mod_button;
use super::mod_caps::{badge_names, mod_reach, CoCaps, ModCaps, ModViewer};
use super::twitch::badges::{parse_badge_list, BadgeMap, BADGE_PX, BADGE_VMARGIN};
use super::twitch::colors::{name_color, readable_color};
use super::twitch::cosmetics::{paint_color_at, ChatterStyle, SevenTvDefs};
use super::twitch::emotes::*;
use super::twitch::events::{builtin_redeem, notice_kind, ChatEventKind, DEFAULT_EVENTS};
use super::twitch::parts::{build_kick_parts_in, build_parts, build_parts_in, split_keep_ws, split_kick, EmoteSource, KnownChatter, MsgPart};
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

/// Height of a 1x emote file on the CDNs (Twitch and BTTV 28 px, 7TV and FFZ up to 32): 2x is twice that, and so on.
const EMOTE_1X_PX: f32 = 28.0;

/// The smallest CDN density (1..=4) whose file covers an emote drawn `px` logical pixels tall at display `scale`,
/// so a 25 px emote on a 100% screen fetches the 1x file and the same emote at 125% the 2x one. The decode shrinks
/// a bigger file to the drawn size anyway; a smaller one would be blurry.
pub fn emote_density(px: f32, scale: f32) -> usize {
    let need = px * scale.max(1.0);
    (1..=4).find(|d| *d as f32 * EMOTE_1X_PX + 0.5 >= need).unwrap_or(4)
}

/// The display scale emote files are picked for (f32 bits): the highest window scale seen this session, so two
/// windows on screens with different scales never flip it back and forth.
static EMOTE_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub fn emote_scale_factor() -> f32 {
    f32::from_bits(EMOTE_SCALE.load(std::sync::atomic::Ordering::Relaxed))
}

/// A window draws at `scale`; raises the scale emote files are picked for when it is higher.
pub fn note_scale_factor(scale: f32) {
    if scale.is_finite() && scale > emote_scale_factor() {
        EMOTE_SCALE.store(scale.to_bits(), std::sync::atomic::Ordering::Relaxed);
    }
}

/// The density for an emote of the `size` option at font `font_px` on the current display scale.
fn density_for(font_px: f32, size: SizeOpt) -> usize {
    emote_density(emote_px(font_px, size), emote_scale_factor())
}

pub fn twitch_emote_url(id: &str, density: usize) -> String {
    let d = match density {
        0 | 1 => "1.0",
        2 => "2.0",
        _ => "3.0",
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
    /// the logged-in account's 7TV personal set (None with 7TV emotes off): its own lines draw it at once, before
    /// (and whether or not) the cosmetics lookup of the account answers
    pub my_personal: Option<Arc<EmoteMap>>,
}

#[derive(Clone)]
pub struct CoEnv {
    pub source: SourceRef,
    /// Kick: the channel's Kick-linked 7TV set and 7TV global, merged (None with 7TV emotes off or not loaded yet).
    pub emotes: Option<Arc<EmoteSets>>,
    pub caps: CoCaps,
    /// The conn frame says this account can post here (Kick replies need it).
    pub can_send: bool,
    pub rumble_badges: RumbleBadgeDefs,
    pub rumble_emotes: RumbleEmotes,
    /// the logged-in account on this platform, for who the mod buttons may act on
    pub viewer_login: String,
    /// the logged-in account's id on this platform (YouTube: its channel id)
    pub viewer_id: String,
    /// Kick: the account's username and slug, lowercased, for lines that mention or answer it
    pub me_names: Vec<String>,
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
    YoutubeMod { sk: String, action: &'static str, msg: Box<YouTubeMsg> },
    /// open the reply thread a reply belongs to (its parent and, on Twitch, the thread's first message)
    Thread { sk: String, parent: String, thread: String },
}

// ---- built rows -------------------------------------------------------------------------------------

/// The "↩ replying to @name: ..." line above a reply.
#[derive(Clone, Debug, PartialEq)]
pub struct ReplyLine {
    /// the line as shown (cut off with an ellipsis when long)
    pub text: String,
    /// the whole parent message, for the hover tooltip
    pub full: String,
    pub sk: String,
    pub parent: String,
    pub thread: String,
}

impl ReplyLine {
    pub fn new(sk: &str, user: &str, text: &str, parent: &str, thread: &str) -> ReplyLine {
        let who = if user.is_empty() { String::new() } else { format!("@{user}: ") };
        ReplyLine { text: format!("↩ replying to {who}{text}"), full: format!("{who}{text}"), sk: sk.to_string(), parent: parent.to_string(), thread: thread.to_string() }
    }

    /// What a click does: open the thread, when the reply says which message it answers.
    pub fn action(&self) -> Option<RowAction> {
        (!self.parent.is_empty() || !self.thread.is_empty()).then(|| RowAction::Thread { sk: self.sk.clone(), parent: self.parent.clone(), thread: self.thread.clone() })
    }
}

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
    /// the "replying to" line above a reply: hover shows the whole parent, a click opens the thread
    pub before: Option<ReplyLine>,
    pub hover: Hover,
    /// the message's first link a preview card can be made for (settings.linkPreviews)
    pub link: Option<LinkCard>,
}

#[derive(Clone)]
pub struct EventSpec {
    pub content: Rc<InlineContent>,
    /// what the header's clickable items do (the name's card, mod buttons)
    pub actions: Vec<RowAction>,
    pub amount: Option<f64>,
    pub is_bits: bool,
    pub inner: Option<FrameSpec>,
    /// a picture on the right of the header (a KICKs gift's coin bundle)
    pub image: Option<String>,
}

#[derive(Clone)]
pub struct NoticeSpec {
    pub system: Rc<InlineContent>,
    pub text: Option<Rc<InlineContent>>,
    pub raid: bool,
    /// a sub / gift: its icon (and its size), drawn left of the lines
    pub icon: Option<(&'static str, f32)>,
    /// a sub / gift: the name line above the sentence, opening the user's card
    pub head: Option<Rc<InlineContent>>,
    pub card: Option<(String, CardUser)>,
    /// the left bar's colour when not the accent (a Super Chat's tier colour)
    pub bar: Option<Hsla>,
    /// a Reply button under it, putting @name in the message box
    pub reply: Option<CardUser>,
    /// a Twitch sub / streak / announcement's attached message, drawn as the sender's chat line under the
    /// sentence: badges, a name that opens the card, mod buttons and the hover tools
    pub line: Option<FrameSpec>,
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
    /// the held message as a chat line (`twitch_message_row` in held mode)
    pub msg: FrameSpec,
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
    /// Every inline flow the row draws.
    pub fn contents(&self) -> Vec<Rc<InlineContent>> {
        match self {
            BuiltRow::Frame(f) => vec![f.content.clone()],
            BuiltRow::Event(e) => {
                let mut v = vec![e.content.clone()];
                if let Some(i) = &e.inner {
                    v.push(i.content.clone());
                }
                v
            }
            BuiltRow::Notice(n) => {
                let mut v: Vec<_> = n.head.iter().cloned().collect();
                v.push(n.system.clone());
                if let Some(t) = &n.text {
                    v.push(t.clone());
                }
                if let Some(l) = &n.line {
                    v.push(l.content.clone());
                }
                v
            }
            BuiltRow::Automod(a) => vec![a.header.clone(), a.msg.content.clone()],
            BuiltRow::Feed(_) => vec![],
        }
    }

    /// The images of every flow: when one of them loads, the row is measured again (its size can change the
    /// line breaks). An AutoMod body, a sub's name line and an event's own line were missing here, so their rows
    /// kept the height measured before their emotes loaded.
    pub fn urls(&self) -> Vec<String> {
        let mut v: Vec<String> = self.contents().iter().flat_map(|c| c.urls()).collect();
        if let BuiltRow::Event(EventSpec { image: Some(u), .. }) = self {
            v.push(u.clone());
        }
        v
    }

    /// The preview card's link, when the row has one: its preview arriving changes the row's height.
    pub fn link(&self) -> Option<&LinkCard> {
        match self {
            BuiltRow::Frame(f) => f.link.as_ref(),
            BuiltRow::Event(e) => e.inner.as_ref().and_then(|f| f.link.as_ref()),
            BuiltRow::Notice(n) => n.line.as_ref().and_then(|f| f.link.as_ref()),
            _ => None,
        }
    }
}

// ---- inline item helpers ----------------------------------------------------------------------------

struct Flow {
    items: Vec<Item>,
    base: f32,
    /// item ranges painted as one span (`InlineContent::spans`)
    spans: Vec<(usize, usize)>,
}

impl Flow {
    fn new(base: f32) -> Self {
        Flow { items: Vec::new(), base, spans: Vec::new() }
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
        InlineContent::with_spans(self.items, self.base, LINE_HEIGHT, self.spans)
    }
}

fn emote_item(url: String, tip: Rc<Tip>, overlays: Vec<SharedStr>, font_px: f32, size: SizeOpt) -> Item {
    let (px, max_w, vm) = emote_box(font_px, size);
    // the popup shows the row's emote until its bigger image is in
    let tip = if tip.fallback.is_empty() && tip.img.as_ref() != url.as_str() { Rc::new(Tip { fallback: url.clone().into(), ..(*tip).clone() }) } else { tip };
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
                i.tip = Some(Rc::new(Tip::new("", t.to_string(), "")));
            }
        }
        self
    }
}

fn link_item(href: &str, text: &str, base: f32) -> Item {
    Item::Text(TextItem::new(text.to_string(), base, token(theme::ACCENT_SOFT)).act(Act::Url(href.to_string().into()), false))
}

/// MessageText: the message's parts as inline items.
fn message_items(f: &mut Flow, parts: &[MsgPart], size: SizeOpt, emotes: EmoteLookup, text_color: Hsla, strike: bool, readable_mentions: bool, mut mention_acts: Option<(&mut Vec<RowAction>, &str)>) {
    let base = f.base;
    let density = density_for(base, size);
    for p in parts {
        match p {
            MsgPart::Text(t) => {
                // a removed message: greyed out, like 7TV (not struck through, which hides ASCII art)
                let mut ti = TextItem::new(t.clone(), base, text_color);
                if strike {
                    ti.color = with_alpha(text_color, 0.45);
                }
                f.push(Item::Text(ti));
            }
            MsgPart::Mention { name, color, login, user_id } => {
                let c = if color.is_empty() { text_color } else if readable_mentions { hex(&readable_color(color)) } else { hex(color) };
                let mut t = TextItem::new(format!("@{name}"), base, if strike { with_alpha(c, 0.45) } else { c }).weight(FontWeight::SEMIBOLD);
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
                let tip = Rc::new(Tip::new(url.clone(), name.clone(), format!("{prefix} cheermote")));
                f.push(emote_item(url.clone(), tip, vec![], base, size));
                let c = if color.is_empty() { text_color } else { hex(color) };
                f.push(Item::Text(TextItem::new(format!("{amount}"), base, if strike { with_alpha(c, 0.45) } else { c }).weight(FontWeight::BOLD)));
            }
            MsgPart::Link { href, text } => f.push(link_item(href, text, base)),
            MsgPart::Emoji { text, overlays } => {
                let mut ti = TextItem::new(text.clone(), base, text_color).tip(emoji_tip(text));
                if strike {
                    ti.color = with_alpha(text_color, 0.45);
                }
                if !overlays.is_empty() {
                    // zero-width emotes on the emoji: drawn over its centre, fitted like on an emote
                    let (px, max_w, _) = emote_box(base, size);
                    ti.overlays = overlays
                        .iter()
                        .map(|o| {
                            let oe = emotes.get(&o.name).cloned();
                            emote_part_url(o.source, &o.id, density, oe.as_deref()).into()
                        })
                        .collect();
                    ti.overlay_box = (max_w, px);
                }
                f.push(Item::Text(ti));
            }
            MsgPart::Emote { source, id, name, overlays } => {
                let entry = match source {
                    EmoteSource::Twitch | EmoteSource::Kick => None,
                    EmoteSource::Provider(_) => emotes.get(name).cloned(),
                };
                let url = emote_part_url(*source, id, density, entry.as_deref());
                let mut tip = emote_tip(*source, id, name, false, entry.as_deref());
                if matches!(source, EmoteSource::Provider(EmoteProvider::SevenTv)) && emotes.is_personal(name) {
                    tip.label = "7TV Personal".into();
                }
                let tip = Rc::new(tip);
                let ov: Vec<SharedStr> = overlays
                    .iter()
                    .map(|o| {
                        let oe = emotes.get(&o.name).cloned();
                        emote_part_url(o.source, &o.id, density, oe.as_deref()).into()
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
    message_items(&mut f, parts, size, EmoteLookup::new(emotes, None), color, false, true, None);
    f.items
}

fn emote_part_url(source: EmoteSource, id: &str, density: usize, entry: Option<&ProviderEmote>) -> String {
    match source {
        EmoteSource::Twitch => twitch_emote_url(id, density),
        EmoteSource::Kick => kick_emote_url(id),
        _ => entry.map(|e| provider_emote_url_at(e, density)).unwrap_or_default(),
    }
}

fn emote_tip(source: EmoteSource, id: &str, name: &str, zero_width: bool, entry: Option<&ProviderEmote>) -> Tip {
    let (img, label) = match source {
        EmoteSource::Twitch => (format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/default/dark/3.0"), "Twitch".to_string()),
        EmoteSource::Kick => (kick_emote_url(id), "Kick".to_string()),
        EmoteSource::Provider(p) => (entry.map(provider_tooltip_url).unwrap_or_default(), format!("{}{}", p.label(), if zero_width { " zero-width" } else { "" })),
    };
    Tip::new(img, name.to_string(), label)
}

/// The popup of a Unicode emoji: the emoji drawn large, its name, its shortcode and "Emoji".
fn emoji_tip(e: &str) -> Tip {
    let (name, short) = super::twitch::emoji::emoji_names(e);
    let label = match short {
        Some(s) => format!(":{s}: \u{b7} Emoji"),
        None => "Emoji".to_string(),
    };
    Tip { glyph: e.to_string().into(), ..Tip::new("", name, label) }
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
        link: None,
    }
}

/// The preview card of a message: its first previewable link, unless the setting is off or the message is gone.
fn with_link(spec: &mut FrameSpec, ctx: &RowCtx, removed: bool) {
    spec.link = if ctx.rs.link_previews && !removed { first_link(&spec.content) } else { None };
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
        // tint closer to twitch.tv's first-message row (was .08, which read too dark)
        return h((0x91, 0x47, 0xff), (145, 71, 255, 0.18), "FIRST MESSAGE", theme::PURPLE_300);
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
    /// An AutoMod hold drawn as a chat line inside its card: not in chat yet, so nothing to delete, pin or reply
    /// to, no highlight of its own (the card is the highlight) and never a redeem / cheer frame.
    pub held: bool,
}

/// The ChatMsg an AutoMod hold is drawn (and moderated) as. `seen` is the user's last chat line here, if any:
/// EventSub's hold carries no badges or colour, so they come from there (or from the known chatters' colour).
pub fn held_chat_msg(h: &HeldMsg, seen: Option<&ChatMsg>, known_color: Option<&str>) -> ChatMsg {
    let color = seen.map(|m| m.user.color.clone()).filter(|c| !c.is_empty()).or_else(|| known_color.map(str::to_string)).unwrap_or_default();
    ChatMsg {
        id: h.id.clone(),
        ts: h.ts,
        user: ChatUser { id: h.user.id.clone(), login: h.user.login.clone(), name: if h.user.name.is_empty() { h.user.login.clone() } else { h.user.name.clone() }, color },
        badges: seen.map(|m| m.badges.clone()).unwrap_or_default(),
        badge_info: seen.map(|m| m.badge_info.clone()).unwrap_or_default(),
        text: h.text.clone(),
        emotes: h.emotes.clone(),
        ..Default::default()
    }
}

/// The ChatMsg a redeem without text acts on (its mod buttons): the redeemer, badges from their last line here.
pub fn redeemer_chat_msg(n: &Notice, seen: Option<&ChatMsg>) -> ChatMsg {
    ChatMsg {
        id: String::new(),
        ts: n.ts,
        user: ChatUser { color: seen.map(|m| m.user.color.clone()).filter(|c| !c.is_empty()).unwrap_or_else(|| n.user.color.clone()), ..n.user.clone() },
        badges: seen.map(|m| m.badges.clone()).unwrap_or_default(),
        ..Default::default()
    }
}

/// The inline mod buttons for a Twitch user's line, by what this account may do to them. `msg` is what the
/// buttons act on; `can_delete` is false where there is no chat message to delete.
fn twitch_mod_buttons(f: &mut Flow, ctx: &RowCtx, env: &TwitchEnv, sk: &str, msg: &ChatMsg, can_delete: bool, actions: &mut Vec<RowAction>) {
    let reach = mod_reach(&env.viewer, &msg.user.id, &msg.badges);
    let caps = &env.caps;
    let (can_ban, can_timeout) = (caps.ban && reach.ban, caps.ban && reach.timeout);
    let is_mod = caps.ban || caps.delete || caps.warn;
    let can = |b: &ModButton| match b.action.as_str() {
        "ban" => can_ban && !msg.deleted,
        "timeout" => can_timeout && !msg.deleted,
        "warn" => caps.warn && reach.warn && !msg.deleted,
        "delete" => can_delete,
        _ => is_mod,
    };
    let run = |b: &ModButton| match b.action.as_str() {
        "ban" => RowAction::Popover { sk: sk.to_string(), msg: Box::new(msg.clone()), kind: "ban" },
        "timeout" => RowAction::QuickTimeout { sk: sk.to_string(), msg: Box::new(msg.clone()), seconds: b.seconds },
        "warn" => RowAction::Popover { sk: sk.to_string(), msg: Box::new(msg.clone()), kind: "warn" },
        "delete" => RowAction::Delete { sk: sk.to_string(), id: msg.id.clone() },
        _ => RowAction::TwitchCommand { sk: sk.to_string(), msg: Box::new(msg.clone()), template: b.command.clone() },
    };
    mod_button_items(f, ctx.mod_list, &can, &run, actions);
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
    let TwitchInput { row, msg, ctx, env, known, susp, rule, style, defs, held } = inp;
    let held = *held;
    let rs = ctx.rs;
    let base = rs.font_size;
    let size = SizeOpt::parse(&rs.emote_size);
    let cosmetics_on = rs.seven_tv_cosmetics;
    let paint = if cosmetics_on { style.and_then(|s| s.paint_id.as_ref()).and_then(|id| defs.and_then(|d| d.paints.get(id))) } else { None };
    let seven_badge = if cosmetics_on { style.and_then(|s| s.badge_id.clone()).and_then(|id| defs.and_then(|d| d.badges.get(&id)).map(|b| (id, b.clone()))) } else { None };

    let color = hex(&name_color(&msg.user, rs.readable_colors));
    let mentions_me = text_mentions_login(&msg.text, &env.me_login_lower);
    let hl = row_highlight(msg, mentions_me, *susp, *rule);
    // the sender's 7TV personal set: the account's own (loaded at login) on its own lines, else what the
    // chatter's 7TV lookup found; only with 7TV emotes on
    let personal = if !rs.seven_tv {
        None
    } else if !env.viewer.id.is_empty() && msg.user.id == env.viewer.id && env.my_personal.is_some() {
        env.my_personal.as_deref()
    } else {
        style.and_then(|s| s.personal.as_deref())
    };
    let emotes = EmoteLookup::new(env.emotes.as_deref(), personal);
    let built = build_parts_in(&msg.text, &msg.emotes, emotes, Some(known), msg.gifs.as_deref().unwrap_or(""));
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
    // a held message is not in chat: nothing to delete until it is allowed (then it comes in as a normal line)
    let can_delete = caps.delete && reach.delete && !msg.deleted && !held;
    let struck = msg.deleted && rs.deleted_messages == "strike";
    let sk = row.sk.clone();

    let mut f = Flow::new(base);
    let mut actions: Vec<RowAction> = Vec::new();
    if ctx.mod_buttons {
        twitch_mod_buttons(&mut f, ctx, env, &sk, msg, can_delete, &mut actions);
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
        // A 7TV paint covers the whole message as one span (Electron puts the name's paint style on the
        // message's <span>), not one gradient per word.
        let mark = f.items.len();
        message_items(&mut f, &parts, size, emotes, color, struck, rs.readable_colors, Some((&mut actions, &sk)));
        let spec = paint.and_then(super::paint_text::PaintSpec::from_paint).map(Rc::new);
        for it in f.items[mark..].iter_mut() {
            if let Item::Text(t) = it {
                t.weight = FontWeight::MEDIUM;
                if let Some(p) = paint {
                    t.gradient = paint_gradient(p, color);
                    t.paint = spec.clone();
                }
            }
        }
        if spec.is_some() && f.items.len() > mark {
            f.spans.push((mark, f.items.len()));
        }
    } else {
        message_items(&mut f, &parts, size, emotes, token(theme::TEXT), struck, rs.readable_colors, Some((&mut actions, &sk)));
    }
    if struck {
        let note = msg.deleted_note.clone().unwrap_or_else(|| "Deleted".into());
        f.push(Item::Text(TextItem::new(format!("\u{2014}{note}"), base * 0.9, token(theme::FAINT)).italic(true)));
    }

    let content = f.finish();
    let mut spec = frame(content, actions, ctx, row.ord);
    if held {
        // drawn inside the AutoMod card, which is its highlight; Copy is the one hover tool that applies
        spec.alt = false;
        spec.hover = Hover { copy: Some(msg.text.clone()), ..Hover::default() };
        return BuiltRow::Frame(spec);
    }
    with_link(&mut spec, ctx, msg.deleted);
    let highlighted = msg.msg_id.as_deref() == Some("highlighted-message");
    spec.bar = hl.bar;
    spec.bg = hl.bg;
    spec.label = hl.label;
    if highlighted && spec.bg.is_none() {
        spec.tint_class = Some(token_a(theme::PURPLE_500, 0.10));
    }
    spec.before = msg.reply.as_ref().map(|r| ReplyLine::new(&sk, &r.user, &r.text, &r.parent_id, &r.thread_id));
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
    let who = EventWho { sk: &sk, user: &msg.user, color: frame_color, buttons: None };
    if (msg.reward_id.is_some() || builtin.is_some()) && !msg.deleted && (if is_bits { ev.cheers } else { ev.redeems }) {
        let reward_title = builtin.map(|b| b.0.to_string()).or_else(|| msg.reward.as_ref().map(|r| r.title.clone()));
        let amount = if builtin.is_some() { None } else { msg.reward.as_ref().and_then(|r| r.bits.filter(|b| *b != 0.0).or(r.cost)) };
        return BuiltRow::Event(event_frame(&who, "redeemed", reward_title, amount, is_bits, if is_bits { "cheer" } else { "redeem" }, Some(spec), base));
    }
    if let Some(b) = msg.bits.filter(|b| *b != 0.0) {
        if ev.cheers && !msg.deleted {
            return BuiltRow::Event(event_frame(&who, "cheered", None, Some(b), true, "cheer", Some(spec), base));
        }
    }
    BuiltRow::Frame(spec)
}

/// Who a redeem / cheer header names: their name opens their card. `buttons` puts the mod buttons in front of
/// it, for a redeem with no chat line under it to carry them.
struct EventWho<'a> {
    sk: &'a str,
    user: &'a ChatUser,
    color: Hsla,
    buttons: Option<(&'a RowCtx<'a>, &'a TwitchEnv, &'a ChatMsg)>,
}

fn event_frame(who: &EventWho, verb: &str, reward: Option<String>, amount: Option<f64>, is_bits_hint: bool, tone: &str, inner: Option<FrameSpec>, base: f32) -> EventSpec {
    let mut f = Flow::new(base);
    let mut actions: Vec<RowAction> = Vec::new();
    if let Some((ctx, env, msg)) = who.buttons {
        if ctx.mod_buttons {
            twitch_mod_buttons(&mut f, ctx, env, who.sk, msg, false, &mut actions);
        }
    }
    let u = who.user;
    let mut name = TextItem::new(u.name.clone(), base, who.color).weight(FontWeight::SEMIBOLD);
    if !u.id.is_empty() || !u.login.is_empty() {
        actions.push(RowAction::Card { sk: who.sk.to_string(), user: CardUser { id: u.id.clone(), login: u.login.clone(), name: u.name.clone(), avatar: String::new() } });
        name = name.act(Act::Tag(actions.len() as u32 - 1), true);
    }
    f.push(Item::Text(name));
    f.push(Item::Text(TextItem::new(format!(" {verb}"), base, token(theme::MUTED))));
    if let Some(r) = reward {
        f.push(Item::Text(TextItem::new(" ", base, token(theme::MUTED))));
        f.push(Item::Text(TextItem::new(r, base, token(theme::TEXT)).weight(FontWeight::SEMIBOLD)));
    }
    let is_bits = is_bits_hint || (tone == "cheer" && amount.is_none());
    EventSpec { content: f.finish(), actions, amount, is_bits, inner, image: None }
}

// ---- notices ----------------------------------------------------------------------------------------

/// `seen` is the notice's user's last chat line here (badges for a redeem's mod buttons).
/// The chat line a Twitch notice's attached message is drawn (and moderated) as: the sender's badges from the
/// notice (or their last line here), never a redeem / cheer frame of its own.
pub fn notice_chat_msg(n: &Notice, seen: Option<&ChatMsg>) -> ChatMsg {
    let color = Some(n.user.color.clone()).filter(|c| !c.is_empty()).or_else(|| seen.map(|m| m.user.color.clone())).unwrap_or_default();
    let (badges, badge_info) = if n.badges.is_empty() { seen.map(|m| (m.badges.clone(), m.badge_info.clone())).unwrap_or_default() } else { (n.badges.clone(), n.badge_info.clone()) };
    ChatMsg {
        id: n.id.clone(),
        ts: n.ts,
        user: ChatUser { color, name: if n.user.name.is_empty() { n.user.login.clone() } else { n.user.name.clone() }, ..n.user.clone() },
        badges,
        badge_info,
        text: n.text.clone(),
        emotes: n.emotes.clone().unwrap_or_default(),
        deleted: n.deleted,
        deleted_note: n.deleted_note.clone(),
        ..Default::default()
    }
}

/// `line`: the attached message already built as a chat line (`notice_chat_msg`); without it the message is
/// drawn as plain text under the sentence.
pub fn notice_row(row: &Row, n: &Notice, ctx: &RowCtx, env: &TwitchEnv, seen: Option<&ChatMsg>, line: Option<FrameSpec>) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let size = SizeOpt::parse(&rs.emote_size);
    if let Some(s) = &n.seventv {
        return seventv_notice(s, base, size);
    }
    if (n.msg_id == "redeem" || n.msg_id == "powerup") && n.reward.is_some() {
        // A redeem with no message: the same header as one with a message, just nothing under it. Its name opens
        // the card and, in mod mode, the timeout / ban buttons sit in front of it (there is no line to delete).
        let r = n.reward.as_ref().unwrap();
        let bits = r.bits.unwrap_or(0.0) != 0.0;
        let amount = if bits { r.bits } else { r.cost };
        let target = redeemer_chat_msg(n, seen);
        let color = hex(&name_color(&target.user, rs.readable_colors));
        let who = EventWho { sk: &row.sk, user: &target.user, color, buttons: Some((ctx, env, &target)) };
        return BuiltRow::Event(event_frame(&who, "redeemed", Some(r.title.clone()), amount, bits, if bits { "cheer" } else { "redeem" }, None, base));
    }
    let kind = notice_kind(&n.msg_id, n.source_msg_id.as_deref());
    let raid = kind == Some(ChatEventKind::Raids);
    let id = crate::chat::twitch::events::notice_id(&n.msg_id, n.source_msg_id.as_deref());
    let mut sys = Flow::new(base);
    let mut icon = None;
    let mut head = None;
    let mut card = None;
    match (matches!(kind, Some(ChatEventKind::Subs | ChatEventKind::Gifts)), sub_split(&n.system, &n.user)) {
        (true, Some((rest, bold))) => {
            // like 7TV: an icon, the name on its own line, then Twitch's sentence without it
            let mystery = id.ends_with("submysterygift");
            icon = Some(match kind {
                Some(ChatEventKind::Gifts) => (crate::icons::GIFT, if mystery { 24.0 } else { 16.0 }),
                _ => (if n.system.contains("with Prime") { crate::icons::CROWN } else { crate::icons::STAR }, 16.0),
            });
            let color = if mystery { token(theme::TEXT) } else { hex(&name_color(&n.user, rs.readable_colors)) };
            let mut h = Flow::new(base);
            h.push(Item::Text(TextItem::new(n.user.name.clone(), base, color).weight(FontWeight::BOLD)));
            head = Some(h.finish());
            card = Some((row.sk.clone(), CardUser { id: n.user.id.clone(), login: n.user.login.clone(), name: n.user.name.clone(), avatar: String::new() }));
            for (t, b) in [(&rest[..bold.0], false), (&rest[bold.0..bold.1], true), (&rest[bold.1..], false)] {
                if !t.is_empty() {
                    let it = TextItem::new(t.to_string(), base, token(theme::TEXT));
                    sys.push(Item::Text(if b { it.weight(FontWeight::BOLD) } else { it }));
                }
            }
        }
        _ => sys.push(Item::Text(TextItem::new(n.system.clone(), base, token(if raid { theme::ORANGE_200 } else { theme::TEXT })))),
    }
    let text = if n.text.is_empty() || line.is_some() {
        None
    } else {
        let parts = build_parts(&n.text, n.emotes.as_deref().unwrap_or(""), env.emotes.as_deref(), None, "");
        let mut f = Flow::new(base);
        message_items(&mut f, &parts, size, EmoteLookup::new(env.emotes.as_deref(), None), token(theme::MUTED), false, rs.readable_colors, None);
        Some(f.finish())
    };
    BuiltRow::Notice(NoticeSpec { system: sys.finish(), text, raid, icon, head, card, bar: None, reply: None, line })
}

/// The app's notice of a live 7TV change, one compact line: "A_g_u__ added the 7TV emote [image] PotatWind".
fn seventv_notice(s: &SevenTvNote, base: f32, size: SizeOpt) -> BuiltRow {
    let (text, muted) = (token(theme::TEXT), token(theme::MUTED));
    let mut f = Flow::new(base);
    let bold = |f: &mut Flow, t: &str| f.push(Item::Text(TextItem::new(t.to_string(), base, text).weight(FontWeight::SEMIBOLD)));
    let verb = match s.action.as_str() {
        "added" => "added the 7TV emote",
        "removed" => "removed the 7TV emote",
        "renamed" => "renamed the 7TV emote",
        _ => "switched the channel's 7TV emote set to",
    };
    if s.actor.is_empty() {
        let mut c = verb.chars();
        let cap = c.next().map(|x| x.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default();
        f.text(&format!("{cap} "), muted);
    } else {
        bold(&mut f, &s.actor);
        f.text(&format!(" {verb} "), muted);
    }
    if !s.emote_id.is_empty() {
        let e = super::twitch::emotes::seven_tv_emote(&serde_json::json!({"id": s.emote_id, "name": s.name}));
        let tip = Rc::new(Tip::new(provider_tooltip_url(&e), s.name.clone(), "7TV"));
        f.push(emote_item(provider_emote_url_at(&e, density_for(base, size)), tip, vec![], base, size));
        f.text(" ", muted);
    }
    if s.action == "renamed" && !s.old_name.is_empty() {
        bold(&mut f, &s.old_name);
        f.text(" to ", muted);
    }
    bold(&mut f, &s.name);
    BuiltRow::Notice(NoticeSpec { system: f.finish(), text: None, raid: false, icon: None, head: None, card: None, bar: Some(token_a(theme::SLATE_500, 0.9)), reply: None, line: None })
}

pub fn feed_row(f: &ModFeedEntry, ctx: &RowCtx) -> BuiltRow {
    let time = ctx.rs.timestamps.then(|| fmt_time(f.ts, false));
    BuiltRow::Feed(FeedSpec { time, text: f.text.clone(), base: ctx.rs.font_size })
}

/// The AutoMod card: its header (what was held and why), the held message as a chat line (`inp.msg`, from
/// `held_chat_msg`: badges, a name that opens the card, emotes, the mod buttons that apply), then Allow / Deny.
pub fn automod_row(inp: &TwitchInput, h: &HeldMsg) -> BuiltRow {
    let (row, ctx, env) = (inp.row, inp.ctx, inp.env);
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
    // who sent it is the line below (badges, coloured name that opens the card)
    head.push(Item::Text(TextItem::new("AutoMod held a message", hs, token(theme::AMBER_200))));
    let msg = match twitch_message_row(&TwitchInput { held: true, ..*inp }) {
        BuiltRow::Frame(f) => f,
        _ => unreachable!("a held line is always a plain frame"),
    };
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
        msg,
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
        tip: Some(Rc::new(Tip::new(src, alt.to_string(), tip_label.to_string()))),
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
    f.push(Item::Pill(PillItem { text: glyph.to_string().into(), size: 10.0, color: fg, weight: FontWeight::BOLD, bg, pad_x: 3.0, margin_r: 4.0, radius: 4.0, copy: false }));
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

pub(super) fn rumble_badge_icon(def: &Value) -> Option<String> {
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
        tip: Some(Rc::new(Tip::new("", title.to_string(), ""))),
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
    let target = if m.user.login.is_empty() { &m.user.name } else { &m.user.login };
    let badges: Vec<&str> = m.badges.iter().map(|b| b.as_str()).collect();
    let can_mod = env.caps.can_moderate && !m.deleted && super::mod_caps::co_reach(Platform::Rumble, &env.viewer_login, &env.source.channel, target, &badges).ban;
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
        f.push(Item::Pill(PillItem { text: money(r.cents).into(), size: 11.0, color: token(theme::WHITE), weight: FontWeight::BOLD, bg: token_a(theme::BLACK, 0.3), pad_x: 4.0, margin_r: 4.0, radius: 4.0, copy: true }));
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
    with_link(&mut spec, ctx, m.deleted);
    BuiltRow::Frame(spec)
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

/// A Kick emote's picture (Kick serves one size).
fn kick_emote_url(id: &str) -> String {
    format!("https://files.kick.com/emotes/{id}/fullsize")
}

/// Glyph chip for Kick badge types without bundled art.
pub(super) fn kick_badge_style(t: &str) -> (&'static str, u32) {
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

/// The picture for a Kick badge: the channel's own art when it has some, else Kick's bundled art for the type.
pub(super) fn kick_badge_url(b: &KickBadge) -> Option<String> {
    agu_chat_core::kick::badges::badge_url(&b.r#type, b.count, b.url.as_deref())
}

/// Tooltip text of a Kick badge: kick.com's own words, which the backend puts on the badge.
pub(super) fn kick_badge_label(b: &KickBadge) -> String {
    if b.label.is_empty() {
        agu_chat_core::kick::badges::badge_label(&b.r#type, &b.text, b.count)
    } else {
        b.label.clone()
    }
}

/// The side of an event's picture (a KICKs gift's coin bundle), for a font size: a little over two lines.
pub fn event_image_px(base: f32) -> f32 {
    (base * 2.4).round()
}

/// Kick's green, the KICKs coin's colour on kick.com.
const KICKS_GREEN: u32 = 0x53fc18;

/// "1 viewer", "118 viewers"
fn viewers_text(n: f64) -> String {
    format!("{} {}", thousands(n), if n == 1.0 { "viewer" } else { "viewers" })
}

/// A KICKs gift, in the frame of a Twitch cheer: "<name> sent [K] 500 · Rage Quit" with the gift's coin bundle on the
/// right, and the sender's message under it as their own chat line (badges, a name that opens the card, the mod
/// buttons). The message can't be deleted or replied to on its own: on Kick it is part of the gift.
fn kick_gift_row(row: &Row, m: &KickMsg, g: &KickGift, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>, style: Option<&ChatterStyle>, defs: Option<&SevenTvDefs>) -> BuiltRow {
    let base = ctx.rs.font_size;
    let mut f = Flow::new(base);
    let actions = vec![RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } }];
    let shown = if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() };
    f.push(Item::Text(TextItem::new(shown, base, name_color_of(&m.user.color, ctx.rs.readable_colors)).weight(FontWeight::SEMIBOLD).act(Act::Tag(0), true)));
    f.push(Item::Text(TextItem::new(" sent ".to_string(), base, token(theme::MUTED))));
    let green = token(KICKS_GREEN);
    f.push(Item::Icon(IconItem { path: icons::KICKS, size: (base * 0.95).round(), color: green, valign: VAlign::BottomAt(0.12 * base), margin_x: 0.0 }));
    f.push(Item::Text(TextItem::new(format!(" {}", thousands(g.amount)), base, green).weight(FontWeight::SEMIBOLD)));
    if !g.name.is_empty() {
        f.push(Item::Text(TextItem::new(" \u{b7} ".to_string(), base, token(theme::MUTED))));
        f.push(Item::Text(TextItem::new(g.name.clone(), base, token(theme::TEXT)).weight(FontWeight::SEMIBOLD)));
    }
    let inner = if m.text.is_empty() {
        None
    } else {
        // the same line kick_row draws for a chat message (the `kicks` notice keeps it from deleting / replying)
        let line = KickMsg { kicks: None, ..m.clone() };
        match kick_row(row, &line, ctx, env, rule, style, defs) {
            BuiltRow::Frame(mut spec) => {
                spec.alt = false; // inside the gift's own tint
                Some(spec)
            }
            _ => None,
        }
    };
    BuiltRow::Event(EventSpec { content: f.finish(), actions, amount: None, is_bits: false, inner, image: g.image.clone().filter(|u| !u.is_empty()) })
}

/// A raid, like Twitch's raid notice (orange bar, orange text): "<name> has raided with 118 viewers." The name opens
/// the raider's card.
fn kick_host_row(row: &Row, m: &KickMsg, h: &KickHost, ctx: &RowCtx) -> BuiltRow {
    let base = ctx.rs.font_size;
    let orange = token(theme::ORANGE_200);
    let mut f = Flow::new(base);
    let shown = if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() };
    let card = (!m.user.id.is_empty() || !m.user.login.is_empty()).then(|| (row.sk.clone(), CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() }));
    let name = TextItem::new(shown, base, orange).weight(FontWeight::BOLD);
    f.push(Item::Text(if card.is_some() { name.act(Act::Tag(0), true) } else { name }));
    f.push(Item::Text(TextItem::new(" has raided with ".to_string(), base, orange)));
    f.push(Item::Text(TextItem::new(viewers_text(h.viewers.max(0.0)), base, orange).weight(FontWeight::BOLD)));
    f.push(Item::Text(TextItem::new(".".to_string(), base, orange)));
    let text = (!h.message.is_empty()).then(|| {
        let mut t = Flow::new(base);
        run_text(&mut t, &h.message, token(theme::MUTED), false);
        t.finish()
    });
    BuiltRow::Notice(NoticeSpec { system: f.finish(), text, raid: true, icon: None, head: None, card, bar: None, reply: None, line: None })
}

pub fn kick_row(row: &Row, m: &KickMsg, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>, style: Option<&ChatterStyle>, defs: Option<&SevenTvDefs>) -> BuiltRow {
    match (m.notice.as_deref(), &m.kicks, &m.host) {
        (Some("kicks"), Some(g), _) => return kick_gift_row(row, m, g, ctx, env, rule, style, defs),
        (Some("host"), _, Some(h)) => return kick_host_row(row, m, h, ctx),
        _ => {}
    }
    // a KICKs gift's message: part of the gift, not a chat message Kick can delete or answer
    let gift_line = m.notice.as_deref() == Some("kicks");
    let rs = ctx.rs;
    let paint = if rs.seven_tv_cosmetics { style.and_then(|s| s.paint_id.as_ref()).and_then(|id| defs.and_then(|d| d.paints.get(id))) } else { None };
    let seven_badge = if rs.seven_tv_cosmetics { style.and_then(|s| s.badge_id.clone()).and_then(|id| defs.and_then(|d| d.badges.get(&id)).map(|b| (id, b.clone()))) } else { None };
    let base = rs.font_size;
    let striking = m.deleted && rs.deleted_messages == "strike";
    let target = if m.user.login.is_empty() { &m.user.name } else { &m.user.login };
    let badges: Vec<&str> = m.badges.iter().map(|b| b.r#type.as_str()).collect();
    let can_mod = env.caps.can_moderate && !m.user.id.is_empty() && !m.deleted && super::mod_caps::co_reach(Platform::Kick, &env.viewer_login, &env.source.channel, target, &badges).ban;
    let can_delete = env.caps.can_delete && !m.deleted && !gift_line;
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
        // what kick.com's chat draws: the user's selected badges, at most four, in Kick's order
        for b in m.badges.iter().filter(|b| !b.hidden) {
            let label = kick_badge_label(b);
            if let Some(u) = kick_badge_url(b) {
                f.push(co_badge_img(&u, &label, base));
                f.push(Item::Gap(4.0));
            } else {
                let (glyph, col) = kick_badge_style(&b.r#type);
                let g = if glyph.is_empty() { b.text.chars().chain(b.r#type.chars()).take(3).collect::<String>() } else { glyph.to_string() };
                simple_badge_chip(&mut f, &label, token(col), token_a(col, 0.2), &g);
            }
        }
        if let Some((id, def)) = &seven_badge {
            let title = if def.tooltip.is_empty() { &def.name } else { &def.tooltip };
            f.push(co_badge_img(&format!("https://cdn.7tv.app/badge/{id}/3x"), title, base));
            f.push(Item::Gap(4.0));
        }
    }
    actions.push(RowAction::Card { sk: row.sk.clone(), user: CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: m.user.avatar.clone() } });
    let name_color = name_color_of(&m.user.color, rs.readable_colors);
    let mut name = TextItem::new(if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() }, base, name_color).weight(FontWeight::SEMIBOLD).act(Act::Tag(actions.len() as u32 - 1), true);
    if let Some(p) = paint {
        name.gradient = paint_gradient(p, name_color);
        name.paint = super::paint_text::PaintSpec::from_paint(p).map(Rc::new);
    }
    f.push(Item::Text(name));
    f.push(Item::Text(TextItem::new(": ", base, token(theme::MUTED))));
    let col = if striking { token(theme::FAINT) } else if m.deleted && rs.deleted_messages != "show" { token(theme::FAINT) } else { token(theme::TEXT) };
    // Kick's own emotes, plus 7TV's (the channel's Kick-linked set and global) with the same popup and zero-width
    // stacking as on Twitch; links and mentions in the text in between
    let size = SizeOpt::parse(&rs.emote_size);
    // (and the sender's 7TV personal set ahead of the channel's, as on Twitch)
    let personal = if rs.seven_tv { style.and_then(|s| s.personal.as_deref()) } else { None };
    let emotes = EmoteLookup::new(env.emotes.as_deref(), personal);
    for p in build_kick_parts_in(&m.text, emotes) {
        match &p {
            MsgPart::Text(t) => run_text(&mut f, t, col, striking),
            _ => message_items(&mut f, std::slice::from_ref(&p), size, emotes, col, striking, rs.readable_colors, None),
        }
    }
    let mut spec = frame(f.finish(), actions, ctx, row.ord);
    co_frame_style(&mut spec, rule);
    // a reply to you or your name in it: the same red frame and label as on Twitch
    if !gift_line && super::mentions::kick_mentions(m, &env.me_names) {
        let hl = row_highlight(&ChatMsg::default(), true, None, None);
        (spec.bar, spec.bg, spec.label) = (hl.bar, hl.bg, hl.label);
    }
    // like Twitch: the line it answers above it (emote codes as their names), the whole of it on hover
    spec.before = m.reply.as_ref().map(|r| {
        let text: String = split_kick(&r.text).into_iter().map(|(emote, a, name)| if emote { name } else { a }).collect();
        ReplyLine::new(&sk, &r.name, &text, &r.id, "")
    });
    spec.hover = Hover {
        copy: Some(m.text.clone()),
        reply: (env.can_send && !m.deleted && !gift_line).then(|| ReplyTarget { sk: sk.clone(), platform: Platform::Kick, id: m.id.clone(), user: if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() }, text: m.text.clone() }),
        delete: can_delete.then(|| RowAction::KickMod { sk: sk.clone(), action: "delete", msg: Box::new(m.clone()), seconds: None }),
        pin: None,
    };
    with_link(&mut spec, ctx, m.deleted);
    BuiltRow::Frame(spec)
}

pub fn youtube_row(row: &Row, m: &YouTubeMsg, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>) -> BuiltRow {
    let rs = ctx.rs;
    let base = rs.font_size;
    let paid = m.paid.as_ref().filter(|p| !p.amount.is_empty());
    if let Some(p) = paid {
        return super_chat_row(row, m, p, rs);
    }
    if m.notice.as_deref() == Some("membership") {
        return member_row(row, m, ctx, env, rule);
    }
    let can_mod = youtube_can_mod(m, env);
    let sk = row.sk.clone();
    let mut f = Flow::new(base);
    let mut actions = Vec::new();
    if ctx.mod_buttons && can_mod {
        let can = |b: &ModButton| b.action != "command" && b.action != "warn";
        let run = |b: &ModButton| RowAction::YoutubeMod { sk: sk.clone(), action: match b.action.as_str() { "ban" => "ban", "timeout" => "timeout", _ => "delete" }, msg: Box::new(m.clone()) };
        mod_button_items(&mut f, ctx.mod_list, &can, &run, &mut actions);
    }
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
        f.push(Item::Pill(PillItem { text: p.amount.clone().into(), size: 11.0, color: token(theme::WHITE), weight: FontWeight::BOLD, bg: token_a(theme::BLACK, 0.3), pad_x: 4.0, margin_r: 4.0, radius: 4.0, copy: true }));
    }
    if m.notice.as_deref() == Some("membership") {
        f.push(Item::Pill(PillItem { text: "member".into(), size: 10.0, color: token(0x5ee9b5), weight: FontWeight::SEMIBOLD, bg: token_a(0x00bc7d, 0.2), pad_x: 4.0, margin_r: 4.0, radius: 4.0, copy: false }));
    }
    if paid.map(|p| p.sticker).unwrap_or(false) && m.parts.is_empty() {
        f.push(Item::Text(TextItem::new("sent a Super Sticker", base, token(theme::MUTED)).italic(true)));
    } else {
        for p in &m.parts {
            if p.t == "emoji" {
                plain_emote(&mut f, p.u.clone(), &p.a, if p.custom { "YouTube member emote" } else { "YouTube" }, rs);
            } else {
                // links clickable (and previewable), like on the other platforms
                text_run_items(&mut f, &p.v, token(theme::TEXT));
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
    spec.hover = Hover { copy: Some(m.text.clone()), delete: can_mod.then(|| RowAction::YoutubeMod { sk: sk.clone(), action: "delete", msg: Box::new(m.clone()) }), ..Default::default() };
    with_link(&mut spec, ctx, false);
    BuiltRow::Frame(spec)
}

/// A Super Chat / Super Sticker drawn like a sub notice: the name, "Sent a Super Chat for $5", the message, a
/// Reply button; the left bar takes the Super Chat's own tier colour.
fn super_chat_row(row: &Row, m: &YouTubeMsg, p: &YouTubePaid, rs: &Settings) -> BuiltRow {
    let base = rs.font_size;
    let name = if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() };
    let user = CardUser { id: m.user.id.clone(), login: String::new(), name: name.clone(), avatar: m.user.avatar.clone() };
    let mut h = Flow::new(base);
    h.push(Item::Text(TextItem::new(name, base, token(theme::TEXT)).weight(FontWeight::BOLD)));
    let mut sys = Flow::new(base);
    sys.push(Item::Text(TextItem::new(if p.sticker { "Sent a Super Sticker for " } else { "Sent a Super Chat for " }, base, token(theme::TEXT))));
    sys.push(Item::Text(TextItem::new(p.amount.clone(), base, token(theme::TEXT)).weight(FontWeight::BOLD)));
    let text = (!m.parts.is_empty()).then(|| {
        let mut f = Flow::new(base);
        for part in &m.parts {
            if part.t == "emoji" {
                plain_emote(&mut f, part.u.clone(), &part.a, if part.custom { "YouTube member emote" } else { "YouTube" }, rs);
            } else {
                f.text(&part.v, token(theme::MUTED));
            }
        }
        f.finish()
    });
    let colour = [&p.bg, &p.header_bg].into_iter().find(|c| !c.is_empty()).map(|c| css_color(c));
    BuiltRow::Notice(NoticeSpec {
        system: sys.finish(),
        text,
        raid: false,
        icon: Some((crate::icons::COINS, 16.0)),
        head: Some(h.finish()),
        card: Some((row.sk.clone(), user)),
        bar: colour,
        reply: None,
        line: None,
    })
}

/// A new member or a membership milestone, drawn like a Twitch sub: a star, the name on its own line, YouTube's
/// sentence ("New member", "Member for 12 months · Knight"), and a milestone's message as the member's own chat
/// line under it (badges, a name that opens the card, mod buttons).
fn member_row(row: &Row, m: &YouTubeMsg, ctx: &RowCtx, env: &CoEnv, rule: Option<&HighlightRule>) -> BuiltRow {
    let base = ctx.rs.font_size;
    let name = if m.user.name.is_empty() { "unknown".to_string() } else { m.user.name.clone() };
    let user = CardUser { id: m.user.id.clone(), login: String::new(), name: name.clone(), avatar: m.user.avatar.clone() };
    let mut h = Flow::new(base);
    h.push(Item::Text(TextItem::new(name, base, token(0x2ba640)).weight(FontWeight::BOLD)));
    let member = m.member.clone().unwrap_or_default();
    // rows from before `member` existed carried the sub line as their text
    let has_message = m.member.is_some() && member.has_message && !m.parts.is_empty();
    let sub = if m.member.is_some() { member.sub.clone() } else { m.text.clone() };
    let mut sys = Flow::new(base);
    if !member.header.is_empty() {
        sys.push(Item::Text(TextItem::new(member.header.clone(), base, token(theme::TEXT)).weight(FontWeight::BOLD)));
        if !sub.is_empty() {
            sys.push(Item::Text(TextItem::new(format!(" · {sub}"), base, token(theme::TEXT))));
        }
    } else {
        sys.push(Item::Text(TextItem::new(if sub.is_empty() { "New member".to_string() } else { sub }, base, token(theme::TEXT)).weight(FontWeight::BOLD)));
    }
    let line = if has_message {
        let plain = YouTubeMsg { notice: None, member: None, ..m.clone() };
        match youtube_row(row, &plain, ctx, env, rule) {
            BuiltRow::Frame(mut f) => {
                f.alt = false;
                Some(f)
            }
            _ => None,
        }
    } else {
        None
    };
    BuiltRow::Notice(NoticeSpec {
        system: sys.finish(),
        text: None,
        raid: false,
        icon: Some((crate::icons::STAR, 16.0)),
        head: Some(h.finish()),
        card: Some((row.sk.clone(), user)),
        bar: None,
        reply: None,
        line,
    })
}

/// Whether a YouTube row gets mod buttons: a login, the message's menu params, not the channel's owner and not
/// the account's own message. Whether the account may moderate is YouTube's answer when a button is used.
fn youtube_can_mod(m: &YouTubeMsg, env: &CoEnv) -> bool {
    if !env.caps.connected || m.menu.as_deref().unwrap_or("").is_empty() || m.badges.iter().any(|b| b.icon == "OWNER") {
        return false;
    }
    let mine = if !env.viewer_id.is_empty() && !m.user.id.is_empty() {
        env.viewer_id == m.user.id
    } else {
        let me = env.viewer_login.trim().trim_start_matches('@').to_lowercase();
        !me.is_empty() && m.user.name.trim().trim_start_matches('@').to_lowercase() == me
    };
    !mine
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
    with_link(&mut spec, ctx, false);
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
    fn emote_box_matches_the_css_numbers() {
        // 13px font, medium: 13*1.9 = 24.7 -> 25; vMargin = -round((25 - 18.85)/2) = -3
        let (px, max_w, vm) = emote_box(13.0, SizeOpt::Medium);
        assert_eq!((px, max_w, vm), (25.0, 60.0, -3.0));
        let (px, _, vm) = emote_box(13.0, SizeOpt::Small);
        assert_eq!(px, 19.0);
        assert_eq!(vm, 0.0);
    }

    #[test]
    fn emote_files_are_the_smallest_that_cover_the_drawn_size() {
        let med = emote_px(13.0, SizeOpt::Medium); // 25 px
        let large = emote_px(13.0, SizeOpt::Large); // 33 px
        let small = emote_px(13.0, SizeOpt::Small); // 19 px
        assert_eq!(emote_density(med, 1.0), 1, "a 25 px emote on a 100% screen: the 28 px file");
        assert_eq!(emote_density(med, 1.25), 2);
        assert_eq!(emote_density(med, 1.5), 2);
        assert_eq!(emote_density(med, 2.0), 2);
        assert_eq!(emote_density(med, 2.5), 3);
        assert_eq!(emote_density(large, 1.0), 2, "drawn larger than 1x");
        assert_eq!(emote_density(small, 1.0), 1);
        assert_eq!(emote_density(small, 1.5), 1);
        assert_eq!(emote_density(small, 2.0), 2);
        assert_eq!(emote_density(28.0, 1.0), 1);
        assert_eq!(emote_density(200.0, 3.0), 4, "never past 4x");
        assert_eq!(emote_density(med, 0.5), 1, "a scale under 1 still gets 1x");
        assert_eq!(twitch_emote_url("25", 1), "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/1.0");
        assert_eq!(twitch_emote_url("25", 2), "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/2.0");
        assert_eq!(twitch_emote_url("25", 4), "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0");
    }

    /// A core Kick row's badges as the chat page reads them: kick.com's labels, Kick's own pictures, and the
    /// hidden ones (unselected, or past four) still there for mod rights.
    #[test]
    fn kick_badges_from_a_core_row() {
        let ev = serde_json::json!({"id": "1", "content": "hi", "sender": {"id": 5, "username": "ytry", "slug": "ytry", "identity": {"color": "#fff",
            "badges": [{"type": "moderator", "text": "Moderator", "sort_order": 12}],
            "badges_v2": [
                {"name": "level", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/47_a.png", "metadata": {"level": 47}, "selected": true, "sort_order": 1},
                {"name": "kick_founder22", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/kick_founder22_b.png", "selected": true, "sort_order": 2},
                {"name": "GoldenK", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/GoldenK_c.png", "selected": true, "sort_order": 4},
                {"name": "GOAT", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/GOAT_d.png", "selected": true, "sort_order": 4},
                {"name": "Ghost", "badge_type": "global", "image_url": "https://ext.cdn.kick.com/chat/badges/Ghost_e.png", "selected": false, "sort_order": 4}
            ]}}});
        let m: KickMsg = serde_json::from_value(agu_chat_core::kick::vendor::kick::to_kick_chat_msg(&ev)).unwrap();
        let shown: Vec<(String, Option<String>)> = m.badges.iter().filter(|b| !b.hidden).map(|b| (kick_badge_label(b), kick_badge_url(b))).collect();
        assert_eq!(
            shown,
            vec![
                ("Level 47".to_string(), Some("https://ext.cdn.kick.com/chat/badges/47_a.png?width=72,format=webp,quality=50".to_string())),
                ("KICK Founder 22".to_string(), Some("https://ext.cdn.kick.com/chat/badges/kick_founder22_b.png?width=72,format=webp,quality=50".to_string())),
                ("GoldenK".to_string(), Some("https://ext.cdn.kick.com/chat/badges/GoldenK_c.png?width=72,format=webp,quality=50".to_string())),
                ("GOAT".to_string(), Some("https://ext.cdn.kick.com/chat/badges/GOAT_d.png?width=72,format=webp,quality=50".to_string())),
            ]
        );
        // the moderator badge did not fit in kick.com's four, but mod rights still see it
        let types: Vec<&str> = m.badges.iter().map(|b| b.r#type.as_str()).collect();
        assert!(types.contains(&"moderator"));
        assert!(!super::super::mod_caps::co_reach(Platform::Kick, "somemod", "xqc", "ytry", &types).ban);
        assert!(super::super::mod_caps::co_reach(Platform::Kick, "xqc", "xqc", "ytry", &types).ban);
        // a channel badge without a picture of its own uses the bundled art, with kick.com's name
        let b = KickBadge { r#type: "bot".into(), ..Default::default() };
        assert_eq!(kick_badge_url(&b).as_deref(), Some("agu-asset:kick-badges/bot.svg"));
        assert_eq!(kick_badge_label(&b), "Bot");
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

#[cfg(test)]
mod held_tests {
    use super::*;
    use crate::chat::twitch::parts::build_parts;

    fn held() -> HeldMsg {
        serde_json::from_value(serde_json::json!({"id": "h1", "ts": 5, "user": {"id": "42", "login": "falconrogue9sr", "name": "FalconRogue9sr"}, "text": "bad Kappa", "emotes": "25:4-8", "reason": "blocked_term", "status": "held"})).unwrap()
    }

    #[test]
    fn a_hold_becomes_a_chat_line_with_its_emotes() {
        let h = held();
        let m = held_chat_msg(&h, None, None);
        assert_eq!((m.id.as_str(), m.ts, m.text.as_str(), m.emotes.as_str()), ("h1", 5, "bad Kappa", "25:4-8"));
        assert_eq!((m.user.id.as_str(), m.user.login.as_str(), m.user.name.as_str()), ("42", "falconrogue9sr", "FalconRogue9sr"));
        assert!(!m.deleted && m.reward_id.is_none() && m.reply.is_none());
        let parts = build_parts(&m.text, &m.emotes, None, None, "");
        assert!(parts.iter().any(|p| matches!(p, MsgPart::Emote { id, name, .. } if id == "25" && name == "Kappa")));
    }

    #[test]
    fn a_hold_takes_badges_and_colour_from_the_users_last_line() {
        let h = held();
        let seen = ChatMsg { badges: "subscriber/12,vip/1".into(), badge_info: "subscriber/14".into(), user: ChatUser { color: "#FF0000".into(), ..Default::default() }, ..Default::default() };
        let m = held_chat_msg(&h, Some(&seen), Some("#00FF00"));
        assert_eq!((m.badges.as_str(), m.badge_info.as_str(), m.user.color.as_str()), ("subscriber/12,vip/1", "subscriber/14", "#FF0000"));
        // no line seen: the known chatters' colour, no badges
        let m = held_chat_msg(&h, None, Some("#00FF00"));
        assert_eq!((m.badges.as_str(), m.user.color.as_str()), ("", "#00FF00"));
        // a hold without a display name shows the login
        let mut h2 = held();
        h2.user.name.clear();
        assert_eq!(held_chat_msg(&h2, None, None).user.name, "falconrogue9sr");
    }

    #[test]
    fn a_redeem_without_text_acts_on_its_redeemer() {
        let n: Notice = serde_json::from_value(serde_json::json!({"id": "r1", "ts": 9, "msgId": "redeem", "rewardId": "rw", "reward": {"title": "Hydrate", "cost": 100}, "user": {"id": "7", "login": "bob", "name": "Bob", "color": ""}})).unwrap();
        let seen = ChatMsg { badges: "moderator/1".into(), user: ChatUser { color: "#123456".into(), ..Default::default() }, ..Default::default() };
        let m = redeemer_chat_msg(&n, Some(&seen));
        assert_eq!((m.user.id.as_str(), m.user.login.as_str(), m.user.name.as_str(), m.user.color.as_str(), m.badges.as_str()), ("7", "bob", "Bob", "#123456", "moderator/1"));
        // nothing to delete: it has no message id
        assert!(m.id.is_empty());
        assert_eq!(n.reward_id.as_deref(), Some("rw"));
    }
}

#[cfg(test)]
mod kick_event_tests {
    use super::*;
    use agu_chat_core::kick::vendor::kick::{to_kick_gift_msg, to_kick_host_msg};

    fn text_of(c: &InlineContent) -> String {
        c.items.iter().filter_map(|i| if let Item::Text(t) = i { Some(t.text.to_string()) } else { None }).collect()
    }

    fn row_of(m: &KickMsg) -> Row {
        Row { key: format!("kick:xqc|k:{}", m.id), ts: m.ts, sk: "kick:xqc".into(), platform: Platform::Kick, ord: 1, backlog: false, body: RowBody::Kick(m.clone()) }
    }

    fn env() -> CoEnv {
        CoEnv {
            source: SourceRef { platform: Platform::Kick, channel: "xqc".into() },
            emotes: None,
            caps: CoCaps { connected: true, can_moderate: true, can_delete: true },
            can_send: true,
            rumble_badges: Default::default(),
            rumble_emotes: Default::default(),
            viewer_login: "xqc".into(),
            viewer_id: String::new(),
            me_names: Vec::new(),
        }
    }

    fn gift(i: usize) -> KickMsg {
        let all: Value = serde_json::from_str(include_str!("../../../core/src/kick/vendor/fixtures/kick_kicks_gifted.json")).unwrap();
        serde_json::from_value(to_kick_gift_msg(&all[i]["data"]).unwrap()).unwrap()
    }

    #[test]
    fn a_kicks_gift_with_a_message_is_a_header_over_the_senders_chat_line() {
        let rs = Settings::default();
        let buttons = super::super::mod_buttons::default_mod_buttons();
        let ctx = RowCtx { rs: &rs, show_platform: false, mod_buttons: true, mod_list: &buttons };
        let mut m = gift(0);
        m.badges = vec![KickBadge { r#type: "subscriber".into(), text: "Subscriber".into(), count: 3.0, ..Default::default() }];
        let BuiltRow::Event(e) = kick_row(&row_of(&m), &m, &ctx, &env(), None, None, None) else { panic!("not an event row") };
        assert_eq!(text_of(&e.content), "Gifter_One sent  500 \u{b7} Rage Quit");
        assert!(e.content.items.iter().any(|i| matches!(i, Item::Icon(ic) if ic.path == crate::icons::KICKS)));
        assert_eq!(e.image.as_deref(), Some("https://static.kick.com/kicks/gifts/500-kick-bundle.webp"));
        assert!(matches!(&e.actions[..], [RowAction::Card { user, .. }] if user.id == "140130904" && user.name == "Gifter_One"));
        assert_eq!((e.amount, e.is_bits), (None, false));
        // the message is the sender's own chat line: badges, name (card), timeout / ban, but no delete or reply
        let line = e.inner.expect("the message line");
        let t = text_of(&line.content);
        assert!(t.ends_with("Gifter_One: You're so bad at the game... but we here so"), "{t}");
        assert!(line.actions.iter().any(|a| matches!(a, RowAction::KickMod { action: "timeout", .. })));
        assert!(line.actions.iter().any(|a| matches!(a, RowAction::KickMod { action: "ban", .. })));
        assert!(!line.actions.iter().any(|a| matches!(a, RowAction::KickMod { action: "delete", .. })));
        assert!(line.actions.iter().any(|a| matches!(a, RowAction::Card { .. })));
        assert!(line.hover.delete.is_none() && line.hover.reply.is_none());
        assert_eq!(line.hover.copy.as_deref(), Some("You're so bad at the game... but we here so"));
        assert!(BuiltRow::Event(EventSpec { content: e.content.clone(), actions: vec![], amount: None, is_bits: false, inner: None, image: e.image.clone() }).urls().contains(&"https://static.kick.com/kicks/gifts/500-kick-bundle.webp".to_string()));
    }

    #[test]
    fn a_kicks_gift_without_a_message_is_only_the_header() {
        let rs = Settings::default();
        let ctx = RowCtx { rs: &rs, show_platform: false, mod_buttons: false, mod_list: &[] };
        for i in [1, 2] {
            let m = gift(i);
            let BuiltRow::Event(e) = kick_row(&row_of(&m), &m, &ctx, &env(), None, None, None) else { panic!("not an event row") };
            assert!(e.inner.is_none());
            assert!(text_of(&e.content).contains(" sent "));
        }
    }

    #[test]
    fn a_raid_is_an_orange_notice_whose_name_opens_the_card() {
        let rs = Settings::default();
        let ctx = RowCtx { rs: &rs, show_platform: false, mod_buttons: true, mod_list: &[] };
        let all: Value = serde_json::from_str(include_str!("../../../core/src/kick/vendor/fixtures/kick_stream_hosted.json")).unwrap();
        let m: KickMsg = serde_json::from_value(to_kick_host_msg(&all[0]["data"]).unwrap()).unwrap();
        let BuiltRow::Notice(n) = kick_row(&row_of(&m), &m, &ctx, &env(), None, None, None) else { panic!("not a notice row") };
        assert_eq!(text_of(&n.system), "PapaGambles has raided with 118 viewers.");
        assert!(n.raid);
        assert!(n.head.is_none() && n.icon.is_none() && n.text.is_none());
        let (sk, user) = n.card.expect("the raider's card");
        assert_eq!((sk.as_str(), user.name.as_str(), user.login.as_str()), ("kick:xqc", "PapaGambles", "papagambles"));
        assert!(matches!(&n.system.items[0], Item::Text(t) if matches!(t.act, Some(Act::Tag(0)))));
        assert_eq!(viewers_text(1.0), "1 viewer");
        assert_eq!(viewers_text(1234.0), "1,234 viewers");
    }
}

#[cfg(test)]
mod reply_line_tests {
    use super::*;

    #[test]
    fn the_reply_line_keeps_the_whole_parent_and_opens_its_thread() {
        let long = "word ".repeat(60);
        let l = ReplyLine::new("twitch:a", "Al", long.trim(), "p1", "root");
        assert!(l.text.starts_with("↩ replying to @Al: word"));
        assert_eq!(l.full, format!("@Al: {}", long.trim()));
        match l.action() {
            Some(RowAction::Thread { sk, parent, thread }) => assert_eq!((sk.as_str(), parent.as_str(), thread.as_str()), ("twitch:a", "p1", "root")),
            _ => panic!("no thread action"),
        }
        // a reply that names no parent (old backlog) only has the tooltip
        assert!(ReplyLine::new("twitch:a", "Al", "hi", "", "").action().is_none());
    }
}
