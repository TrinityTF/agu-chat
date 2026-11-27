//! Port of src/renderer/chat/ChatPane.tsx (+ MessageList.tsx's scrolling logic and useChatFeed.ts): the chat
//! view of one split. The props are the contract with layout/: `sources`, `mod_mode`, `focused`, the
//! per-source `status` (`onStatus`) and `PaneEvent::Activity` (`onActivity`).
//!
//! Round 2 scope: message list, rows, emotes, badges, cosmetics, highlights / ignores / search, mod buttons and
//! actions, pinned cards, alerts. A disabled input bar stands where `SendBox` will come; the user card, mod
//! popover, pop-out windows, channel modes and the settings dialog are later rounds.
//!
//! Differences from the DOM version, all forced by GPUI: the list is `gpui::list()` with `ListAlignment::Top`
//! and the stick-to-bottom is done here (a short chat then starts at the top like a scroll container does);
//! the hover / Alt pause freezes the *list contents* instead of the scroll position; hover state of a row's
//! toolbar is tracked by the pane because the toolbar overlaps the row above.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agu_chat_core::protocol::{AccountsState, AguBridge, ConnFrame, Frame, HighlightRule, Platform, Settings, SourceRef, SubId, Unsubscribe};
use futures::channel::mpsc::{unbounded, UnboundedSender};
use futures::StreamExt;
use gpui::{point, px, AnyElement, AppContext as _, Context, Entity, EventEmitter, Point, Pixels, Subscription, Window};
use super::chat_list::{ChatListState, ItemMetrics};
use serde_json::{json, Value};

use super::alerts::AlertWants;
use super::select::SelScope;
use super::mentions::Alert;
use super::PxF;
use super::highlights::{compile_highlights, match_input_of, Highlighter};
use super::ignores::{apply_ignores, compile_ignores, filter_ignored, ignore_login, toggle_ignored_user, CompiledIgnores, IgnoreCache};
use super::inline::Tip;
use super::mod_actions::{self, AutomodResult, ToastMsg};
use super::mod_buttons::{expand_template, format_duration, normalize_mod_buttons, TemplateVars};
use super::mod_caps::{badge_names, fetch_co_caps, fetch_mod_caps, mod_reach, relay_api, CoCaps, ModReach, ModViewer, REACH_ALL};
use super::rows::*;
use super::search::{is_empty_query, match_row, parse_search};
use super::services::Services;
use super::store::{source_key, ChatStore};
use super::twitch::cosmetics::{ChatterStyle, StyleLookup};
use super::twitch::emotes::{fetch_seven_tv_set, flush_cache, forget_cache, merge_emote_sets, set_retry_delay, seven_tv_channel_key_on, EmoteSets, ProviderScopedMap, SevenTvIds, SevenTvPlatform};
use super::twitch::events::{kick_event_visible, notice_visible};
use super::twitch::seven_events::{apply_changes, same_id, Dispatch, EmoteChange, SevenEvent, EMOTE_SET_UPDATE, USER_UPDATE};
use super::twitch::use_twitch_data::{load_badges, load_cached_provider_sets, load_kick_provider_sets, load_provider_sets, reload_seven_tv, ProviderSets, TwitchData};
use super::types::*;
use crate::layout::tree::{is_whisper, is_whisper_key};
use crate::text_input::{InputStyle, TextInput, TextInputEvent};

/// Tabs that got live chat while not selected: "message" (a subtle marker) or "highlight" (a strong one).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Message,
    Highlight,
}

pub enum PaneEvent {
    /// A source's `conn` state changed (the split header shows it).
    Status,
    /// Live chat arrived (drives the tab's unread / highlight marker).
    Activity(Activity),
    /// Messages that mention the user arrived (the same ones that turn the tab dot red): the Mentions panel.
    Mentions(Vec<RowRef>),
    /// Mentions reported earlier were deleted since (by `mentions::mention_id`).
    MentionsGone(Vec<String>),
    /// A hover tool in the Mentions panel acted on this pane (reply, open the thread): show its tab.
    Reveal,
}

impl EventEmitter<PaneEvent> for ChatPane {}

pub(super) enum Inbox {
    Reset(String),
    Frame(String, Frame),
    SubError(String, String),
}

#[derive(Default)]
struct Routes {
    /// sub id -> source key
    subs: HashMap<SubId, String>,
    /// frames that arrived before `subscribe` resolved with their id
    pending: HashMap<SubId, Vec<Frame>>,
    /// subscribes in flight: while > 0 unknown sub ids are buffered (they may be ours)
    inflight: usize,
}

pub(super) struct ShownRow {
    pub row: RowRef,
    pub built: RefCell<Option<(u64, Option<Rc<BuiltRow>>)>>,
}

pub(super) struct PinPick {
    pub sk: String,
    pub id: String,
    pub pos: Point<Pixels>,
}

pub(super) struct PopReq {
    pub sk: String,
    pub msg: ChatMsg,
    pub kind: super::mod_popover::PopKind,
    pub pos: Point<Pixels>,
    pub above: bool,
}

/// `UnpinAfter`'s state: "Customized time" (an amount in min / sec) or "Manually unpinned or end of stream".
pub(super) struct UnpinAfter {
    pub time_mode: bool,
    pub unit_min: bool,
    pub amount: Entity<TextInput>,
    pub busy: bool,
    pub _sub: Subscription,
}

/// Twitch takes 30 seconds to 30 minutes, or "until unpinned / stream ends".
pub(super) const PIN_MIN_S: i64 = 30;
pub(super) const PIN_MAX_S: i64 = 1800;

/// The seconds a "Customized time" amount means, when it is a valid one.
pub(super) fn unpin_secs(amount: &str, unit_min: bool) -> Option<i64> {
    let n: f64 = amount.trim().parse().ok()?;
    if !n.is_finite() {
        return None;
    }
    let secs = (n * if unit_min { 60.0 } else { 1.0 }).round() as i64;
    (PIN_MIN_S..=PIN_MAX_S).contains(&secs).then_some(secs)
}

pub(super) struct Toast {
    pub id: u64,
    pub text: String,
    pub warn: bool,
}

pub(super) struct Confirm {
    pub text: String,
    pub run: Option<RowAction>,
}

/// The chatter a Twitch suspicious-user frame is about (`suspicious` or `suspicioususer`), if it names one.
fn suspicious_user_of(f: &Value) -> Option<String> {
    let id = match f.get("type").and_then(|t| t.as_str())? {
        "suspicious" => f.get("user").and_then(|u| u.get("id")),
        "suspicioususer" => f.get("userId"),
        _ => None,
    }?;
    id.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// Who wrote a Twitch line (a message, a notice with a message, a held AutoMod message).
fn row_user_id(r: &Row) -> Option<&str> {
    match &r.body {
        RowBody::Msg(m) => Some(m.user.id.as_str()),
        RowBody::Notice(n) => Some(n.user.id.as_str()),
        RowBody::Automod(h) => Some(h.user.id.as_str()),
        _ => None,
    }
}

/// An open reply thread: which chat, its first message's id, the list's scroll, how many lines it showed last
/// frame (a new one scrolls it down) and the focus that Esc closes it from.
pub(super) struct ThreadOpen {
    pub sk: String,
    pub root: String,
    pub scroll: gpui::ScrollHandle,
    pub count: usize,
    pub focus: gpui::FocusHandle,
    /// the "Reply in thread" box (made on the first frame, which has the window) and its event subscription
    pub input: Option<(Entity<TextInput>, Subscription)>,
    pub sending: bool,
    /// the reply box's emote / Tab autocomplete and "@" list, like the message box's
    pub auto: Option<super::input::autocomplete::AutoState>,
    pub mentions: super::input::autocomplete::Mentions,
    pub auto_scroll: gpui::ScrollHandle,
    pub mention_scroll: gpui::ScrollHandle,
    /// the selection the lists last scrolled to (start of the word, row): only a move scrolls
    pub auto_at: Option<(usize, usize)>,
    pub mention_at: Option<(usize, usize)>,
    /// text the box was given by a pick, not typed: its Changed event opens no list
    pub skip_changed: u32,
    /// the inventory the box's emote images were built from
    pub images_inv: Option<*const super::input::inventory::Inventory>,
}

pub struct ChatPane {
    pub sources: Vec<SourceRef>,
    pub mod_mode: bool,
    /// This split has keyboard focus.
    pub focused: bool,
    /// Connection state per source (`source_key`), from the uniform `conn` frame.
    pub status: HashMap<String, ConnFrame>,

    pub(super) bridge: Arc<dyn AguBridge>,
    pub(super) svc: Services,
    pub(super) settings: Settings,
    pub(super) accounts: AccountsState,
    pub(super) store: ChatStore,

    // derived row list
    pub(super) shown: Vec<ShownRow>,
    pub(super) list_state: ChatListState,
    /// scrollbar thumb drag in progress: (pointer y at press, thumb top at press), window px
    pub(super) sb_drag: Option<(f32, f32)>,
    /// bumped whenever what a built row depends on changes
    pub(super) gen: u64,
    /// The display scale the built rows picked their emote files for (`rows::emote_scale_factor`).
    pub(super) emote_scale: f32,
    pub(super) highlighter: Highlighter,
    pub(super) ignores: CompiledIgnores,
    ignore_cache: IgnoreCache,
    rule_cache: HashMap<usize, Option<HighlightRule>>,
    pub(super) mod_list: Vec<agu_chat_core::protocol::ModButton>,

    // data per source
    pub(super) twitch: HashMap<String, TwitchData>,
    pub(super) envs: HashMap<String, Rc<TwitchEnv>>,
    pub(super) co_caps: HashMap<String, CoCaps>,
    /// per Kick source: the 7TV emotes of the broadcaster's Kick-linked 7TV account (and 7TV global)
    kick_seven: HashMap<String, Arc<ProviderSets>>,
    /// per Kick source: `kick_seven` with the settings applied, what its rows render against
    pub(super) kick_emotes: HashMap<String, Arc<EmoteSets>>,
    requested: HashSet<String>,
    /// "Reload emotes" in progress: (loads still out, whether one came back without everything)
    reload: Option<(usize, bool)>,
    /// the 7TV EventAPI subscriptions this pane holds, per Twitch or Kick source
    seven_subs: HashMap<String, SevenTvIds>,
    seven_seq: u64,

    // scrolling / pausing
    pub(super) scroll_held: bool,
    pub(super) hover_held: bool,
    pub(super) alt_held: bool,
    /// a text selection in the list holds it still (`select.rs`)
    pub(super) sel_held: bool,
    pub(super) pause_mark: u64,
    pending_rows: Option<Vec<RowRef>>,
    pub search_open: bool,
    pub(super) search: Option<Entity<TextInput>>,
    pub(super) query: String,
    _search_sub: Option<Subscription>,
    pub(super) needs_refresh: bool,

    // ui state
    pub(super) toasts: Vec<Toast>,
    toast_seq: u64,
    pub(super) tip: Option<(Rc<Tip>, Point<Pixels>)>,
    /// (tooltip showing, an image was hit by the current mouse move), shared by the pane's flows: `InlineFlow::on_tip`
    pub(super) tip_state: Rc<Cell<(bool, bool)>>,
    pub(super) hover_row: Option<String>,
    pub(super) hover_bar: Option<String>,
    pin_tick: Option<gpui::Task<()>>,
    /// per Twitch source: its hype train and when to ask again (`hype_train.rs`)
    pub(super) hype: super::hype_train::HypeMap,
    /// the hype train bars shown expanded, by source key
    pub(super) hype_open: HashSet<String>,
    pub(super) hype_tick: Option<gpui::Task<()>>,
    pub(super) reply_target: Option<ReplyTarget>,
    pub(super) send_box: Option<Entity<super::input::send_box::SendBox>>,
    /// the Ban / Warn popover (`modPop`) and the request to open one (built in render, which has the window)
    pub(super) mod_pop: Option<Entity<super::mod_popover::ModPopoverView>>,
    pub(super) pop_req: Option<PopReq>,
    /// open user cards (pop-out windows), oldest first
    pub(super) cards: Vec<(String, gpui::AnyWindowHandle)>,
    /// the last clicked name / button: its rectangle and its window's bounds, both in screen-space terms
    pub(super) last_act: Option<(gpui::Bounds<Pixels>, gpui::Bounds<Pixels>)>,
    /// the acting account's 7TV personal emote set (picker / autocomplete)
    pub(super) personal: Vec<Arc<super::twitch::emotes::ProviderEmote>>,
    /// the Twitch user id `personal` belongs to (it is kept while the next account's loads)
    personal_owner: String,
    pub(super) inv_cache: RefCell<HashMap<String, (u64, Arc<super::input::inventory::Inventory>)>>,
    pub(super) hidden_pins: Vec<String>,
    pub(super) pin_open: HashSet<String>,
    /// (card key, showing the "Unpin After" sub-panel)
    pub(super) pin_menu: Option<(String, bool)>,
    pub(super) unpin_after: Option<UnpinAfter>,
    /// the "Pin for" menu a message's pin button opened
    pub(super) pin_pick: Option<PinPick>,
    pub(super) confirm: Option<Confirm>,
    /// the reply thread open over the message list (DankChat's reply sheet)
    pub(super) thread: Option<ThreadOpen>,
    /// built rows of the open thread: row key -> (generation, the row they were built from, built)
    pub(super) thread_built: RefCell<HashMap<String, (u64, RowRef, Rc<BuiltRow>)>>,
    /// built rows drawn in the Mentions panel: row key -> (generation, the row they were built from, built)
    pub(super) mention_built: RefCell<HashMap<String, (u64, RowRef, Rc<BuiltRow>)>>,
    /// keys of the rows this pane reported as mentions (their later deletion takes them off the panel)
    mention_keys: HashSet<String>,
    /// a whisper opened this tab: its conversation came in as a snapshot, whose newest line is the mention
    pub whisper_pending: bool,
    /// the row a jump from the Mentions panel landed on, flashed for a moment
    pub(super) flash: Option<String>,
    /// text selection in the message list and in the reply thread
    pub(super) sel_main: Rc<SelScope>,
    pub(super) sel_thread: Rc<SelScope>,
    /// while a row is being drawn: the scope its flows are selectable in, the row's index, the next flow's number
    pub(super) sel_ctx: RefCell<Option<(Rc<SelScope>, u32, u32)>>,
    /// the right-click menu over a selection: where, and the text it copies
    pub(super) copy_menu: Option<(Point<Pixels>, String)>,
    pub(super) images_inbox: Rc<RefCell<Vec<String>>>,
    /// link previews that just arrived (their rows get measured again)
    pub(super) previews_inbox: Rc<RefCell<Vec<String>>>,
    pub(super) styled_users: Rc<RefCell<Vec<String>>>,
    seen_ord: u64,
    aid_frames: u32,
    pub(super) settle: u32,
    aids_done: bool,

    routes: Arc<Mutex<Routes>>,
    unsub_frames: Option<Unsubscribe>,
    unsub_accounts: Option<Unsubscribe>,
    _tasks: Vec<gpui::Task<()>>,
}

const MAX_TOASTS: usize = 4;

/// Start or stop hearing about a channel's 7TV emote set and its user (who may switch the active set).
fn seven_watch(svc: &Services, ids: &SevenTvIds, on: bool) {
    for (kind, id) in [(EMOTE_SET_UPDATE, &ids.set_id), (USER_UPDATE, &ids.user_id)] {
        if id.is_empty() {
            continue;
        }
        if on {
            svc.seven_events.subscribe(kind, id);
        } else {
            svc.seven_events.unsubscribe(kind, id);
        }
    }
}

fn hidden_pins_path(svc: &Services) -> std::path::PathBuf {
    svc.data_dir.join("hidden-pins.json")
}

impl ChatPane {
    pub fn new(sources: Vec<SourceRef>, mod_mode: bool, bridge: Arc<dyn AguBridge>, settings: Settings, cx: &mut Context<Self>) -> Self {
        let svc = cx.global::<Services>().clone();
        let mut refs: Vec<SourceRef> = Vec::new();
        for r in &sources {
            if !refs.iter().any(|x| source_key(x) == source_key(r)) {
                refs.push(r.clone());
            }
        }
        let mut store = ChatStore::default();
        store.sync(&refs);

        let (tx, mut rx) = unbounded::<Inbox>();
        let routes = Arc::new(Mutex::new(Routes::default()));

        // Frames reach the callback on backend threads, possibly BEFORE `subscribe` resolves with the id the
        // frames belong to (the snapshot replay happens inside subscribe): buffer unknown ids while one of our
        // subscribes is in flight, flush them when the id is known, drop them once none is in flight.
        let unsub_frames = {
            let routes = routes.clone();
            let tx: UnboundedSender<Inbox> = tx.clone();
            bridge.on_frame(Arc::new(move |sub, frame| {
                let mut r = routes.lock().unwrap();
                if let Some(k) = r.subs.get(sub) {
                    let _ = tx.unbounded_send(Inbox::Frame(k.clone(), frame.clone()));
                } else if r.inflight > 0 {
                    let list = r.pending.entry(sub.clone()).or_default();
                    if list.len() < 4096 {
                        list.push(frame.clone());
                    }
                }
            }))
        };

        let mut tasks: Vec<gpui::Task<()>> = Vec::new();
        {
            let (bridge, routes, tx) = (bridge.clone(), routes.clone(), tx.clone());
            let srcs = refs.clone();
            tasks.push(cx.spawn(async move |_this, _cx| {
                for s in srcs {
                    let sk = source_key(&s);
                    routes.lock().unwrap().inflight += 1;
                    let res = bridge.subscribe(s).await;
                    let mut r = routes.lock().unwrap();
                    r.inflight -= 1;
                    match res {
                        Ok(id) => {
                            r.subs.insert(id.clone(), sk.clone());
                            // a replayed snapshot replaces whatever an earlier subscription left behind
                            let _ = tx.unbounded_send(Inbox::Reset(sk.clone()));
                            for f in r.pending.remove(&id).unwrap_or_default() {
                                let _ = tx.unbounded_send(Inbox::Frame(sk.clone(), f));
                            }
                        }
                        Err(e) => {
                            let _ = tx.unbounded_send(Inbox::SubError(sk, e));
                        }
                    }
                    if r.inflight == 0 {
                        r.pending.clear();
                    }
                }
            }));
        }
        tasks.push(cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(Some(m)) = rx.try_next() {
                    batch.push(m);
                }
                if this.update(cx, |p, cx| p.on_inbox(batch, cx)).is_err() {
                    break;
                }
            }
        }));

        // Unsubscribe when the pane goes away (a split closed, or its channels changed).
        {
            let bridge = bridge.clone();
            let routes = routes.clone();
            cx.on_release(move |this, cx| {
                for (_, ids) in this.seven_subs.drain() {
                    seven_watch(&this.svc, &ids, false);
                }
                for (_, h) in this.cards.drain(..) {
                    let _ = h.update(cx, |_, w, _| w.remove_window());
                }
                if let Some(u) = this.unsub_frames.take() {
                    u();
                }
                if let Some(u) = this.unsub_accounts.take() {
                    u();
                }
                let ids: Vec<SubId> = routes.lock().unwrap().subs.keys().cloned().collect();
                for id in ids {
                    cx.background_executor().spawn(bridge.unsubscribe(id)).detach();
                }
            })
            .detach();
        }

        // accounts: the viewer (own messages, mod rights) follows the logged-in account
        let (atx, mut arx) = unbounded::<AccountsState>();
        let unsub_accounts = bridge.on_accounts(Arc::new(move |a| {
            let _ = atx.unbounded_send(a.clone());
        }));
        {
            let bridge = bridge.clone();
            tasks.push(cx.spawn(async move |this, cx| {
                let first = bridge.accounts_get().await;
                if this.update(cx, |p, cx| p.on_accounts(first, cx)).is_err() {
                    return;
                }
                while let Some(a) = arx.next().await {
                    if this.update(cx, |p, cx| p.on_accounts(a, cx)).is_err() {
                        break;
                    }
                }
            }));
        }

        let images_inbox = svc.images.0.add_inbox();
        let previews_inbox = svc.previews.add_inbox();
        svc.previews.set_enabled(settings.link_previews);
        let styled_users: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        {
            let inbox = styled_users.clone();
            svc.cosmetics.on_style(move |uid| inbox.borrow_mut().push(uid.to_string()));
        }
        svc.images.0.set_animate(settings.animated_emotes);
        svc.cosmetics.ensure_defs(&svc.net, cx);

        let hidden_pins: Vec<String> = std::fs::read(hidden_pins_path(&svc))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
            .unwrap_or_default();

        {
            // live 7TV emote changes; the listener goes when the pane does
            let weak = cx.entity().downgrade();
            svc.seven_events.listen(move |ev, cx| weak.update(cx, |p, cx| p.on_seven_event(ev, cx)).is_ok());
        }

        let mut pane = ChatPane {
            sources,
            mod_mode,
            focused: false,
            status: HashMap::new(),
            bridge,
            svc,
            highlighter: compile_highlights(&settings.highlight_rules),
            ignores: compile_ignores(&settings.ignored_users, &settings.ignored_phrases),
            ignore_cache: IgnoreCache::default(),
            rule_cache: HashMap::new(),
            mod_list: normalize_mod_buttons(&serde_json::to_value(&settings.mod_buttons).unwrap_or_default()),
            settings,
            accounts: AccountsState::new(),
            store,
            shown: Vec::new(),
            list_state: ChatListState::new(),
            sb_drag: None,
            gen: 1,
            emote_scale: super::rows::emote_scale_factor(),
            twitch: HashMap::new(),
            envs: HashMap::new(),
            co_caps: HashMap::new(),
            kick_seven: HashMap::new(),
            kick_emotes: HashMap::new(),
            requested: HashSet::new(),
            reload: None,
            seven_subs: HashMap::new(),
            seven_seq: 0,
            scroll_held: false,
            hover_held: false,
            alt_held: false,
            sel_held: false,
            pause_mark: 0,
            pending_rows: None,
            search_open: false,
            search: None,
            query: String::new(),
            _search_sub: None,
            needs_refresh: false,
            toasts: Vec::new(),
            toast_seq: 0,
            tip: None,
            tip_state: Rc::new(Cell::new((false, false))),
            hover_row: None,
            hover_bar: None,
            pin_tick: None,
            hype: HashMap::new(),
            hype_open: HashSet::new(),
            hype_tick: None,
            reply_target: None,
            send_box: None,
            mod_pop: None,
            pop_req: None,
            cards: Vec::new(),
            last_act: None,
            personal: Vec::new(),
            personal_owner: String::new(),
            inv_cache: RefCell::new(HashMap::new()),
            hidden_pins,
            pin_open: HashSet::new(),
            pin_menu: None,
            unpin_after: None,
            pin_pick: None,
            confirm: None,
            thread: None,
            thread_built: RefCell::new(HashMap::new()),
            mention_built: RefCell::new(HashMap::new()),
            mention_keys: HashSet::new(),
            whisper_pending: false,
            flash: None,
            sel_main: SelScope::new(),
            sel_thread: SelScope::new(),
            sel_ctx: RefCell::new(None),
            copy_menu: None,
            images_inbox,
            previews_inbox,
            styled_users,
            seen_ord: 0,
            aid_frames: 0,
            settle: 3,
            aids_done: false,
            routes,
            unsub_frames: Some(unsub_frames),
            unsub_accounts: Some(unsub_accounts),
            _tasks: tasks,
        };
        pane.start_loads(cx);
        pane.wire_selection(cx);
        pane
    }

    /// What the selection scopes need from the pane: the message list's rows (most are not on screen), the pause
    /// while something is selected, the scroll while a drag is held past the list's edge, the right-click menu.
    fn wire_selection(&self, cx: &mut Context<Self>) {
        let weak = cx.weak_entity();
        let w = weak.clone();
        self.sel_main.set_rows(Rc::new(move |row, cx| w.update(cx, |p, cx| p.built(row as usize, cx).map(|b| sel_flows(&b))).ok().flatten()));
        let w = weak.clone();
        self.sel_main.set_on_change(Rc::new(move |on, cx| {
            let _ = w.update(cx, |p, cx| p.set_sel_hold(on, cx));
        }));
        let ls = self.list_state.clone();
        self.sel_main.set_scroll(Rc::new(move |d| ls.scroll_by(d)));
        for scope in [&self.sel_main, &self.sel_thread] {
            let w = weak.clone();
            scope.set_on_context(Rc::new(move |pos, text, _win, cx| {
                let _ = w.update(cx, |p, cx| {
                    p.copy_menu = Some((pos, text));
                    cx.notify();
                });
            }));
        }
        let w = weak.clone();
        self.sel_thread.set_rows(Rc::new(move |row, cx| {
            w.update(cx, |p, cx| p.thread_built_rows(cx).get(row as usize).map(|(_, b)| sel_flows(b))).ok().flatten()
        }));
    }

    /// A selection in the message list pauses it like a hover pause does (new lines wait, the pill counts them);
    /// dropping the selection lets them in.
    pub(super) fn set_sel_hold(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.sel_held == on {
            return;
        }
        if on && !self.frozen() && !self.scroll_held {
            self.pause_mark = self.last_ord();
        }
        self.sel_held = on;
        self.update_store_hold();
        if !self.frozen() {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// Drop the message list's selection (the chat resumed).
    fn drop_selection(&mut self) {
        self.sel_main.clear_quiet();
        self.sel_held = false;
    }

    // ---- settings / accounts ---------------------------------------------------------------------------

    pub fn set_settings(&mut self, s: Settings, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.settings, s);
        let new = &self.settings;
        self.svc.images.0.set_animate(new.animated_emotes);
        self.svc.previews.set_enabled(new.link_previews);
        if old.highlight_rules != new.highlight_rules {
            self.highlighter = compile_highlights(&new.highlight_rules);
            self.rule_cache.clear();
        }
        if old.ignored_users != new.ignored_users || old.ignored_phrases != new.ignored_phrases {
            self.ignores = compile_ignores(&new.ignored_users, &new.ignored_phrases);
            self.ignore_cache = IgnoreCache::default();
        }
        if old.mod_buttons != new.mod_buttons {
            self.mod_list = normalize_mod_buttons(&serde_json::to_value(&new.mod_buttons).unwrap_or_default());
        }
        if old.pause_chat != new.pause_chat && new.pause_chat != "hover" {
            self.hover_held = false;
        }
        if old.pause_chat != new.pause_chat && new.pause_chat != "alt" {
            self.alt_held = false;
        }
        self.rebuild_envs();
        self.gen += 1;
        self.needs_refresh = true;
        self.refresh(cx);
        self.invalidate_all();
        cx.notify();
    }

    pub fn set_mod_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.mod_mode != on {
            self.mod_mode = on;
            self.gen += 1;
            self.invalidate_all();
            cx.notify();
        }
    }

    fn on_accounts(&mut self, a: AccountsState, cx: &mut Context<Self>) {
        let changed = self.accounts.get(&Platform::Twitch).map(|t| (t.connected, t.user.clone())) != a.get(&Platform::Twitch).map(|t| (t.connected, t.user.clone()));
        let co_changed = [Platform::Rumble, Platform::Kick].iter().any(|p| self.accounts.get(p).map(|t| t.connected) != a.get(p).map(|t| t.connected));
        self.accounts = a;
        self.rule_cache.clear();
        if changed || co_changed {
            // mod rights depend on who is logged in
            self.requested.retain(|k| !k.starts_with("caps:") && !k.starts_with("cocaps:"));
            self.start_loads(cx);
        }
        self.rebuild_envs();
        self.gen += 1;
        self.invalidate_all();
        // a YouTube login shows or hides the header's mod mode toggle
        cx.emit(PaneEvent::Status);
        cx.notify();
    }

    /// The logged-in account's login on Kick / Rumble ("" when logged out).
    pub(super) fn co_login(&self, platform: Platform) -> String {
        self.accounts.get(&platform).and_then(|a| a.user.as_ref()).map(|u| if u.login.is_empty() { u.name.clone() } else { u.login.clone() }).unwrap_or_default()
    }

    /// The logged-in Kick account's username and slug, lowercased (empty when logged out): what a Kick mention names.
    pub(super) fn kick_me(&self) -> Vec<String> {
        super::mentions::kick_me(&self.accounts)
    }

    pub(super) fn me_login_lower(&self) -> String {
        super::mentions::me_login_lower(&self.accounts)
    }

    fn me_id(&self) -> String {
        self.accounts.get(&Platform::Twitch).and_then(|a| a.user.as_ref()).map(|u| u.id.clone()).unwrap_or_default()
    }

    /// Open the reply thread a reply belongs to, over the message list.
    pub(super) fn open_thread(&mut self, sk: &str, parent: &str, thread: &str, cx: &mut Context<Self>) {
        let root = super::thread::thread_root(self.store.rows(), sk, parent, thread);
        if root.is_empty() {
            return;
        }
        self.thread_built.borrow_mut().clear();
        self.thread = Some(ThreadOpen { sk: sk.to_string(), root, scroll: gpui::ScrollHandle::new(), count: 0, focus: cx.focus_handle(), input: None,
            sending: false,
            auto: None,
            mentions: Default::default(),
            auto_scroll: gpui::ScrollHandle::new(),
            mention_scroll: gpui::ScrollHandle::new(),
            auto_at: None,
            mention_at: None,
            skip_changed: 0,
            images_inv: None,
        });
        cx.notify();
    }

    /// The open thread can take a reply from here: a Twitch or Kick chat this account can post in.
    pub(super) fn thread_can_reply(&self) -> bool {
        let Some(t) = &self.thread else { return false };
        let Some(st) = self.store.source(&t.sk) else { return false };
        matches!(st.source.platform, Platform::Twitch | Platform::Kick) && st.conn.as_ref().and_then(|c| c.can_send).unwrap_or(false)
    }

    /// The thread's reply box, made once per open thread: Enter sends, Esc closes the thread.
    pub(super) fn thread_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<TextInput>> {
        let t = self.thread.as_mut()?;
        if let Some((i, _)) = &t.input {
            return Some(i.clone());
        }
        let input = cx.new(|cx| {
            let mut i = TextInput::new(window, cx, "Reply in thread", InputStyle::Area);
            i.set_max_chars(Some(500));
            i
        });
        let sub = cx.subscribe(&input, |this, _, ev: &TextInputEvent, cx| match ev {
            TextInputEvent::Changed => this.thread_changed(cx),
            TextInputEvent::Enter => this.send_thread_reply(cx),
            TextInputEvent::Escape => this.close_thread(cx),
            _ => {}
        });
        t.input = Some((input.clone(), sub));
        Some(input)
    }

    /// The open thread's chat's emote inventory: what the message box completes from for that chat.
    fn thread_inventory(&self) -> Arc<super::input::inventory::Inventory> {
        let st = self.thread.as_ref().and_then(|t| self.store.source(&t.sk));
        st.map(|st| self.source_inventory(st)).unwrap_or_default()
    }

    /// Hand the reply box the emote images of its chat (rebuilt only when that inventory changed).
    pub(super) fn thread_sync_images(&mut self, cx: &mut Context<Self>) {
        let inv = self.thread_inventory();
        let Some(t) = self.thread.as_mut() else { return };
        let Some((input, _)) = &t.input else { return };
        let ptr = Arc::as_ptr(&inv);
        if t.images_inv == Some(ptr) {
            return;
        }
        t.images_inv = Some(ptr);
        let lookup = super::input::send_box::emote_image_lookup(&inv.inventory);
        input.update(cx, |i, cx| i.set_inline_images(Some(lookup), cx));
    }

    /// The reply box's text changed: open, follow or close the emote list and the "@" list (the message box's rules;
    /// "/" starts nothing here, commands go in the box below).
    fn thread_changed(&mut self, cx: &mut Context<Self>) {
        use super::input::autocomplete::{auto_for_change, ChangeCtx};
        let Some(t) = self.thread.as_mut() else { return };
        if t.skip_changed > 0 {
            t.skip_changed -= 1;
            cx.notify();
            return;
        }
        let Some((input, _)) = &t.input else { return };
        let (val, pos) = {
            let i = input.read(cx);
            (i.text(), i.caret())
        };
        let sks = vec![t.sk.clone()];
        let mut mentions = std::mem::take(&mut t.mentions);
        let prev = t.auto.take();
        let inv = self.thread_inventory();
        let cache: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
        let names = || cache.get_or_init(|| self.chatters(&sks)).clone();
        mentions.refresh(&val, pos, &names);
        let ctx = ChangeCtx { inventory: &inv.inventory, slash_ok: false, twitch_caps: None, names: &names };
        let auto = auto_for_change(&val, pos, prev.as_ref(), &ctx);
        if let Some(t) = self.thread.as_mut() {
            t.mentions = mentions;
            t.auto = auto;
        }
        cx.notify();
    }

    /// Write the reply box's text from a pick (no list re-opens from it).
    fn thread_put(&mut self, text: &str, caret: usize, cx: &mut Context<Self>) {
        let Some(t) = self.thread.as_mut() else { return };
        let Some((input, _)) = &t.input else { return };
        t.skip_changed += 1;
        let input = input.clone();
        input.update(cx, |i, cx| i.replace_all(text, caret, cx));
    }

    fn thread_text(&self, cx: &gpui::App) -> (String, usize) {
        self.thread.as_ref().and_then(|t| t.input.as_ref()).map(|(i, _)| (i.read(cx).text(), i.read(cx).caret())).unwrap_or_default()
    }

    fn thread_focus_input(&self, window: &mut Window, cx: &gpui::App) {
        if let Some((i, _)) = self.thread.as_ref().and_then(|t| t.input.as_ref()) {
            i.read(cx).focus(window);
        }
    }

    /// Put the emote list's pick (or its selected row) into the reply box.
    pub(super) fn thread_pick_auto(&mut self, choice: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(auto) = self.thread.as_mut().and_then(|t| t.auto.take()) else { return };
        let (text, _) = self.thread_text(cx);
        let pick = choice.and_then(|i| auto.matches.get(i).cloned());
        if let Some((t, caret)) = super::input::autocomplete::commit_auto(&text, &auto, pick.as_ref()) {
            self.thread_put(&t, caret, cx);
        }
        self.thread_focus_input(window, cx);
        cx.notify();
    }

    /// Put the "@" list's pick (or its selected name) into the reply box.
    pub(super) fn thread_pick_mention(&mut self, choice: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let (text, _) = self.thread_text(cx);
        let applied = self.thread.as_mut().and_then(|t| t.mentions.apply(&text, choice.as_deref()));
        if let Some((t, caret)) = applied {
            self.thread_put(&t, caret, cx);
        }
        self.thread_focus_input(window, cx);
        cx.notify();
    }

    /// The reply box's keys the lists own (before the text field sees them): Tab completes an emote or name,
    /// arrows move through an open list, Enter / Tab take its row, Esc closes it.
    pub(super) fn thread_key(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        use super::input::autocomplete::{auto_for_tab, preview_auto, AutoMode, ChangeCtx};
        let key = ev.keystroke.key.as_str();
        if crate::text_input::is_shortcut_mod(&ev.keystroke.modifiers) {
            return;
        }
        let Some(t) = self.thread.as_ref() else { return };
        if t.mentions.state.is_some() {
            match key {
                "tab" | "enter" => self.thread_pick_mention(None, window, cx),
                "up" | "down" => {
                    if let Some(t) = self.thread.as_mut() {
                        t.mentions.mv(if key == "up" { -1 } else { 1 });
                    }
                }
                "escape" => {
                    if let Some(t) = self.thread.as_mut() {
                        t.mentions.close();
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(auto) = t.auto.clone() {
            match key {
                "tab" | "enter" => self.thread_pick_auto(None, window, cx),
                "up" | "down" => {
                    let n = auto.matches.len();
                    let idx = (auto.idx + n + if key == "up" { n - 1 } else { 1 }) % n;
                    if auto.mode == AutoMode::Cmd {
                        if let Some(a) = self.thread.as_mut().and_then(|t| t.auto.as_mut()) {
                            a.idx = idx;
                        }
                    } else {
                        let (text, _) = self.thread_text(cx);
                        if let Some((txt, pos, next)) = preview_auto(&text, &auto, idx) {
                            self.thread_put(&txt, pos, cx);
                            if let Some(t) = self.thread.as_mut() {
                                t.auto = Some(next);
                            }
                        }
                    }
                }
                "escape" => {
                    if let Some(t) = self.thread.as_mut() {
                        t.auto = None;
                    }
                }
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if key == "tab" {
            let (text, pos) = self.thread_text(cx);
            let sks = vec![t.sk.clone()];
            let inv = self.thread_inventory();
            let cache: std::cell::OnceCell<Vec<String>> = std::cell::OnceCell::new();
            let names = || cache.get_or_init(|| self.chatters(&sks)).clone();
            let ctx = ChangeCtx { inventory: &inv.inventory, slash_ok: false, twitch_caps: None, names: &names };
            let auto = auto_for_tab(&text, pos, &ctx);
            if let (Some(a), Some(t)) = (auto, self.thread.as_mut()) {
                t.auto = Some(a);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// Send what is in the thread's box as a reply to the thread's first message (Twitch keeps every reply to it
    /// in the thread).
    fn send_thread_reply(&mut self, cx: &mut Context<Self>) {
        if !self.thread_can_reply() {
            return;
        }
        let Some(t) = &self.thread else { return };
        let Some((input, _)) = &t.input else { return };
        if t.sending {
            return;
        }
        let text = input.read(cx).text().trim().to_string();
        if text.is_empty() {
            return;
        }
        if text.starts_with('/') {
            self.toast("Commands go in the message box below", true, cx);
            return;
        }
        let (sk, root, input) = (t.sk.clone(), t.root.clone(), input.clone());
        let Some(src) = self.ref_of(&sk) else { return };
        let Some(path) = super::input::send_box::send_path(src.platform) else { return };
        let bridge = self.bridge.clone();
        if let Some(t) = self.thread.as_mut() {
            t.sending = true;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let err = super::input::send_box::post_text(&bridge, &src, path, &text, Some(&root)).await;
            let _ = this.update(cx, |p, cx| {
                if let Some(t) = p.thread.as_mut().filter(|t| t.root == root) {
                    t.sending = false;
                }
                match err {
                    Some(e) => p.toast(e, true, cx),
                    None => input.update(cx, |i, cx| i.set_text("", cx)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn thread_open(&self) -> bool {
        self.thread.is_some()
    }

    pub fn close_thread(&mut self, cx: &mut Context<Self>) {
        if self.thread.take().is_some() {
            self.sel_thread.clear_quiet();
            self.thread_built.borrow_mut().clear();
            cx.notify();
        }
    }

    /// The open thread's lines, built like the list's rows (cached by row and generation).
    pub(super) fn thread_built_rows(&mut self, cx: &mut gpui::App) -> Vec<(String, Rc<BuiltRow>)> {
        let Some(t) = &self.thread else { return Vec::new() };
        let rows = super::thread::thread_rows(self.store.rows(), &t.sk, &t.root);
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let cached = self.thread_built.borrow().get(&r.key).filter(|(g, from, _)| *g == self.gen && Arc::ptr_eq(from, &r)).map(|(_, _, b)| b.clone());
            let b = match cached {
                Some(b) => b,
                None => {
                    let Some(b) = self.build_row(&r, cx).map(Rc::new) else { continue };
                    self.thread_built.borrow_mut().insert(r.key.clone(), (self.gen, r.clone(), b.clone()));
                    b
                }
            };
            out.push((r.key.clone(), b));
        }
        out
    }

    // ---- the Mentions panel (its rows are drawn by the pane that has their chat) --------------------------

    /// This pane shows the chat `sk`.
    pub fn has_source(&self, sk: &str) -> bool {
        self.store.source(sk).is_some()
    }

    /// The Mentions panel's rows from this pane's chats, drawn like the list draws them (badges, name that opens
    /// the card, emotes, hover tools), from the row as the store has it now when it is still there. None for a
    /// row that can't be drawn here or is hidden now (deleted, or its author ignored since).
    /// `order[i]` is row i's place in the panel, `scope` the panel's text selection.
    pub fn mention_row_els(&mut self, rows: &[RowRef], order: &[usize], scope: &Rc<SelScope>, cx: &mut Context<Self>) -> Vec<Option<AnyElement>> {
        let wanted: HashSet<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        let current: HashMap<String, RowRef> = self.store.rows().iter().filter(|r| wanted.contains(r.key.as_str())).map(|r| (r.key.clone(), r.clone())).collect();
        let mut keep: HashSet<String> = HashSet::new();
        let mut out = Vec::with_capacity(rows.len());
        for (n, snap) in rows.iter().enumerate() {
            let r = current.get(&snap.key).cloned().unwrap_or_else(|| snap.clone());
            if super::mentions::row_deleted(&r) || apply_ignores(&r, &self.ignores).is_none() || self.store.source(&r.sk).is_none() {
                out.push(None);
                continue;
            }
            keep.insert(r.key.clone());
            let cached = self.mention_built.borrow().get(&r.key).filter(|(g, from, _)| *g == self.gen && Arc::ptr_eq(from, &r)).map(|(_, _, b)| b.clone());
            let b = match cached {
                Some(b) => b,
                None => match self.build_row_as(&r, false, cx).map(Rc::new) {
                    Some(b) => {
                        self.mention_built.borrow_mut().insert(r.key.clone(), (self.gen, r.clone(), b.clone()));
                        b
                    }
                    None => {
                        out.push(None);
                        continue;
                    }
                },
            };
            let place = order.get(n).copied().unwrap_or(n);
            out.push(Some(self.with_sel_row(scope, place, |this| this.panel_row_el(&r.key, &b, cx))));
        }
        self.mention_built.borrow_mut().retain(|k, _| keep.contains(k));
        out
    }

    /// A short note at the bottom of this split (the Mentions panel's jump that found no chat to go to).
    pub fn notify_toast(&mut self, text: &str, cx: &mut Context<Self>) {
        self.toast(text.to_string(), false, cx);
    }

    /// The panel closed: its built rows can go.
    pub fn forget_mention_rows(&mut self) {
        self.mention_built.borrow_mut().clear();
    }

    /// The hover tooltip (emote / badge) of a row in the Mentions panel, when this pane's own view isn't drawn.
    pub fn tip_overlay(&self, window: &Window) -> Option<AnyElement> {
        self.tip_el(window)
    }

    /// Jump from the Mentions panel: scroll the list to the row and flash it, or say why it can't be shown.
    pub fn jump_to(&mut self, key: &str, cx: &mut Context<Self>) {
        // a hover / Alt pause (or a selection) would keep the list as it was
        self.hover_held = false;
        self.alt_held = false;
        self.drop_selection();
        self.refresh(cx);
        let Some(ix) = self.shown.iter().position(|s| s.row.key == key) else {
            let msg = if self.store.rows().iter().any(|r| r.key == key) { "That message is hidden in this chat right now" } else { "That message is no longer in this chat" };
            self.toast(msg, false, cx);
            return;
        };
        let (_, vh) = self.list_state.viewport();
        self.list_state.scroll_to_item(ix, if vh > 0.0 { (vh / 3.0).round() } else { 40.0 });
        self.scroll_held = true;
        self.pause_mark = self.last_ord();
        self.update_store_hold();
        self.settle = 3;
        self.flash = Some(key.to_string());
        let k = key.to_string();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(1400)).await;
            let _ = this.update(cx, |p, cx| {
                if p.flash.as_deref() == Some(k.as_str()) {
                    p.flash = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// A Twitch user's newest chat line in this source (their badges and colour, for lines that carry neither).
    fn last_line_of(&self, sk: &str, user_id: &str) -> Option<ChatMsg> {
        if user_id.is_empty() {
            return None;
        }
        self.store.rows().iter().rev().find_map(|r| match &r.body {
            RowBody::Msg(m) if r.sk == sk && m.user.id == user_id => Some(m.clone()),
            _ => None,
        })
    }

    // ---- data loading (useTwitchData) -------------------------------------------------------------------

    fn start_loads(&mut self, cx: &mut Context<Self>) {
        let sks: Vec<(String, SourceRef, Option<String>)> = self.store.sources.iter().map(|s| (s.sk.clone(), s.source.clone(), s.channel_id.clone())).collect();
        let acct = format!("{}:{}", self.accounts.get(&Platform::Twitch).map(|a| a.connected).unwrap_or(false), self.me_id());
        self.load_personal(false, cx);
        for (sk, source, channel_id) in sks {
            match source.platform {
                Platform::Twitch => {
                    // A whisper conversation has no channel: no badges, cheers, mod rights or channel emotes.
                    let whisper = is_whisper(&source);
                    if !whisper && self.requested.insert(format!("badges:{sk}")) {
                        let (bridge, s2, sk2) = (self.bridge.clone(), source.clone(), sk.clone());
                        cx.spawn(async move |this, cx| {
                            if let Some((map, cid)) = load_badges(&bridge, &s2, &sk2).await {
                                let _ = this.update(cx, |p, cx| {
                                    let new_cid = cid.is_some_and(|cid| p.store.set_channel_id(&sk2, &cid));
                                    p.twitch.entry(sk2.clone()).or_default().badge_map = Arc::new(map);
                                    p.data_changed(cx);
                                    if new_cid {
                                        // the emote sets are keyed by it: start them now, not with the next frame
                                        p.start_loads(cx);
                                    }
                                });
                            }
                        })
                        .detach();
                    }
                    if !whisper && self.requested.insert(format!("cheermotes:{sk}:{acct}")) {
                        let (bridge, s2, sk2) = (self.bridge.clone(), source.clone(), sk.clone());
                        cx.spawn(async move |this, cx| {
                            if let Some(cm) = super::twitch::cheermotes::load_cheermotes(&bridge, &s2, &sk2).await {
                                let _ = this.update(cx, |p, cx| {
                                    p.twitch.entry(sk2).or_default().cheermotes = Some(Arc::new(cm));
                                    p.data_changed(cx);
                                });
                            }
                        })
                        .detach();
                    }
                    if !whisper && self.requested.insert(format!("caps:{sk}:{acct}")) {
                        let (bridge, s2, sk2) = (self.bridge.clone(), source.clone(), sk.clone());
                        cx.spawn(async move |this, cx| {
                            let caps = fetch_mod_caps(&bridge, &s2).await;
                            let _ = this.update(cx, |p, cx| {
                                p.twitch.entry(sk2).or_default().caps = caps;
                                p.data_changed(cx);
                                // the header's mod mode toggle shows only where this account can moderate
                                cx.emit(PaneEvent::Status);
                            });
                        })
                        .detach();
                    }
                    if !whisper && self.requested.insert(format!("pickeremotes:{sk}:{}", channel_id.clone().unwrap_or_default())) {
                        self.load_picker_emotes(sk.clone(), source.clone(), channel_id.clone().unwrap_or_else(|| sk.clone()), false, cx);
                    }
                    if let Some(cid) = channel_id {
                        if self.requested.insert(format!("emotes:{sk}:{cid}")) {
                            self.load_emote_sets(sk.clone(), cid, 0, false, cx);
                        }
                    }
                }
                Platform::Rumble | Platform::Kick => {
                    if source.platform == Platform::Kick {
                        self.load_kick_seven(&sk, false, cx);
                    }
                    let key = format!("cocaps:{sk}:{}{}", self.accounts.get(&Platform::Rumble).map(|a| a.connected as u8).unwrap_or(0), self.accounts.get(&Platform::Kick).map(|a| a.connected as u8).unwrap_or(0));
                    if self.requested.insert(key) {
                        let (bridge, s2, sk2) = (self.bridge.clone(), source.clone(), sk.clone());
                        cx.spawn(async move |this, cx| {
                            let caps = fetch_co_caps(&bridge, &s2).await;
                            let _ = this.update(cx, |p, cx| {
                                p.co_caps.insert(sk2, caps);
                                p.gen += 1;
                                p.invalidate_all();
                                cx.emit(PaneEvent::Status);
                                cx.notify();
                            });
                        })
                        .detach();
                    }
                }
                _ => {}
            }
        }
    }

    /// The Kick broadcaster's user id (what 7TV links a Kick account by), from the source's status frame.
    fn kick_user_id(&self, sk: &str) -> Option<String> {
        let v = self.store.source(sk)?.status.as_ref()?.get("userId")?;
        let id = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return None,
        };
        (!id.is_empty()).then_some(id)
    }

    /// The account a source's 7TV channel set is linked from: a Twitch channel id, or a Kick broadcaster's user id.
    fn seven_owner(&self, sk: &str) -> Option<(SevenTvPlatform, String)> {
        let st = self.store.source(sk)?;
        match st.source.platform {
            Platform::Twitch => st.channel_id.clone().map(|id| (SevenTvPlatform::Twitch, id)),
            Platform::Kick => self.kick_user_id(sk).map(|id| (SevenTvPlatform::Kick, id)),
            _ => None,
        }
    }

    /// A source's loaded 7TV / BTTV / FFZ sets, whichever platform it is on.
    fn seven_sets_mut(&mut self, sk: &str) -> Option<&mut Arc<ProviderSets>> {
        if self.kick_seven.contains_key(sk) {
            return self.kick_seven.get_mut(sk);
        }
        self.twitch.get_mut(sk).and_then(|d| d.provider_sets.as_mut())
    }

    /// A Kick source's 7TV emotes, once its broadcaster's user id is known. `reload`: again past the cache, part of
    /// "Reload emotes".
    fn load_kick_seven(&mut self, sk: &str, reload: bool, cx: &mut Context<Self>) {
        let Some(uid) = self.kick_user_id(sk) else { return };
        if !self.requested.insert(format!("kick7tv:{sk}:{uid}")) && !reload {
            return;
        }
        let sk2 = sk.to_string();
        let task = self.svc.net.run(load_kick_provider_sets(self.svc.net.client.clone(), uid, reload));
        self.reload_begin(reload);
        cx.spawn(async move |this, cx| {
            let sets = task.await;
            let _ = this.update(cx, |p, cx| {
                let ok = sets.is_some();
                if let Some(sets) = sets {
                    p.kick_seven.insert(sk2, Arc::new(sets));
                    p.data_changed(cx);
                }
                p.reload_end(reload, ok, cx);
            });
        })
        .detach();
    }

    /// A Twitch channel's 7TV / BTTV / FFZ sets. The first try shows what is cached at once (however old) while the
    /// network refreshes it; a refresh that did not get everything (a provider down or slow at startup) is tried
    /// again with growing waits. Each arrival rebuilds the rows already in the list, so lines that came in before
    /// the sets get their emotes too. `reload` ("Reload emotes") skips the cache, the global sets' too.
    fn load_emote_sets(&mut self, sk: String, cid: String, attempt: u32, reload: bool, cx: &mut Context<Self>) {
        let client = self.svc.net.client.clone();
        let cached = (attempt == 0 && !reload).then(|| self.svc.net.run(load_cached_provider_sets(client.clone(), cid.clone())));
        let task = self.svc.net.run(load_provider_sets(client, cid.clone(), reload));
        self.reload_begin(reload);
        cx.spawn(async move |this, cx| {
            let same_channel = |p: &ChatPane| p.store.source(&sk).and_then(|s| s.channel_id.as_deref()) == Some(cid.as_str());
            if let Some(cached) = cached {
                if let Some(Some(sets)) = cached.await {
                    let _ = this.update(cx, |p, cx| {
                        if same_channel(p) && p.twitch.get(&sk).map_or(true, |d| d.provider_sets.is_none()) {
                            p.twitch.entry(sk.clone()).or_default().provider_sets = Some(Arc::new(sets));
                            p.data_changed(cx);
                        }
                    });
                }
            }
            let got = task.await;
            let (applied, complete) = this
                .update(cx, |p, cx| {
                    let Some((sets, complete)) = got else {
                        p.reload_end(reload, false, cx);
                        return (false, false);
                    };
                    let applied = same_channel(p);
                    if applied {
                        p.twitch.entry(sk.clone()).or_default().provider_sets = Some(Arc::new(sets));
                        p.data_changed(cx);
                    }
                    p.reload_end(reload, complete || !applied, cx);
                    (applied, complete)
                })
                .unwrap_or((false, false));
            if complete || !applied {
                return;
            }
            let Some(wait) = set_retry_delay(attempt) else { return };
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |p, cx| {
                if same_channel(p) {
                    p.load_emote_sets(sk.clone(), cid.clone(), attempt + 1, false, cx);
                }
            });
        })
        .detach();
    }

    /// The acting account's 7TV personal set, loaded once per account (and again past the cache on `reload`).
    fn load_personal(&mut self, reload: bool, cx: &mut Context<Self>) {
        let id = self.me_id();
        if id.is_empty() || (!self.requested.insert(format!("personal:{id}")) && !reload) {
            return;
        }
        let owner = id.clone();
        let task = self.svc.net.run({
            let client = self.svc.net.client.clone();
            async move { super::twitch::emotes::fetch_seven_tv_personal(&client, &id, reload).await }
        });
        self.reload_begin(reload);
        cx.spawn(async move |this, cx| {
            let list = task.await;
            let _ = this.update(cx, |p, cx| {
                let ok = list.is_some();
                if let Some(list) = list {
                    p.personal = list;
                    p.personal_owner = owner;
                    p.data_changed(cx);
                }
                p.reload_end(reload, ok, cx);
            });
        })
        .detach();
    }

    /// Twitch's own emotes of a channel for the picker / autocomplete (`reload`: past the cache).
    fn load_picker_emotes(&mut self, sk: String, source: SourceRef, cid: String, reload: bool, cx: &mut Context<Self>) {
        let bridge = self.bridge.clone();
        self.reload_begin(reload);
        cx.spawn(async move |this, cx| {
            let r = super::twitch::emotes::fetch_twitch_picker_emotes(&bridge, &source, &cid, reload).await;
            let _ = this.update(cx, |p, cx| {
                p.twitch.entry(sk).or_default().twitch_emotes = r;
                p.data_changed(cx);
                p.reload_end(reload, true, cx);
            });
        })
        .detach();
    }

    /// "Reload emotes" (the split's ⋯ menu): every emote set this split's sources draw with, fetched again past the
    /// disk cache: Twitch's own (picker) emotes, the 7TV / BTTV / FFZ channel and global sets, the account's 7TV
    /// personal set, and a Kick channel's 7TV set. Images that failed to load are tried again at once. Rows rebuild
    /// as each set arrives; a toast says when all are back. Kick / Rumble / YouTube's own sets come with the chat
    /// connection and are not refetched here.
    pub fn reload_emotes(&mut self, cx: &mut Context<Self>) {
        if self.reload.is_some() {
            return; // already on its way
        }
        // one held for this call, so loads that finish at once can't end it early
        self.reload = Some((1, false));
        self.toast("Reloading emotes\u{2026}", false, cx);
        self.svc.images.0.retry_failed();
        self.load_personal(true, cx);
        let sources: Vec<(String, SourceRef, Option<String>)> = self.store.sources.iter().map(|s| (s.sk.clone(), s.source.clone(), s.channel_id.clone())).collect();
        for (sk, source, channel_id) in sources {
            match source.platform {
                Platform::Twitch if !is_whisper(&source) => {
                    self.requested.insert(format!("pickeremotes:{sk}:{}", channel_id.clone().unwrap_or_default()));
                    self.load_picker_emotes(sk.clone(), source, channel_id.clone().unwrap_or_else(|| sk.clone()), true, cx);
                    if let Some(cid) = channel_id {
                        self.requested.insert(format!("emotes:{sk}:{cid}"));
                        self.load_emote_sets(sk, cid, 0, true, cx);
                    }
                }
                Platform::Kick => self.load_kick_seven(&sk, true, cx),
                _ => {}
            }
        }
        // rows drawn again, so failed images are asked for again
        self.data_changed(cx);
        self.reload_end(true, true, cx);
    }

    /// A load that is part of "Reload emotes" started.
    fn reload_begin(&mut self, reload: bool) {
        if let (true, Some((n, _))) = (reload, self.reload.as_mut()) {
            *n += 1;
        }
    }

    /// A load that is part of "Reload emotes" ended (`ok`: it got everything); the last one says so.
    fn reload_end(&mut self, reload: bool, ok: bool, cx: &mut Context<Self>) {
        let Some((n, missed)) = self.reload.as_mut().filter(|_| reload) else { return };
        *n = n.saturating_sub(1);
        *missed |= !ok;
        if *n > 0 {
            return;
        }
        let missed = *missed;
        self.reload = None;
        if missed {
            self.toast("Some emote sets did not load; trying again", true, cx);
        } else {
            self.toast("Emotes reloaded", false, cx);
        }
    }

    /// Display names newest-first for the given source keys (read lazily: only when "@" or Tab is used).
    pub(super) fn chatters(&self, sks: &[String]) -> Vec<String> {
        let mut out = Vec::new();
        for r in self.store.rows().iter().rev() {
            if !sks.contains(&r.sk) {
                continue;
            }
            let n = match &r.body {
                RowBody::Msg(m) => &m.user.name,
                RowBody::Rumble(m) => &m.user.name,
                RowBody::Kick(m) => &m.user.name,
                RowBody::YouTube(m) => &m.user.name,
                RowBody::X(m) => &m.user.name,
                _ => continue,
            };
            if !n.is_empty() {
                out.push(n.clone());
            }
            if out.len() >= 1000 {
                break;
            }
        }
        out
    }

    /// (newest message id, pinned message id) of a source.
    pub(super) fn message_ids(&self, sk: &str) -> (Option<String>, Option<String>) {
        let last = self.store.rows().iter().rev().find_map(|r| match &r.body {
            RowBody::Msg(m) if r.sk == sk => Some(m.id.clone()),
            _ => None,
        });
        (last, self.store.source(sk).and_then(|s| s.pinned.as_ref().map(|p| p.id.clone())))
    }

    /// A source's emote inventory for the send boxes' picker and autocomplete (cached per data generation).
    pub(super) fn source_inventory(&self, st: &super::store::SourceState) -> Arc<super::input::inventory::Inventory> {
        use super::input::inventory::*;
        let rs = &self.settings;
        let mut cache = self.inv_cache.borrow_mut();
        match cache.get(&st.sk) {
            Some((g, i)) if *g == self.gen => i.clone(),
            _ => {
                let i = Arc::new(match st.source.platform {
                    Platform::Twitch => {
                        let d = self.twitch.get(&st.sk).cloned().unwrap_or_default();
                        let empty = ProviderScopedMap::default();
                        let sets = d.provider_sets.as_ref().map(|ps| ProviderSets {
                            seven: if rs.seven_tv { ps.seven.clone() } else { empty.clone() },
                            bttv: if rs.bttv { ps.bttv.clone() } else { empty.clone() },
                            ffz: if rs.ffz { ps.ffz.clone() } else { empty.clone() },
                            ..Default::default()
                        });
                        build_twitch_inventory(sets.as_ref(), &d.twitch_emotes, &self.personal)
                    }
                    Platform::Rumble => build_rumble_inventory(&st.rumble_emotes),
                    Platform::Kick => build_kick_inventory(&st.kick_emotes),
                    Platform::Youtube => build_youtube_inventory(&st.youtube_emotes),
                    _ => Inventory::default(),
                });
                cache.insert(st.sk.clone(), (self.gen, i.clone()));
                i
            }
        }
    }

    /// The send box view of each source: can it post, caps, and its emote inventory (cached per data generation).
    pub(super) fn send_sources(&self) -> Vec<super::input::send_box::SendSource> {
        let mut out = Vec::new();
        for st in &self.store.sources {
            let conn = st.conn.as_ref();
            let inv = self.source_inventory(st);
            out.push(super::input::send_box::SendSource {
                sk: st.sk.clone(),
                source: st.source.clone(),
                can_send: conn.and_then(|c| c.can_send).unwrap_or(false),
                read_only_reason: conn.and_then(|c| c.read_only_reason.clone()),
                live: conn.and_then(|c| c.live),
                logged_out: conn.and_then(|c| c.logged_out).unwrap_or(false),
                pending: conn.map(|c| c.state == "connecting").unwrap_or(true),
                caps: self.twitch.get(&st.sk).map(|d| d.caps.clone()).unwrap_or_default(),
                inventory: inv,
                gate: self.chat_gate(st),
            });
        }
        out
    }

    /// The source's chat modes as they apply to the logged-in account. Kick says who the account is in the channel;
    /// on Twitch it is pieced together from the mod rights and the badges on the account's own latest line there;
    /// YouTube says nothing about the viewer.
    fn chat_gate(&self, st: &super::store::SourceState) -> Option<agu_chat_core::chat_rules::ChatGate> {
        let conn = st.conn.as_ref()?;
        let modes = conn.chat_modes.as_ref()?;
        // who the account is only matters for the modes that keep people out
        let gated = modes.subs_only || modes.members_only || modes.followers_min.is_some() || modes.account_age_min.is_some();
        let viewer = match st.source.platform {
            Platform::Twitch if gated => {
                let caps = self.twitch.get(&st.sk).map(|d| d.caps.clone()).unwrap_or_default();
                let me_login = self.me_login_lower();
                let broadcaster = caps.is_broadcaster.unwrap_or(!me_login.is_empty() && st.source.channel.to_lowercase().trim_start_matches('#') == me_login);
                let own = self.last_line_of(&st.sk, &self.me_id());
                super::input::send_box::twitch_viewer(broadcaster, caps.is_mod == Some(true), own.as_ref().map(|m| m.badges.as_str()))
            }
            _ => conn.chat_viewer.clone().unwrap_or_default(),
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        agu_chat_core::chat_rules::chat_gate(modes, &viewer, now)
    }

    fn data_changed(&mut self, cx: &mut Context<Self>) {
        self.rebuild_envs();
        self.gen += 1;
        self.invalidate_all();
        cx.notify();
    }

    /// 7TV / BTTV / FFZ with the settings applied, merged by priority, per Twitch source.
    pub(super) fn rebuild_envs(&mut self) {
        let rs = &self.settings;
        let me_login = self.me_login_lower();
        let me_id = self.me_id();
        let defs = self.svc.cosmetics.defs();
        let empty = ProviderScopedMap::default();
        // the account's own personal set, for its own lines (the send box's inventory has it too)
        let my_personal: Option<Arc<super::twitch::emotes::EmoteMap>> = (rs.seven_tv && !me_id.is_empty() && self.personal_owner == me_id && !self.personal.is_empty())
            .then(|| Arc::new(self.personal.iter().map(|e| (e.name.clone(), e.clone())).collect()));
        let mut envs = HashMap::new();
        for st in self.store.sources.iter().filter(|s| s.source.platform == Platform::Twitch) {
            let d = self.twitch.get(&st.sk).cloned().unwrap_or_default();
            let emotes: Option<Arc<EmoteSets>> = d.provider_sets.as_ref().map(|ps| {
                Arc::new(merge_emote_sets(if rs.seven_tv { &ps.seven } else { &empty }, if rs.bttv { &ps.bttv } else { &empty }, if rs.ffz { &ps.ffz } else { &empty }))
            });
            let broadcaster = d.caps.is_broadcaster.unwrap_or(!me_login.is_empty() && st.source.channel.to_lowercase().trim_start_matches('#') == me_login);
            let lead = !me_id.is_empty()
                && self
                    .store
                    .rows()
                    .iter()
                    .rev()
                    .find_map(|r| match &r.body {
                        RowBody::Msg(m) if r.sk == st.sk && m.user.id == me_id => Some(badge_names(&m.badges).contains("lead_moderator")),
                        _ => None,
                    })
                    .unwrap_or(false);
            envs.insert(
                st.sk.clone(),
                Rc::new(TwitchEnv {
                    source: st.source.clone(),
                    badge_map: d.badge_map.clone(),
                    cheermotes: d.cheermotes.clone(),
                    emotes,
                    seven_defs: defs.clone(),
                    caps: d.caps.clone(),
                    viewer: ModViewer { id: me_id.clone(), broadcaster, lead },
                    me_login_lower: me_login.clone(),
                    my_personal: my_personal.clone(),
                }),
            );
        }
        self.envs = envs;
        let mut kick_emotes = HashMap::new();
        if rs.seven_tv {
            for (sk, ps) in &self.kick_seven {
                if self.store.source(sk).is_some() {
                    kick_emotes.insert(sk.clone(), Arc::new(merge_emote_sets(&ps.seven, &empty, &empty)));
                }
            }
        }
        self.kick_emotes = kick_emotes;
        self.sync_seven_subs();
    }

    // ---- live 7TV emote sets (EventAPI) ----------------------------------------------------------------

    /// Hold an EventAPI subscription for each Twitch or Kick source whose 7TV ids are known (7TV on), and nothing else.
    fn sync_seven_subs(&mut self) {
        let mut want: HashMap<String, SevenTvIds> = HashMap::new();
        if self.settings.seven_tv {
            for st in &self.store.sources {
                let sets = match st.source.platform {
                    Platform::Twitch => self.twitch.get(&st.sk).and_then(|d| d.provider_sets.as_ref()),
                    Platform::Kick => self.kick_seven.get(&st.sk),
                    _ => None,
                };
                if let Some(ids) = sets.and_then(|p| p.seven_ids.clone()) {
                    want.insert(st.sk.clone(), ids);
                }
            }
        }
        if want == self.seven_subs {
            return;
        }
        // new ones first: a channel that switched sets keeps its user subscription (and the socket) throughout
        let old = std::mem::take(&mut self.seven_subs);
        for (sk, ids) in &want {
            if old.get(sk) != Some(ids) {
                seven_watch(&self.svc, ids, true);
            }
        }
        for (sk, ids) in &old {
            if want.get(sk) != Some(ids) {
                seven_watch(&self.svc, ids, false);
            }
        }
        self.seven_subs = want;
    }

    fn on_seven_event(&mut self, ev: &SevenEvent, cx: &mut Context<Self>) {
        match ev {
            SevenEvent::Dispatch(Dispatch::EmoteSet { set_id, actor, changes }) => {
                let sks: Vec<String> = self.seven_subs.iter().filter(|(_, ids)| same_id(&ids.set_id, set_id)).map(|(sk, _)| sk.clone()).collect();
                if sks.is_empty() || changes.is_empty() {
                    return;
                }
                for sk in &sks {
                    if let Some(ps) = self.seven_sets_mut(sk) {
                        apply_changes(&mut Arc::make_mut(ps).seven.channel, changes);
                    }
                    self.forget_seven_cache(sk);
                    for c in changes {
                        let (action, e, old_name) = match c {
                            EmoteChange::Added(e) => ("added", e, ""),
                            EmoteChange::Removed(e) => ("removed", e, ""),
                            EmoteChange::Updated { old, new } if old.name != new.name => ("renamed", new, old.name.as_str()),
                            // only its flags changed: nothing to tell
                            EmoteChange::Updated { .. } => continue,
                        };
                        self.push_seven_notice(sk, json!({"actor": actor, "action": action, "emoteId": e.id, "name": e.name, "oldName": old_name}));
                    }
                }
                self.after_seven_change(cx);
            }
            SevenEvent::Dispatch(Dispatch::UserSet { user_id, actor, set_id, set_name }) => {
                let sks: Vec<String> = self.seven_subs.iter().filter(|(_, ids)| same_id(&ids.user_id, user_id) && !same_id(&ids.set_id, set_id)).map(|(sk, _)| sk.clone()).collect();
                for sk in sks {
                    self.forget_seven_cache(&sk);
                    let ids = SevenTvIds { user_id: user_id.clone(), set_id: set_id.clone() };
                    let note = json!({"actor": actor, "action": "set", "name": if set_name.is_empty() { "another set".to_string() } else { set_name.clone() }});
                    let client = self.svc.net.client.clone();
                    let sid = set_id.clone();
                    let task = self.svc.net.run(async move { fetch_seven_tv_set(&client, &sid).await });
                    cx.spawn(async move |this, cx| {
                        let Some(Some(map)) = task.await else { return };
                        let _ = this.update(cx, |p, cx| {
                            if let Some(ps) = p.seven_sets_mut(&sk) {
                                let ps = Arc::make_mut(ps);
                                ps.seven.channel = map;
                                ps.seven_ids = Some(ids);
                            }
                            p.push_seven_notice(&sk, note);
                            p.after_seven_change(cx);
                        });
                    })
                    .detach();
                }
            }
            SevenEvent::Dispatch(Dispatch::Other) => {}
            SevenEvent::Resynced => {
                // the socket was down for a while: whatever changed meanwhile comes with a fresh load
                for sk in self.seven_subs.keys().cloned().collect::<Vec<_>>() {
                    let Some((platform, cid)) = self.seven_owner(&sk) else { continue };
                    let task = self.svc.net.run(reload_seven_tv(self.svc.net.client.clone(), platform, cid));
                    cx.spawn(async move |this, cx| {
                        let Some((seven, Some(ids))) = task.await else { return };
                        let _ = this.update(cx, |p, cx| {
                            if let Some(ps) = p.seven_sets_mut(&sk) {
                                let ps = Arc::make_mut(ps);
                                ps.seven.channel = seven.channel;
                                if !seven.global.is_empty() {
                                    ps.seven.global = seven.global;
                                }
                                ps.seven_ids = Some(ids);
                            }
                            p.data_changed(cx);
                        });
                    })
                    .detach();
                }
            }
        }
    }

    /// The channel's cached 7TV answer is stale now: a restart must not bring the old set back.
    fn forget_seven_cache(&self, sk: &str) {
        if let Some((platform, cid)) = self.seven_owner(sk) {
            forget_cache(&seven_tv_channel_key_on(platform, &cid));
            let _ = self.svc.net.rt.spawn(async { flush_cache() });
        }
    }

    /// One compact line in the channel's chat about a 7TV change (`note`: a `SevenTvNote` as JSON).
    fn push_seven_notice(&mut self, sk: &str, note: Value) {
        self.seven_seq += 1;
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let s = |k: &str| note.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let system = match s("action").as_str() {
            "renamed" => format!("{} renamed the 7TV emote {} to {}", s("actor"), s("oldName"), s("name")),
            "set" => format!("{} switched the channel's 7TV emote set to {}", s("actor"), s("name")),
            a => format!("{} {a} the 7TV emote {}", s("actor"), s("name")),
        };
        let notice = json!({"id": format!("7tv-{ts}-{}", self.seven_seq), "ts": ts, "msgId": "seventv", "system": system.trim(), "seventv": note});
        self.store.apply(sk, &json!({"type": "chatnotice", "notice": notice}));
    }

    fn after_seven_change(&mut self, cx: &mut Context<Self>) {
        self.data_changed(cx);
        self.refresh(cx);
    }

    // ---- frames ------------------------------------------------------------------------------------------

    fn on_inbox(&mut self, batch: Vec<Inbox>, cx: &mut Context<Self>) {
        for m in batch {
            match m {
                Inbox::Reset(sk) => self.store.reset_source(&sk),
                Inbox::Frame(sk, f) => {
                    // an emote list that arrives after the chat (Rumble, Kick and YouTube send them late) rebuilds
                    // that source's send box inventory
                    if f.get("type").and_then(|t| t.as_str()).is_some_and(|t| t == "emotes" || t == "chatlog") {
                        self.inv_cache.borrow_mut().remove(&sk);
                    }
                    if super::hype_train::contributes(&f) {
                        self.hype_poke(&sk);
                    }
                    let susp_user = suspicious_user_of(&f);
                    self.store.apply(&sk, &f);
                    // Twitch names a monitored / restricted chatter just AFTER their line came in over IRC: the lines
                    // already drawn get the label (or lose it) now
                    if let Some(uid) = susp_user {
                        self.invalidate_rows(|r| r.sk == sk && row_user_id(r) == Some(uid.as_str()));
                        self.thread_built.borrow_mut().clear();
                    }
                }
                Inbox::SubError(sk, msg) => {
                    self.store.apply(&sk, &json!({"type": "conn", "state": "error", "detail": msg, "canSend": false, "readOnlyReason": msg}));
                }
            }
        }
        // status for the split header
        let mut changed = false;
        for st in &self.store.sources {
            if let Some(c) = &st.conn {
                let differs = self.status.get(&st.sk).map(|o| (&o.state, &o.live, &o.title, &o.detail, &o.can_send) != (&c.state, &c.live, &c.title, &c.detail, &c.can_send)).unwrap_or(true);
                if differs {
                    self.status.insert(st.sk.clone(), c.clone());
                    changed = true;
                }
            }
        }
        if changed {
            cx.emit(PaneEvent::Status);
        }
        self.start_loads(cx);
        self.refresh(cx);
        self.scan_alerts(cx);
        cx.notify();
    }

    // ---- the shown list ---------------------------------------------------------------------------------

    /// Rows after the event toggles, deleted-message hiding, ignores and search.
    fn compute_shown(&mut self) -> Vec<RowRef> {
        let visible = agu_chat_model::shown::visible_rows(self.store.rows(), &self.settings);
        let mut shown = filter_ignored(&visible, &self.ignores, &mut self.ignore_cache);
        if self.search_open {
            let q = parse_search(&self.query);
            if !is_empty_query(&q) {
                shown.retain(|r| match_row(r, &q));
            }
        }
        shown
    }

    pub(super) fn frozen(&self) -> bool {
        self.hover_held || self.alt_held || self.sel_held
    }

    fn update_store_hold(&mut self) {
        let held = self.scroll_held || self.frozen() || self.search_open;
        self.store.set_held(held);
    }

    /// Recompute the shown rows and tell the list what changed. While the list is frozen (hover / Alt pause)
    /// the new rows wait in `pending_rows`.
    pub(super) fn refresh(&mut self, _cx: &mut Context<Self>) {
        let new_rows = self.compute_shown();
        if self.frozen() {
            self.pending_rows = Some(new_rows);
            return;
        }
        self.pending_rows = None;
        self.apply_rows(new_rows);
    }

    fn apply_rows(&mut self, new: Vec<RowRef>) {
        self.settle = 3;
        if self.shown.is_empty() {
            self.shown = new.into_iter().map(|row| ShownRow { row, built: RefCell::new(None) }).collect();
            self.list_state.reset(self.shown.len());
            self.drop_selection();
            return;
        }
        // Match old and new rows by key, whatever changed: trimmed at the top, appended, an out-of-order row put in
        // the middle (a split's other chat, a late line), a row removed. A scrolled-up view keeps its row through all
        // of it (before, anything but trim/append reset the list, which snapped a paused busy chat back down).
        let new_ix: HashMap<&str, usize> = new.iter().enumerate().map(|(i, r)| (r.key.as_str(), i)).collect();
        let map: Vec<Option<(usize, bool)>> = self.shown.iter().map(|o| new_ix.get(o.row.key.as_str()).map(|&j| (j, Arc::ptr_eq(&o.row, &new[j])))).collect();
        drop(new_ix);
        let mut old: Vec<Option<ShownRow>> = std::mem::take(&mut self.shown).into_iter().map(Some).collect();
        let mut reuse: Vec<Option<ShownRow>> = (0..new.len()).map(|_| None).collect();
        for (i, m) in map.iter().enumerate() {
            if let Some((j, true)) = m {
                reuse[*j] = old[i].take();
            }
        }
        self.shown = new.into_iter().zip(reuse).map(|(row, kept)| kept.unwrap_or(ShownRow { row, built: RefCell::new(None) })).collect();
        self.list_state.remap(self.shown.len(), &map);
        // a selection follows its rows (a row that changed keeps its units: the same message)
        if !self.sel_main.remap_rows(|r| map.get(r as usize).copied().flatten().map(|(j, _)| j as u32)) {
            self.sel_held = false;
        }
    }

    /// Every row's built form is stale (settings, env): drop the caches and re-measure, keeping the scroll spot.
    pub(super) fn invalidate_all(&mut self) {
        self.settle = 3;
        for s in &self.shown {
            *s.built.borrow_mut() = None;
        }
        self.list_state.invalidate_all();
    }

    /// One row's content changed (an image got its size, a chatter's 7TV style arrived).
    pub(super) fn invalidate_rows(&mut self, pred: impl Fn(&Row) -> bool) {
        for (i, s) in self.shown.iter().enumerate() {
            if pred(&s.row) {
                *s.built.borrow_mut() = None;
                self.list_state.invalidate(i);
            }
        }
    }

    pub(super) fn is_own(&self, r: &Row) -> bool {
        super::mentions::is_own(r, &self.accounts)
    }

    pub(super) fn rule_of(&mut self, r: &RowRef) -> Option<HighlightRule> {
        if self.highlighter.size() == 0 {
            return None;
        }
        let key = Arc::as_ptr(r) as usize;
        if let Some(hit) = self.rule_cache.get(&key) {
            return hit.clone();
        }
        let hit = match_input_of(r).filter(|_| !self.is_own(r)).and_then(|i| self.highlighter.matches(&i).cloned());
        self.rule_cache.insert(key, hit.clone());
        hit
    }

    // ---- alerts and tab activity: once per live row (history batches never ring) -----------------------------

    fn scan_alerts(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<RowRef> = self.store.rows().to_vec();
        let me_login = self.me_login_lower();
        let kick_me = self.kick_me();
        let mut max_ord = self.seen_ord;
        let mut activity: Option<Activity> = None;
        let mut fire: Vec<(String, AlertWants)> = Vec::new();
        let mut mentions: Vec<RowRef> = Vec::new();
        // A whisper that opened its tab came in with the conversation's snapshot: its newest line is the news.
        if self.whisper_pending {
            if let Some(r) = rows.iter().rev().find(|r| is_whisper_key(&r.sk) && matches!(r.body, RowBody::Msg(_))) {
                self.whisper_pending = false;
                if !self.is_own(r) && !super::mentions::row_deleted(r) && apply_ignores(r, &self.ignores).is_some() {
                    mentions.push(r.clone());
                }
            }
        }
        for r in &rows {
            if r.ord <= self.seen_ord {
                continue;
            }
            max_ord = max_ord.max(r.ord);
            let (own, hidden) = (self.is_own(r), apply_ignores(r, &self.ignores).is_none());
            // ignored rows never ring
            let Some(alert) = super::mentions::classify(r, own, hidden, &me_login, &kick_me, || self.rule_of(r)) else { continue };
            activity.get_or_insert(Activity::Message);
            if alert.red() {
                activity = Some(Activity::Highlight);
                mentions.push(r.clone());
            }
            let wants = match alert {
                Alert::Plain => continue,
                Alert::Mention => AlertWants { sound: self.settings.mention_sound, sound_file: String::new(), flash: self.settings.mention_flash },
                Alert::Rule(rule) => AlertWants { sound: rule.sound, sound_file: rule.sound_file.clone(), flash: rule.flash },
            };
            let key = super::mentions::mention_id(r);
            fire.push((key, wants));
        }
        self.seen_ord = max_ord;
        for (key, wants) in fire {
            let svc = self.svc.clone();
            let bridge = self.bridge.clone();
            let mut load = |file: &str| -> Option<Arc<Vec<u8>>> {
                // the sounds folder is read through the bridge; the future is ready at once for local files
                let fut = bridge.sounds_read(file.to_string());
                futures::executor::block_on(fut).ok().map(Arc::new)
            };
            svc.alerts.alert_once(&key, &wants, &self.settings, svc.window_active.get(), svc.hwnd.get(), &mut load);
        }
        if let Some(a) = activity {
            cx.emit(PaneEvent::Activity(a));
        }
        // mentions reported before that were deleted since leave the panel
        if !self.mention_keys.is_empty() {
            let gone: Vec<RowRef> = rows.iter().filter(|r| self.mention_keys.contains(&r.key) && super::mentions::row_deleted(r)).cloned().collect();
            if !gone.is_empty() {
                for r in &gone {
                    self.mention_keys.remove(&r.key);
                }
                cx.emit(PaneEvent::MentionsGone(gone.iter().map(|r| super::mentions::mention_id(r)).collect()));
            }
        }
        if !mentions.is_empty() {
            for r in &mentions {
                self.mention_keys.insert(r.key.clone());
            }
            // the panel keeps 200: so many keys here can't all still be on it
            if self.mention_keys.len() > 2 * super::mentions::MAX_MENTIONS {
                let live: HashSet<&str> = rows.iter().map(|r| r.key.as_str()).collect();
                self.mention_keys.retain(|k| live.contains(k.as_str()));
            }
            cx.emit(PaneEvent::Mentions(mentions));
        }
    }

    // ---- scrolling / pausing (MessageList) ------------------------------------------------------------------

    pub(super) fn last_ord(&self) -> u64 {
        self.shown.last().map(|s| s.row.ord).unwrap_or(0)
    }

    /// Called every frame: the user scrolled up (hold) or came back within 40 px of the bottom (resume).
    pub(super) fn track_scroll(&mut self) {
        let following = self.list_state.following();
        // (not while a selection holds the list: a drag past the bottom edge scrolls it there a step at a time)
        if !following && !self.sel_held && self.list_state.dist_to_bottom() <= 40.0 {
            self.list_state.follow();
        }
        let held = !self.list_state.following();
        if held && !self.scroll_held {
            self.scroll_held = true;
            self.pause_mark = self.last_ord();
            self.update_store_hold();
        } else if !held && self.scroll_held && !self.frozen() {
            self.resume_inner();
        } else if !held && self.scroll_held {
            self.scroll_held = false;
            self.update_store_hold();
        }
    }

    fn resume_inner(&mut self) {
        self.scroll_held = false;
        self.update_store_hold();
        self.needs_refresh = true;
    }

    /// Clicking the pill / closing the search: back to the live view.
    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.scroll_held = false;
        self.hover_held = false;
        self.alt_held = false;
        self.drop_selection();
        self.update_store_hold();
        self.refresh(cx);
        self.scroll_to_end();
        cx.notify();
    }

    pub(super) fn scroll_to_end(&mut self) {
        self.list_state.follow();
    }

    /// Pause on mouse-over (settings.pauseChat == "hover").
    pub(super) fn set_hover_hold(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.settings.pause_chat != "hover" || self.hover_held == on {
            return;
        }
        self.set_freeze(on, cx, true);
    }

    /// Pause while Alt is held (settings.pauseChat == "alt"); the layout forwards modifier changes.
    pub fn set_alt(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.settings.pause_chat != "alt" || self.alt_held == on {
            return;
        }
        self.set_freeze(on, cx, false);
    }

    fn set_freeze(&mut self, on: bool, cx: &mut Context<Self>, hover: bool) {
        if on {
            if self.scroll_held {
                // already held by scrolling: the list is not at the bottom anyway
            }
            if hover { self.hover_held = true } else { self.alt_held = true }
            if !self.frozen() || self.pause_mark == 0 || !self.scroll_held {
                self.pause_mark = self.last_ord();
            }
        } else if hover {
            self.hover_held = false;
        } else {
            self.alt_held = false;
        }
        self.update_store_hold();
        if !self.frozen() {
            self.refresh(cx);
        }
        cx.notify();
    }

    /// The number shown in the pill: rows that arrived since the pause began.
    pub(super) fn new_count(&self) -> usize {
        let rows: &[RowRef] = match &self.pending_rows {
            Some(p) => p,
            None => &[],
        };
        let base = self.shown.iter().rev().take_while(|s| s.row.ord > self.pause_mark).count();
        let extra = if rows.is_empty() { 0 } else { rows.iter().rev().take_while(|r| r.ord > self.pause_mark).count() };
        if self.pending_rows.is_some() { extra } else { base }
    }

    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_open {
            self.search_open = true;
            self.pause_mark = self.last_ord();
            self.update_store_hold();
            if self.search.is_none() {
                let input = cx.new(|cx| TextInput::new(window, cx, "Search messages, from:name", InputStyle::Bare));
                self._search_sub = Some(cx.subscribe_in(&input, window, |this, input, ev: &TextInputEvent, window, cx| match ev {
                    TextInputEvent::Changed => {
                        this.query = input.read(cx).text();
                        this.refresh(cx);
                        this.scroll_held = false;
                        this.scroll_to_end();
                        cx.notify();
                    }
                    TextInputEvent::Escape => this.close_search(window, cx),
                    _ => {}
                }));
                self.search = Some(input);
            }
        }
        if let Some(s) = &self.search {
            s.update(cx, |s, cx| s.select_all_text(cx));
            s.read(cx).focus(window);
        }
        cx.notify();
    }

    pub fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.search_open {
            return;
        }
        self.search_open = false;
        self.query.clear();
        if let Some(s) = &self.search {
            s.update(cx, |s, cx| s.set_text("", cx));
        }
        let _ = window;
        self.resume(cx);
    }

    // ---- toasts ------------------------------------------------------------------------------------------------

    pub(super) fn toast(&mut self, text: impl Into<String>, warn: bool, cx: &mut Context<Self>) {
        self.toast_seq += 1;
        let id = self.toast_seq;
        self.toasts.push(Toast { id, text: text.into(), warn });
        while self.toasts.len() > MAX_TOASTS {
            self.toasts.remove(0);
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(if warn { 6000 } else { 2800 })).await;
            let _ = this.update(cx, |p, cx| {
                p.toasts.retain(|t| t.id != id);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn show_toast(&mut self, m: ToastMsg, cx: &mut Context<Self>) {
        self.toast_msg(m, cx)
    }

    pub(super) fn bridge_clone(&self) -> Arc<dyn AguBridge> {
        self.bridge.clone()
    }

    /// Test aids (env, like `AGU_SHOT`): put the pane into an interactive state without synthetic input.
    /// `AGU_HOVER=n` hovers the n-th row from the end, `AGU_SEARCH=query` opens split search with that query,
    /// `AGU_SCROLL_UP=px` scrolls the list up, `AGU_PIN_OPEN=1` / `AGU_PIN_MENU=1` open the pinned card, `AGU_TIP=1`
    /// shows an emote tooltip.
    pub(super) fn test_aids(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.aids_done || self.shown.is_empty() {
            return;
        }
        self.aid_frames += 1;
        let need = std::env::var("AGU_AID_FRAMES").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(40);
        if self.aid_frames < need {
            window.request_animation_frame();
            return;
        }
        self.aids_done = true;
        if let Ok(path) = std::env::var("AGU_DUMP_ROWS") {
            // row boxes as the list laid them out (y relative to the list's top), for comparing with the DOM
            let mut out = String::new();
            for (i, s) in self.shown.iter().enumerate() {
                if let Some((y, h)) = self.list_state.bounds_for_item(i) {
                    out.push_str(&format!("{i}	{:.2}	{:.2}	{}
", y, h, s.row.key));
                }
            }
            let _ = std::fs::write(path, out);
        }
        if let Some(n) = std::env::var("AGU_HOVER").ok().and_then(|v| v.parse::<usize>().ok()) {
            let n = self.shown.len().saturating_sub(1 + n);
            self.hover_row = Some(self.shown[n].row.key.clone());
        }
        if let Ok(q) = std::env::var("AGU_SEARCH") {
            self.open_search(window, cx);
            self.query = q.clone();
            if let Some(s) = &self.search {
                s.update(cx, |s, cx| s.set_text(&q, cx));
            }
            self.refresh(cx);
            self.scroll_to_end();
        }
        if let Some(px) = std::env::var("AGU_SCROLL_UP").ok().and_then(|v| v.parse::<f32>().ok()) {
            self.list_state.scroll_by(-px);
        }
        if std::env::var("AGU_PIN_OPEN").is_ok() || std::env::var("AGU_PIN_MENU").is_ok() {
            let ids: Vec<(String, String)> = self.store.sources.iter().filter_map(|s| s.pinned.as_ref().map(|p| (s.sk.clone(), p.id.clone()))).collect();
            for (sk, id) in ids {
                if std::env::var("AGU_PIN_OPEN").is_ok() {
                    self.pin_open.insert(id.clone());
                }
                if std::env::var("AGU_PIN_MENU").is_ok() {
                    let after = std::env::var("AGU_PIN_AFTER").is_ok();
                    self.pin_menu = Some((format!("{sk}|{id}"), after));
                    if after {
                        let input = cx.new(|cx| {
                            let mut t = TextInput::new(window, cx, "", InputStyle::Chat);
                            t.set_text("20", cx);
                            t.set_centered(true);
                            t
                        });
                        let sub = cx.subscribe(&input, |_this, _i, _ev: &TextInputEvent, _cx| {});
                        self.unpin_after = Some(UnpinAfter { time_mode: true, unit_min: true, amount: input, busy: false, _sub: sub });
                    }
                }
            }
        }
        // `AGU_SHOT=usercard|modpopover`: open the card / popover of the newest message's author as a click on its name would
        if let Ok(shot) = std::env::var("AGU_SHOT") {
            if shot == "usercard" || shot == "modpopover" {
                let target = self.store.rows().iter().rev().find_map(|r| match &r.body {
                    RowBody::Msg(m) if !m.user.id.is_empty() => Some((r.sk.clone(), m.clone())),
                    _ => None,
                });
                if let Some((sk, m)) = target {
                    self.last_act = Some((gpui::Bounds::new(point(px(60.), px(150.)), gpui::size(px(80.), px(16.))), window.bounds()));
                    if shot == "usercard" {
                        self.open_card(sk, CardUser { id: m.user.id.clone(), login: m.user.login.clone(), name: m.user.name.clone(), avatar: String::new() }, cx);
                    } else {
                        let kind = std::env::var("AGU_POP_KIND").unwrap_or_else(|_| "ban".into());
                        self.run_action(RowAction::Popover { sk, msg: Box::new(m), kind: if kind == "warn" { "warn" } else { "ban" } }, cx);
                    }
                }
            }
        }
        // `AGU_SHOT=thread`: open the reply thread of the newest reply, as a click on its "replying to" line would
        if std::env::var("AGU_SHOT").is_ok_and(|s| s == "thread") {
            let target = self.store.rows().iter().rev().find_map(|r| {
                let l = super::thread::reply_link(r)?;
                (!l.parent.is_empty()).then(|| (r.sk.clone(), l.parent.to_string(), l.thread.to_string()))
            });
            if let Some((sk, parent, thread)) = target {
                self.open_thread(&sk, &parent, &thread, cx);
            }
        }
        if std::env::var("AGU_TIP").is_ok() {
            self.tip = Some((Rc::new(Tip::new("https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0", "Kappa", "Twitch")), gpui::point(gpui::px(120.), gpui::px(200.))));
        }
        cx.notify();
    }

    /// A pin with an end time re-renders every second so its card goes away on time.
    pub(super) fn ensure_pin_tick(&mut self, cx: &mut Context<Self>) {
        if self.pin_tick.is_some() {
            return;
        }
        self.pin_tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            if this
                .update(cx, |p, cx| {
                    let any = p.store.sources.iter().any(|s| s.pinned.as_ref().and_then(|m| m.pin.as_ref()).and_then(|x| x.ends_at.as_ref()).is_some());
                    if !any {
                        p.pin_tick = None;
                    }
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }));
    }

    fn toast_msg(&mut self, m: ToastMsg, cx: &mut Context<Self>) {
        if let Some((t, warn)) = m {
            self.toast(t, warn, cx);
        }
    }

    // ---- row actions (Handlers) -----------------------------------------------------------------------------------

    pub(super) fn ref_of(&self, sk: &str) -> Option<SourceRef> {
        self.store.source(sk).map(|s| s.source.clone())
    }

    /// Whether this account may moderate any chat in this split, so the header shows the mod mode toggle: the
    /// streamer or a mod on Twitch, Kick's and Rumble's own answers, and a YouTube login (YouTube says whether
    /// it is allowed only when an action is tried). X chat has no moderation.
    pub fn can_moderate_here(&self) -> bool {
        self.store.sources.iter().any(|st| match st.source.platform {
            Platform::Twitch => self
                .twitch
                .get(&st.sk)
                .map(|d| d.caps.is_broadcaster == Some(true) || d.caps.is_mod == Some(true) || d.caps.ban || d.caps.delete)
                .unwrap_or(false),
            Platform::Kick | Platform::Rumble => self.co_caps.get(&st.sk).map(|c| c.can_moderate).unwrap_or(false),
            Platform::Youtube => self.accounts.get(&Platform::Youtube).map(|a| a.connected).unwrap_or(false),
            _ => false,
        })
    }

    /// Ctrl+click on a name: the message box starts with "@name " (kept once if it already does), caret at the
    /// end, box focused. The display name, unless it has characters chat would not match (a localized Twitch
    /// name), then the login.
    /// `sk` is the chat the person wrote in: the box switches to it.
    pub(super) fn mention_user(&mut self, sk: Option<&str>, user: &super::rows::CardUser, window: &mut Window, cx: &mut Context<Self>) {
        let who = if !user.name.is_empty() && (user.name.is_ascii() || user.login.is_empty()) { &user.name } else { &user.login };
        if who.is_empty() {
            return;
        }
        let Some(sb) = self.send_box.clone() else { return };
        let input = sb.read(cx).input.clone();
        let tag = format!("@{who}");
        let cur = input.read(cx).text();
        let rest = cur.trim_start();
        let lower = rest.to_lowercase();
        let rest = if lower == tag.to_lowercase() || lower.starts_with(&format!("{} ", tag.to_lowercase())) { rest[tag.len()..].trim_start() } else { rest };
        let next = if rest.is_empty() { format!("{tag} ") } else { format!("{tag} {rest}") };
        input.update(cx, |i, cx| i.replace_all(&next, next.len(), cx));
        if let Some(sk) = sk {
            sb.update(cx, |sb, cx| sb.target_chat(sk, cx));
        }
        input.read(cx).focus(window);
    }

    /// Pin a message for `seconds` (None = until unpinned or the stream ends).
    pub(super) fn pin_with(&mut self, sk: String, id: String, seconds: Option<u32>, cx: &mut Context<Self>) {
        let bridge = self.bridge.clone();
        let Some(src) = self.ref_of(&sk) else { return };
        cx.spawn(async move |this, cx| {
            let (_, t) = mod_actions::pin_message(&bridge, &src, &id, seconds).await;
            let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
        })
        .detach();
    }

    pub(super) fn run_action(&mut self, a: RowAction, cx: &mut Context<Self>) {
        let bridge = self.bridge.clone();
        match a {
            RowAction::Copy(text) => {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
                self.toast("Copied", false, cx);
            }
            RowAction::Reply(t) => {
                self.reply_target = Some(t);
                cx.notify();
            }
            // The pin button opens the "Pin for" menu itself (it needs the click position); a pin without a
            // menu (anything else dispatching this) keeps the 20 minute default.
            RowAction::Pin { sk, id } => self.pin_with(sk, id, Some(1200), cx),
            RowAction::Delete { sk, id } => {
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let (ok, t) = mod_actions::twitch_delete(&bridge, &src, &id).await;
                    let _ = this.update(cx, |p, cx| {
                        if ok {
                            p.store.mark_deleted(&sk, &id);
                            p.refresh(cx);
                        }
                        p.toast_msg(t, cx);
                    });
                })
                .detach();
            }
            RowAction::QuickTimeout { sk, msg, seconds } => {
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let body = json!({"action": "timeout", "userId": msg.user.id, "duration": seconds});
                    let ok = format!("timed out {} ({})", msg.user.name, format_duration(seconds as i64));
                    let (_, t) = mod_actions::twitch_action(&bridge, &src, body, Some(ok)).await;
                    let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
                })
                .detach();
            }
            RowAction::TwitchCommand { sk, msg, template } => {
                let Some(src) = self.ref_of(&sk) else { return };
                let line = expand_template(&template, &TemplateVars { user: &msg.user.login, name: Some(&msg.user.name), user_id: Some(&msg.user.id), msg_id: Some(&msg.id), channel: Some(&src.channel), msg: Some(&msg.text) }).trim().to_string();
                if line.is_empty() {
                    return;
                }
                if line.starts_with('/') && !(line.to_lowercase() == "/me" || line.to_lowercase().starts_with("/me ")) {
                    // runSlashCommand lives with the input box (chat/input/slash.ts), a later round
                    self.toast("Slash commands arrive with the input box", true, cx);
                    return;
                }
                let text: String = line.chars().take(500).collect();
                cx.spawn(async move |this, cx| {
                    let r = relay_api(&bridge, &src, "/api/chat/send", Some("POST"), Some(json!({"text": text}))).await;
                    if !r.ok {
                        let msg = match &r.body {
                            Value::Object(o) => o.get("error").and_then(|e| e.as_str()).unwrap_or("send failed").to_string(),
                            other => other.to_string(),
                        };
                        let _ = this.update(cx, |p, cx| p.toast(msg, true, cx));
                    }
                })
                .detach();
            }
            RowAction::Popover { sk, msg, kind } => {
                // `openModPop`: below the button, or above it in the lower half
                let Some((rect, wb)) = self.last_act else { return };
                let above = f32::from(rect.top()) > f32::from(wb.size.height) / 2.0;
                let pos = if above { point(rect.left(), rect.top() - px(4.)) } else { point(rect.left(), rect.bottom() + px(4.)) };
                self.pop_req = Some(PopReq { sk, msg: *msg, kind: super::mod_popover::PopKind::parse(kind), pos, above });
                cx.notify();
            }
            RowAction::Card { sk, user } => self.open_card(sk, user, cx),
            RowAction::Thread { sk, parent, thread } => self.open_thread(&sk, &parent, &thread, cx),
            RowAction::Automod { sk, id, allow } => {
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let (res, t) = mod_actions::resolve_automod(&bridge, &src, &id, if allow { "allow" } else { "deny" }).await;
                    let _ = this.update(cx, |p, cx| {
                        if res == AutomodResult::Gone {
                            p.store.mark_automod_expired(&sk, &id);
                            p.refresh(cx);
                        }
                        p.toast_msg(t, cx);
                    });
                })
                .detach();
            }
            RowAction::RumbleMod { sk, action, msg, seconds } => {
                if action == "ban" && self.confirm.is_none() {
                    let who = if msg.user.login.is_empty() { msg.user.name.clone() } else { msg.user.login.clone() };
                    self.confirm = Some(Confirm { text: format!("Ban {who} from the Rumble chat?"), run: Some(RowAction::RumbleMod { sk, action: "ban!", msg, seconds }) });
                    cx.notify();
                    return;
                }
                let action = if action == "ban!" { "ban" } else { action };
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let t = mod_actions::rumble_mod_action(&bridge, &src, action, &msg, seconds).await;
                    let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
                })
                .detach();
            }
            RowAction::YoutubeMod { sk, action, msg } => {
                if action == "ban" && self.confirm.is_none() {
                    self.confirm = Some(Confirm { text: format!("Ban {} from the YouTube chat?", if msg.user.name.is_empty() { "this user" } else { &msg.user.name }), run: Some(RowAction::YoutubeMod { sk, action: "ban!", msg }) });
                    cx.notify();
                    return;
                }
                let action = if action == "ban!" { "ban" } else { action };
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let t = mod_actions::youtube_mod_action(&bridge, &src, action, &msg).await;
                    let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
                })
                .detach();
            }
            RowAction::KickMod { sk, action, msg, seconds } => {
                if action == "ban" && self.confirm.is_none() {
                    self.confirm = Some(Confirm { text: format!("Ban {} from the Kick chat?", if msg.user.name.is_empty() { "this user" } else { &msg.user.name }), run: Some(RowAction::KickMod { sk, action: "ban!", msg, seconds }) });
                    cx.notify();
                    return;
                }
                let action = if action == "ban!" { "ban" } else { action };
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let t = mod_actions::kick_mod_action(&bridge, &src, action, &msg, seconds).await;
                    let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
                })
                .detach();
            }
        }
    }

    /// `Handlers.openCard`: one card per user (an open one is brought to the front), new ones stagger, at most 4.
    fn open_card(&mut self, sk: String, user: CardUser, cx: &mut Context<Self>) {
        let Some(st) = self.store.source(&sk) else { return };
        let platform = st.source.platform;
        let id = format!("{sk}|{}", if user.id.is_empty() { user.name.to_lowercase() } else { user.id.clone() });
        // windows the user closed are gone
        let live: std::collections::HashSet<_> = cx.windows().into_iter().map(|w| w.window_id()).collect();
        self.cards.retain(|(_, h)| live.contains(&h.window_id()));
        if let Some((_, h)) = self.cards.iter().find(|(i, _)| *i == id) {
            let _ = h.update(cx, |_, w, _| w.activate_window());
            return;
        }
        let Some((rect, wb)) = self.last_act else { return };
        let stagger = 24.0 * (self.cards.len() % 4) as f32;
        let above = f32::from(rect.top()) > f32::from(wb.size.height) / 2.0;
        // A card the user once resized by hand sets the size of every card after it.
        let remembered = crate::window_state::card_size();
        let (cw, ch) = remembered.map(|s| (s.width.max(240.0), s.height.max(super::popout::POPOUT_MIN_H))).unwrap_or((340.0, super::popout::POPOUT_H0));
        // The pop-out opens beside the name: below it, or above it in the lower half (cards run ~380px tall).
        let up = if remembered.is_some() { ch } else { 380.0 };
        let x = f32::from(wb.origin.x) + f32::from(rect.left()) + stagger;
        let y = f32::from(wb.origin.y) + if above { f32::from(rect.top()) - up } else { f32::from(rect.bottom()) + 4.0 } + stagger;
        let user_sized = remembered.is_some();
        let weak = cx.entity().downgrade();
        let title = format!("{} - Agu Chat", user.name);
        let (id2, sk2) = (id.clone(), sk.clone());
        // Opened a beat later: the new window reads this pane while it builds, which it can't while we are updating it.
        cx.spawn(async move |this, cx| {
            let handle = cx.update(|cx| {
                super::popout::open_popout(cx, &title, x, y, cw, ch, move |window, cx| {
                    cx.new(|cx| super::user_card::UserCardView::new(window, cx, weak, id2, sk2, platform, user, user_sized))
                })
            });
            let Ok(Some(h)) = handle else { return };
            let _ = this.update(cx, |p, cx| {
                if p.cards.len() >= 4 {
                    let (_, old) = p.cards.remove(0);
                    let _ = old.update(cx, |_, w, _| w.remove_window());
                }
                p.cards.push((id, h));
            });
        })
        .detach();
    }

    /// What a user card draws from this pane (None once the source is gone).
    pub(super) fn card_ctx(&self, sk: &str, user: &CardUser, platform: Platform) -> Option<super::user_card::CardCtx> {
        let st = self.store.source(sk)?;
        let env = self.envs.get(sk);
        let caps = env.map(|e| e.caps.clone()).unwrap_or_default();
        let me_id = self.me_id();
        let reach: ModReach = if matches!(platform, Platform::Kick | Platform::Rumble) {
            let badges: Vec<String> = self
                .store
                .rows()
                .iter()
                .rev()
                .find_map(|r| match &r.body {
                    RowBody::Kick(m) if r.sk == sk && m.user.id == user.id => Some(m.badges.iter().map(|b| b.r#type.clone()).collect()),
                    RowBody::Rumble(m) if r.sk == sk && m.user.id == user.id => Some(m.badges.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let names: Vec<&str> = badges.iter().map(|b| b.as_str()).collect();
            let target = if user.login.is_empty() { &user.name } else { &user.login };
            super::mod_caps::co_reach(platform, &self.co_login(platform), &st.source.channel, target, &names)
        } else if platform != Platform::Twitch {
            REACH_ALL
        } else if user.login.eq_ignore_ascii_case(&st.source.channel) {
            // the channel's own account, whether or not it has said anything yet
            ModReach { ban: false, timeout: false, warn: false, delete: false }
        } else if let Some(env) = env {
            // No line from them in chat = no badges to go on: nothing is ruled out except banning yourself.
            let badges = self.store.rows().iter().rev().find_map(|r| match &r.body {
                RowBody::Msg(m) if r.sk == sk && m.user.id == user.id => Some(m.badges.clone()),
                _ => None,
            });
            match badges {
                Some(b) => mod_reach(&env.viewer, &user.id, &b),
                None if user.id == me_id => ModReach { ban: false, timeout: false, warn: false, delete: true },
                None => REACH_ALL,
            }
        } else {
            REACH_ALL
        };
        let mine = |r: &Row| -> bool {
            r.sk == sk
                && match &r.body {
                    RowBody::Msg(m) => m.user.id == user.id,
                    RowBody::Rumble(m) => m.user.id == user.id,
                    // a raid or a KICKs gift without a message is not something they said
                    RowBody::Kick(m) => m.user.id == user.id && !m.text.is_empty(),
                    RowBody::YouTube(m) => m.user.id == user.id,
                    RowBody::X(m) => m.user.id == user.id,
                    _ => false,
                }
        };
        let recent: Vec<RowRef> = self.store.rows().iter().rev().filter(|r| mine(r)).take(super::user_card::RECENT_MAX).cloned().collect();
        let recent_total = self.store.rows().iter().filter(|r| mine(r)).count();
        let (badges, sub) = recent.first().map(|r| super::user_card::badges_of(r, env.map(|e| &*e.badge_map), &st.rumble_badges)).unwrap_or_default();
        let login = ignore_login(&user.login, &user.name);
        // your own card: nothing to ignore
        let my_login = if platform == Platform::Twitch { self.me_login_lower() } else { self.co_login(platform) };
        let is_me = (platform == Platform::Twitch && !me_id.is_empty() && user.id == me_id) || (!my_login.is_empty() && ignore_login(&my_login, "") == login);
        Some(super::user_card::CardCtx {
            source: st.source.clone(),
            caps,
            reach,
            suspicious: st.suspicious.get(&user.id).cloned(),
            co_can_moderate: self.co_caps.get(sk).map(|c| c.can_moderate).unwrap_or(false),
            ignored: !login.is_empty() && super::ignores::is_user_ignored(&self.settings.ignored_users, platform, &login),
            can_ignore: !login.is_empty() && !is_me,
            emotes: env.and_then(|e| e.emotes.clone()),
            recent,
            recent_total,
            readable_colors: self.settings.readable_colors,
            badges,
            sub,
            me: self.me_login_lower(),
        })
    }

    /// The last lines of a person in a source (the ban popover lists the newest 5), oldest first.
    pub(super) fn rows_of_user(&self, sk: &str, uid: &str) -> Vec<String> {
        self.store
            .rows()
            .iter()
            .filter_map(|r| match &r.body {
                RowBody::Msg(m) if r.sk == sk && m.user.id == uid => Some(m.text.clone()),
                _ => None,
            })
            .collect()
    }

    pub(super) fn mark_deleted(&mut self, sk: &str, id: &str, cx: &mut Context<Self>) {
        self.store.mark_deleted(sk, id);
        self.refresh(cx);
    }

    pub(super) fn confirm_yes(&mut self, cx: &mut Context<Self>) {
        if let Some(c) = self.confirm.take() {
            if let Some(a) = c.run {
                self.run_action(a, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn open_url(&self, url: &str) {
        self.bridge.open_external(url.to_string());
    }

    // ---- ignores ---------------------------------------------------------------------------------------------------

    /// Toggle an ignored user (UserCard's "Ignore"; used by the later user-card round).
    pub fn set_ignored(&mut self, platform: Platform, login: &str, on: bool, cx: &mut Context<Self>) {
        let cur = self.settings.ignored_users.clone();
        let next = toggle_ignored_user(&cur, platform, &ignore_login(login, ""), on);
        let bridge = self.bridge.clone();
        let login = login.to_string();
        cx.spawn(async move |this, cx| {
            let _ = bridge.settings_set(json!({"ignoredUsers": next})).await;
            let _ = this.update(cx, |p, cx| p.toast(if on { format!("Ignoring {login}") } else { format!("No longer ignoring {login}") }, false, cx));
        })
        .detach();
    }

    // ---- pinned messages ---------------------------------------------------------------------------------------------

    pub(super) fn hide_pin(&mut self, id: &str) {
        self.hidden_pins.retain(|x| x != id);
        self.hidden_pins.push(id.to_string());
        let keep = self.hidden_pins.len().saturating_sub(20);
        self.hidden_pins.drain(..keep);
        if let Ok(b) = serde_json::to_vec(&self.hidden_pins) {
            let _ = std::fs::write(hidden_pins_path(&self.svc), b);
        }
    }

    /// Drain what the shared services queued for this pane (image sizes, chatter styles); once per frame.
    pub(super) fn drain_events(&mut self, cx: &mut Context<Self>) {
        let loaded: Vec<String> = std::mem::take(&mut *self.images_inbox.borrow_mut());
        if !loaded.is_empty() {
            let set: HashSet<&str> = loaded.iter().map(|s| s.as_str()).collect();
            let waiting: Vec<usize> = self
                .shown
                .iter()
                .enumerate()
                .filter(|(_, s)| s.built.borrow().as_ref().and_then(|(_, b)| b.as_ref()).map(|b| b.urls().iter().any(|u| set.contains(u.as_str()))).unwrap_or(false))
                .map(|(i, _)| i)
                .collect();
            for i in waiting {
                if let Some((_, Some(b))) = self.shown[i].built.borrow().as_ref() {
                    for it in b_contents(b) {
                        it.invalidate();
                    }
                }
                self.list_state.invalidate(i);
            }
            // the open thread's lines lay out anew too
            for (_, _, b) in self.thread_built.borrow().values() {
                if b.urls().iter().any(|u| set.contains(u.as_str())) {
                    for it in b_contents(b) {
                        it.invalidate();
                    }
                }
            }
        }
        let previews: Vec<String> = std::mem::take(&mut *self.previews_inbox.borrow_mut());
        if !previews.is_empty() {
            // a card appeared under these rows: measured again (the built rows stay, the card is drawn from the cache)
            let set: HashSet<&str> = previews.iter().map(|s| s.as_str()).collect();
            let waiting: Vec<usize> = self
                .shown
                .iter()
                .enumerate()
                .filter(|(_, s)| s.built.borrow().as_ref().and_then(|(_, b)| b.as_ref()).and_then(|b| b.link().map(|l| set.contains(l.target.key.as_str()))).unwrap_or(false))
                .map(|(i, _)| i)
                .collect();
            for i in waiting {
                self.list_state.invalidate(i);
            }
        }
        let styled: Vec<String> = std::mem::take(&mut *self.styled_users.borrow_mut());
        if !styled.is_empty() {
            let set: HashSet<String> = styled.into_iter().collect();
            if set.contains(super::twitch::cosmetics::Cosmetics::DEFS_ARRIVED) {
                // the paint / badge definitions came after the rows were built: every row reads them anew
                self.rebuild_envs();
                self.gen += 1;
                self.invalidate_rows(|r| matches!(&r.body, RowBody::Msg(_) | RowBody::Kick(_) | RowBody::Automod(_)));
            } else {
                self.invalidate_rows(|r| match &r.body {
                    RowBody::Msg(m) => set.contains(&m.user.id),
                    RowBody::Automod(h) => set.contains(&h.user.id),
                    RowBody::Kick(m) => set.contains(&super::twitch::cosmetics::kick_key(&m.user.id)),
                    _ => false,
                });
            }
        }
        if self.needs_refresh {
            self.needs_refresh = false;
            self.refresh(cx);
        }
    }

    /// Height and collapsing margins of a row as the DOM lays it out (fractional, like Chromium's LayoutUnits:
    /// 1/64 px), from the flow layouts and the box styles `pane_render` puts around them.
    pub(super) fn row_metrics(&mut self, ix: usize, width: f32, window: &mut Window, cx: &mut Context<Self>) -> ItemMetrics {
        if ix >= self.shown.len() {
            return ItemMetrics::default();
        }
        let Some(b) = self.built(ix, cx) else { return ItemMetrics::default() };
        let base = self.settings.font_size;
        let fl = |v: f32| (v * 64.0).floor() / 64.0;
        let svc = self.svc.clone();
        let frame_h = |f: &FrameSpec, w: f32, window: &mut Window, cx: &mut gpui::App| -> f32 {
            let side = if f.bar.is_some() { 3.0 + 6.0 + 8.0 } else { 16.0 } + if f.border.is_some() { 2.0 } else { 0.0 };
            let flow = f.content.height_at(w - side, window, cx);
            let before = if f.before.is_some() { fl(base * 0.85 * LINE_HEIGHT) } else { 0.0 } + if f.label.is_some() { super::pane_render::LABEL_LINE } else { 0.0 };
            // the link preview card, once its preview is in (asking starts the request)
            let card = f.link.as_ref().map_or(0.0, |l| match svc.previews.get(&l.target, &svc.net, cx) {
                Some(p) => {
                    let lay = super::link_card::measure(&p, w - side, base, window);
                    l.layout.set(Some(lay));
                    lay.row_h()
                }
                None => {
                    l.layout.set(None);
                    0.0
                }
            });
            2.0 + before + flow + card + if f.border.is_some() { 2.0 } else { 0.0 }
        };
        match &*b {
            BuiltRow::Frame(f) => {
                let h = frame_h(f, width, window, cx);
                ItemMetrics { h, mt: f.margin_y, mb: f.margin_y }
            }
            BuiltRow::Event(e) => {
                // my-px, a 3px left bar; header `pt-0.5` + a 19.5 px line (+ pb-0.5 when nothing follows)
                let inner_w = width - 3.0;
                // (a picture on the right makes the header as tall as it, with pb-0.5 under it)
                // the header's text wraps in a narrow pane: measure it beside what sits on its right (icon, amount,
                // picture; 4 px apart) and the 8 px gap, inside px-2, so the next row never draws over it
                let mut right: Vec<f32> = Vec::new();
                if e.is_bits || e.amount.is_some() {
                    right.push(base);
                }
                if let Some(a) = e.amount {
                    right.push(super::inline::plain_text_width(window, &super::pane_render::thousands(a), base, gpui::FontWeight::SEMIBOLD));
                }
                if e.image.is_some() {
                    right.push(super::rows::event_image_px(base));
                }
                let right_w = right.iter().sum::<f32>() + 4.0 * right.len().saturating_sub(1) as f32;
                let text_w = (inner_w - 16.0 - 8.0 - right_w).max(1.0);
                let text_h = e.content.height_at(text_w, window, cx);
                let line = e.image.as_ref().map(|_| super::rows::event_image_px(base)).unwrap_or(0.0).max(19.5).max(text_h);
                let header = 2.0 + line + if e.inner.is_some() && e.image.is_none() { 0.0 } else { 2.0 };
                let inner = e.inner.as_ref().map(|f| frame_h(f, inner_w, window, cx)).unwrap_or(0.0);
                ItemMetrics { h: header + inner, mt: 1.0, mb: 1.0 }
            }
            BuiltRow::Notice(n) => {
                // 3px left bar, px-2, py-1, the icon column of a sub / gift
                let w = Self::notice_lines_w(width, n);
                let lines = n.head.as_ref().map(|h| h.height_at(w, window, cx)).unwrap_or(0.0)
                    + n.system.height_at(w, window, cx)
                    + n.text.as_ref().map(|t| t.height_at(w, window, cx)).unwrap_or(0.0)
                    + if n.reply.is_some() { super::pane_render::NOTICE_BUTTON_H + super::pane_render::NOTICE_BUTTON_GAP } else { 0.0 };
                let h = 8.0 + lines.max(n.icon.map(|(_, s)| s + 2.0).unwrap_or(0.0));
                // the attached message's chat line, inside the 3px bar under the sentence
                let line = n.line.as_ref().map(|f| frame_h(f, width - 3.0, window, cx)).unwrap_or(0.0);
                ItemMetrics { h: h + line, mt: 0.0, mb: 0.0 }
            }
            BuiltRow::Feed(f) => ItemMetrics { h: 2.0 + fl(f.base * 0.9 * LINE_HEIGHT), mt: 0.0, mb: 0.0 },
            BuiltRow::Automod(a) => {
                // py-1 + header line (the chip makes it 18.5) + body + the buttons / resolved line. The header is a
                // wrapping flex row (icon 14, the name flow, the chip): the chip drops to its own line, 6px below,
                // when it does not fit after the name.
                let w = width - 3.0 - 16.0;
                let name_w = (w - 14.0 - 6.0).max(1.0);
                let chip_w = super::inline::plain_text_width(window, &a.chip, 10.0, gpui::FontWeight::NORMAL) + 12.0 + 2.0;
                let fits = 14.0 + 6.0 + a.header.width_at(name_w, window, cx) + 6.0 + chip_w <= w;
                let head = if fits { a.header.height_at(name_w, window, cx).max(18.5) } else { a.header.height_at(name_w, window, cx).max(14.0) + 6.0 + 18.5 };
                // the held line is a row frame of its own (its px-2 inside the card's 3px bar)
                let body = frame_h(&a.msg, width - 3.0, window, cx);
                let tail = if a.status == "held" { if a.can_resolve { 4.0 + 19.95 } else { 0.0 } } else { 15.95 };
                ItemMetrics { h: 8.0 + head + body + tail, mt: 1.0, mb: 1.0 }
            }
        }
    }

    /// The built form of a shown row (cached per generation).
    pub(super) fn built(&mut self, ix: usize, cx: &mut gpui::App) -> Option<Rc<BuiltRow>> {
        {
            let s = &self.shown[ix];
            if let Some((g, b)) = s.built.borrow().as_ref() {
                if *g == self.gen {
                    return b.clone();
                }
            }
        }
        let row = self.shown[ix].row.clone();
        let b = self.build_row(&row, cx).map(Rc::new);
        *self.shown[ix].built.borrow_mut() = Some((self.gen, b.clone()));
        b
    }

    fn build_row(&mut self, row: &RowRef, cx: &mut gpui::App) -> Option<BuiltRow> {
        self.build_row_as(row, true, cx)
    }

    /// `mod_buttons`: false where the inline mod buttons must not show (the Mentions panel: their popovers open
    /// in the split, under the panel).
    fn build_row_as(&mut self, row: &RowRef, mod_buttons: bool, cx: &mut gpui::App) -> Option<BuiltRow> {
        let mod_list = self.mod_list.clone();
        let rule = self.rule_of(row);
        let rs = self.settings.clone();
        // The setting, and only in a split that mixes platforms: with one platform the mark says nothing. (A merged
        // co-chat used to force the marks on regardless of the setting, so switching it off did nothing.)
        let mixed = self.sources.iter().any(|s| s.platform != self.sources[0].platform);
        let show_platform = rs.show_platform_marks && mixed;
        let ctx = RowCtx { rs: &rs, show_platform, mod_buttons: mod_buttons && self.mod_mode && rs.show_mod_buttons, mod_list: &mod_list };
        let st = self.store.source(&row.sk);
        match &row.body {
            RowBody::Msg(m) => {
                let env = self.envs.get(&row.sk)?.clone();
                let st = st?;
                let lookup = if rs.seven_tv_cosmetics || rs.seven_tv { self.svc.cosmetics.style_of(&m.user.id, &self.svc.net, cx) } else { StyleLookup::Known(None) };
                let style: Option<ChatterStyle> = match lookup {
                    StyleLookup::Known(s) => s,
                    StyleLookup::Unknown => None,
                };
                let defs = env.seven_defs.clone();
                let susp = st.suspicious.get(&m.user.id).map(|s| s.status.clone());
                Some(twitch_message_row(&TwitchInput { row, msg: m, ctx: &ctx, env: &env, known: &st.known, susp: susp.as_deref(), rule: rule.as_ref(), style: style.as_ref(), defs: defs.as_deref(), held: false }))
            }
            RowBody::Notice(n) => {
                let seen = self.last_line_of(&row.sk, &n.user.id);
                let env = self.envs.get(&row.sk)?.clone();
                // A sub / streak / announcement with a message: the message is the sender's chat line (badges, a
                // name that opens the card, mod buttons, hover tools), not grey text.
                let is_redeem = (n.msg_id == "redeem" || n.msg_id == "powerup") && n.reward.is_some();
                let line = match st {
                    Some(st) if !n.text.is_empty() && n.seventv.is_none() && !is_redeem && !n.user.id.is_empty() => {
                        let msg = notice_chat_msg(n, seen.as_ref());
                        let lookup = if rs.seven_tv_cosmetics || rs.seven_tv { self.svc.cosmetics.style_of(&msg.user.id, &self.svc.net, cx) } else { StyleLookup::Known(None) };
                        let style: Option<ChatterStyle> = match lookup {
                            StyleLookup::Known(s) => s,
                            StyleLookup::Unknown => None,
                        };
                        let defs = env.seven_defs.clone();
                        let susp = st.suspicious.get(&msg.user.id).map(|s| s.status.clone());
                        match twitch_message_row(&TwitchInput { row, msg: &msg, ctx: &ctx, env: &env, known: &st.known, susp: susp.as_deref(), rule: rule.as_ref(), style: style.as_ref(), defs: defs.as_deref(), held: false }) {
                            BuiltRow::Frame(mut f) => {
                                // inside the notice's own tint
                                f.alt = false;
                                Some(f)
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                Some(notice_row(row, n, &ctx, &env, seen.as_ref(), line))
            }
            RowBody::ModFeed(f) => Some(feed_row(f, &ctx)),
            RowBody::Automod(h) => {
                // the held message drawn as the chat line it would be: badges and colour from the user's last line
                let env = self.envs.get(&row.sk)?.clone();
                let st = st?;
                let seen = self.last_line_of(&row.sk, &h.user.id);
                let known_color = st.known.get(&h.user.login.to_lowercase()).map(|k| k.color.as_str());
                let msg = held_chat_msg(h, seen.as_ref(), known_color);
                let lookup = if rs.seven_tv_cosmetics || rs.seven_tv { self.svc.cosmetics.style_of(&h.user.id, &self.svc.net, cx) } else { StyleLookup::Known(None) };
                let style: Option<ChatterStyle> = match lookup {
                    StyleLookup::Known(s) => s,
                    StyleLookup::Unknown => None,
                };
                let defs = env.seven_defs.clone();
                Some(automod_row(&TwitchInput { row, msg: &msg, ctx: &ctx, env: &env, known: &st.known, susp: None, rule: None, style: style.as_ref(), defs: defs.as_deref(), held: true }, h))
            }
            RowBody::Rumble(m) => {
                let st = st?;
                let env = CoEnv { source: st.source.clone(), emotes: None, caps: self.co_caps.get(&row.sk).cloned().unwrap_or_default(), can_send: st.conn.as_ref().and_then(|c| c.can_send).unwrap_or(false), rumble_badges: st.rumble_badges.clone(), rumble_emotes: st.rumble_emotes.clone(), viewer_login: self.co_login(Platform::Rumble), viewer_id: String::new(), me_names: Vec::new() };
                Some(rumble_row(row, m, &ctx, &env, rule.as_ref()))
            }
            RowBody::Kick(m) => {
                let st = st?;
                let env = CoEnv { source: st.source.clone(), emotes: self.kick_emotes.get(&row.sk).cloned(), caps: self.co_caps.get(&row.sk).cloned().unwrap_or_default(), can_send: st.conn.as_ref().and_then(|c| c.can_send).unwrap_or(false), rumble_badges: Default::default(), rumble_emotes: Default::default(), viewer_login: self.co_login(Platform::Kick), viewer_id: String::new(), me_names: self.kick_me() };
                let key = super::twitch::cosmetics::kick_key(&m.user.id);
                let lookup = if (rs.seven_tv_cosmetics || rs.seven_tv) && !m.user.anonymous { self.svc.cosmetics.style_of(&key, &self.svc.net, cx) } else { StyleLookup::Known(None) };
                let style: Option<ChatterStyle> = match lookup {
                    StyleLookup::Known(s) => s,
                    StyleLookup::Unknown => None,
                };
                let defs = self.svc.cosmetics.defs();
                Some(kick_row(row, m, &ctx, &env, rule.as_ref(), style.as_ref(), defs.as_deref()))
            }
            RowBody::YouTube(m) => {
                let st = st?;
                let yt = self.accounts.get(&Platform::Youtube);
                let connected = yt.map(|a| a.connected).unwrap_or(false);
                let env = CoEnv {
                    source: st.source.clone(),
                    emotes: None,
                    caps: CoCaps { connected, can_moderate: connected, can_delete: connected },
                    can_send: false,
                    rumble_badges: Default::default(),
                    rumble_emotes: Default::default(),
                    viewer_login: self.co_login(Platform::Youtube),
                    viewer_id: yt.and_then(|a| a.user.as_ref()).map(|u| u.id.clone()).unwrap_or_default(),
                    me_names: Vec::new(),
                };
                Some(youtube_row(row, m, &ctx, &env, rule.as_ref()))
            }
            RowBody::X(m) => Some(x_row(row, m, &ctx, rule.as_ref())),
        }
    }
}

fn b_contents(b: &BuiltRow) -> Vec<Rc<super::inline::InlineContent>> {
    b.contents()
}

/// A built row's flows numbered as the row draws them (the selection's `FlowKey::sub`).
pub(super) fn sel_flows(b: &BuiltRow) -> super::select::RowFlows {
    b.contents().into_iter().enumerate().map(|(i, c)| (i as u32, c)).collect()
}

#[cfg(test)]
mod unpin_tests {
    use super::unpin_secs;

    #[test]
    fn a_suspicious_frame_names_its_chatter() {
        use serde_json::json;
        assert_eq!(super::suspicious_user_of(&json!({"type": "suspicious", "user": {"id": "42"}, "msg": null})).as_deref(), Some("42"));
        assert_eq!(super::suspicious_user_of(&json!({"type": "suspicioususer", "user": null, "userId": "42"})).as_deref(), Some("42"));
        assert_eq!(super::suspicious_user_of(&json!({"type": "suspicious", "user": null})), None);
        assert_eq!(super::suspicious_user_of(&json!({"type": "chat", "userId": "42"})), None);
    }

    #[test]
    fn unpin_after_takes_30_seconds_to_30_minutes() {
        assert_eq!(unpin_secs("20", true), Some(1200));
        assert_eq!(unpin_secs("30", false), Some(30));
        assert_eq!(unpin_secs("29", false), None);
        assert_eq!(unpin_secs("30", true), Some(1800));
        assert_eq!(unpin_secs("31", true), None);
        assert_eq!(unpin_secs("0.5", true), Some(30));
        assert_eq!(unpin_secs("", true), None);
        assert_eq!(unpin_secs("abc", false), None);
    }
}
