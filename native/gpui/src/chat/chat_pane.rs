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
use gpui::{point, px, AppContext as _, Context, Entity, EventEmitter, Point, Pixels, Subscription, Window};
use super::chat_list::{ChatListState, ItemMetrics};
use serde_json::{json, Value};

use super::alerts::AlertWants;
use super::PxF;
use super::chat_text::text_mentions_login;
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
use super::twitch::emotes::{merge_emote_sets, EmoteSets, ProviderScopedMap};
use super::twitch::events::notice_visible;
use super::twitch::use_twitch_data::{load_badges, load_provider_sets, ProviderSets, TwitchData};
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
    pub(super) highlighter: Highlighter,
    pub(super) ignores: CompiledIgnores,
    ignore_cache: IgnoreCache,
    rule_cache: HashMap<usize, Option<HighlightRule>>,
    pub(super) mod_list: Vec<agu_chat_core::protocol::ModButton>,

    // data per source
    pub(super) twitch: HashMap<String, TwitchData>,
    pub(super) envs: HashMap<String, Rc<TwitchEnv>>,
    pub(super) co_caps: HashMap<String, CoCaps>,
    requested: HashSet<String>,

    // scrolling / pausing
    pub(super) scroll_held: bool,
    pub(super) hover_held: bool,
    pub(super) alt_held: bool,
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
    pub(super) inv_cache: RefCell<HashMap<String, (u64, Arc<super::input::inventory::Inventory>)>>,
    pub(super) hidden_pins: Vec<String>,
    pub(super) pin_open: HashSet<String>,
    /// (card key, showing the "Unpin After" sub-panel)
    pub(super) pin_menu: Option<(String, bool)>,
    pub(super) unpin_after: Option<UnpinAfter>,
    pub(super) confirm: Option<Confirm>,
    pub(super) images_inbox: Rc<RefCell<Vec<String>>>,
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
            twitch: HashMap::new(),
            envs: HashMap::new(),
            co_caps: HashMap::new(),
            requested: HashSet::new(),
            scroll_held: false,
            hover_held: false,
            alt_held: false,
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
            reply_target: None,
            send_box: None,
            mod_pop: None,
            pop_req: None,
            cards: Vec::new(),
            last_act: None,
            personal: Vec::new(),
            inv_cache: RefCell::new(HashMap::new()),
            hidden_pins,
            pin_open: HashSet::new(),
            pin_menu: None,
            unpin_after: None,
            confirm: None,
            images_inbox,
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
        pane
    }

    // ---- settings / accounts ---------------------------------------------------------------------------

    pub fn set_settings(&mut self, s: Settings, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.settings, s);
        let new = &self.settings;
        self.svc.images.0.set_animate(new.animated_emotes);
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
        cx.notify();
    }

    /// The logged-in account's login on Kick / Rumble ("" when logged out).
    pub(super) fn co_login(&self, platform: Platform) -> String {
        self.accounts.get(&platform).and_then(|a| a.user.as_ref()).map(|u| if u.login.is_empty() { u.name.clone() } else { u.login.clone() }).unwrap_or_default()
    }

    pub(super) fn me_login_lower(&self) -> String {
        self.accounts.get(&Platform::Twitch).and_then(|a| a.user.as_ref()).map(|u| u.login.to_lowercase()).unwrap_or_default()
    }

    fn me_id(&self) -> String {
        self.accounts.get(&Platform::Twitch).and_then(|a| a.user.as_ref()).map(|u| u.id.clone()).unwrap_or_default()
    }

    // ---- data loading (useTwitchData) -------------------------------------------------------------------

    fn start_loads(&mut self, cx: &mut Context<Self>) {
        let sks: Vec<(String, SourceRef, Option<String>)> = self.store.sources.iter().map(|s| (s.sk.clone(), s.source.clone(), s.channel_id.clone())).collect();
        let acct = format!("{}:{}", self.accounts.get(&Platform::Twitch).map(|a| a.connected).unwrap_or(false), self.me_id());
        self.load_personal(cx);
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
                                    if let Some(cid) = cid {
                                        p.store.set_channel_id(&sk2, &cid);
                                    }
                                    p.twitch.entry(sk2.clone()).or_default().badge_map = Arc::new(map);
                                    p.data_changed(cx);
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
                            });
                        })
                        .detach();
                    }
                    if !whisper && self.requested.insert(format!("pickeremotes:{sk}:{}", channel_id.clone().unwrap_or_default())) {
                        let (bridge, s2, sk2, cid) = (self.bridge.clone(), source.clone(), sk.clone(), channel_id.clone().unwrap_or_else(|| sk.clone()));
                        cx.spawn(async move |this, cx| {
                            let r = super::twitch::emotes::fetch_twitch_picker_emotes(&bridge, &s2, &cid).await;
                            let _ = this.update(cx, |p, cx| {
                                p.twitch.entry(sk2).or_default().twitch_emotes = r;
                                p.data_changed(cx);
                            });
                        })
                        .detach();
                    }
                    if let Some(cid) = channel_id {
                        if self.requested.insert(format!("emotes:{sk}:{cid}")) {
                            let sk2 = sk.clone();
                            let task = self.svc.net.run(load_provider_sets(self.svc.net.client.clone(), cid));
                            cx.spawn(async move |this, cx| {
                                if let Some(sets) = task.await {
                                    let _ = this.update(cx, |p, cx| {
                                        p.twitch.entry(sk2).or_default().provider_sets = Some(Arc::new(sets));
                                        p.data_changed(cx);
                                    });
                                }
                            })
                            .detach();
                        }
                    }
                }
                Platform::Rumble | Platform::Kick => {
                    let key = format!("cocaps:{sk}:{}{}", self.accounts.get(&Platform::Rumble).map(|a| a.connected as u8).unwrap_or(0), self.accounts.get(&Platform::Kick).map(|a| a.connected as u8).unwrap_or(0));
                    if self.requested.insert(key) {
                        let (bridge, s2, sk2) = (self.bridge.clone(), source.clone(), sk.clone());
                        cx.spawn(async move |this, cx| {
                            let caps = fetch_co_caps(&bridge, &s2).await;
                            let _ = this.update(cx, |p, cx| {
                                p.co_caps.insert(sk2, caps);
                                p.gen += 1;
                                p.invalidate_all();
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

    /// The acting account's 7TV personal set, loaded once per account.
    fn load_personal(&mut self, cx: &mut Context<Self>) {
        let id = self.me_id();
        if id.is_empty() || !self.requested.insert(format!("personal:{id}")) {
            return;
        }
        let task = self.svc.net.run({
            let client = self.svc.net.client.clone();
            async move { super::twitch::emotes::fetch_seven_tv_personal(&client, &id).await }
        });
        cx.spawn(async move |this, cx| {
            if let Some(list) = task.await {
                let _ = this.update(cx, |p, cx| {
                    p.personal = list;
                    p.data_changed(cx);
                });
            }
        })
        .detach();
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

    /// The send box view of each source: can it post, caps, and its emote inventory (cached per data generation).
    pub(super) fn send_sources(&self) -> Vec<super::input::send_box::SendSource> {
        use super::input::inventory::*;
        let rs = &self.settings;
        let mut out = Vec::new();
        for st in &self.store.sources {
            let conn = st.conn.as_ref();
            let inv = {
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
                                });
                                build_twitch_inventory(sets.as_ref(), &d.twitch_emotes, &self.personal)
                            }
                            Platform::Rumble => build_rumble_inventory(&st.rumble_emotes),
                            Platform::Kick => build_kick_inventory(&st.kick_emotes),
                            _ => Inventory::default(),
                        });
                        cache.insert(st.sk.clone(), (self.gen, i.clone()));
                        i
                    }
                }
            };
            out.push(super::input::send_box::SendSource {
                sk: st.sk.clone(),
                source: st.source.clone(),
                can_send: conn.and_then(|c| c.can_send).unwrap_or(false),
                read_only_reason: conn.and_then(|c| c.read_only_reason.clone()),
                pending: conn.map(|c| c.state == "connecting").unwrap_or(true),
                caps: self.twitch.get(&st.sk).map(|d| d.caps.clone()).unwrap_or_default(),
                inventory: inv,
            });
        }
        out
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
                }),
            );
        }
        self.envs = envs;
    }

    // ---- frames ------------------------------------------------------------------------------------------

    fn on_inbox(&mut self, batch: Vec<Inbox>, cx: &mut Context<Self>) {
        for m in batch {
            match m {
                Inbox::Reset(sk) => self.store.reset_source(&sk),
                Inbox::Frame(sk, f) => {
                    self.store.apply(&sk, &f);
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
        let rs = &self.settings;
        let rows = self.store.rows();
        let mut bombs: HashSet<String> = HashSet::new();
        for r in rows {
            if let RowBody::Notice(n) = &r.body {
                if let Some(g) = n.gift_id.as_ref().filter(|g| !g.is_empty()) {
                    let id = n.source_msg_id.clone().filter(|s| !s.is_empty()).unwrap_or(n.msg_id.clone());
                    if id.ends_with("submysterygift") {
                        bombs.insert(g.clone());
                    }
                }
            }
        }
        let visible: Vec<RowRef> = rows
            .iter()
            .filter(|r| match &r.body {
                RowBody::Notice(n) => notice_visible(&n.msg_id, n.source_msg_id.as_deref(), n.gift_id.as_deref(), &bombs),
                RowBody::ModFeed(_) => rs.show_mod_actions,
                RowBody::Automod(_) => rs.show_automod,
                RowBody::Msg(m) => !(m.deleted && rs.deleted_messages == "hide"),
                RowBody::Rumble(m) => !(m.deleted && rs.deleted_messages == "hide"),
                RowBody::Kick(m) => !(m.deleted && rs.deleted_messages == "hide"),
                _ => true,
            })
            .cloned()
            .collect();
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
        self.hover_held || self.alt_held
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
        let n_old = self.shown.len();
        let n_new = new.len();
        let reset = |this: &mut Self, new: Vec<RowRef>| {
            this.shown = new.into_iter().map(|row| ShownRow { row, built: RefCell::new(None) }).collect();
            this.list_state.reset(this.shown.len());
        };
        if n_old == 0 || n_new == 0 {
            reset(self, new);
            return;
        }
        let off = self.shown.iter().position(|o| o.row.key == new[0].key);
        let Some(off) = off else {
            reset(self, new);
            return;
        };
        let overlap = (n_old - off).min(n_new);
        for i in 0..overlap {
            if self.shown[off + i].row.key != new[i].key {
                reset(self, new);
                return;
            }
        }
        // the structure matches: trim at the front, replace changed rows, drop/append at the end
        let old = std::mem::take(&mut self.shown);
        let mut next: Vec<ShownRow> = Vec::with_capacity(n_new);
        let mut changed: Vec<usize> = Vec::new();
        let mut old_iter: Vec<Option<ShownRow>> = old.into_iter().map(Some).collect();
        for (i, row) in new.into_iter().enumerate() {
            if i < overlap {
                let o = old_iter[off + i].take().unwrap();
                if Arc::ptr_eq(&o.row, &row) {
                    next.push(o);
                } else {
                    changed.push(i);
                    next.push(ShownRow { row, built: RefCell::new(None) });
                }
            } else {
                next.push(ShownRow { row, built: RefCell::new(None) });
            }
        }
        self.shown = next;
        if off > 0 {
            self.list_state.splice(0..off, 0);
        }
        for i in changed {
            self.list_state.splice(i..i + 1, 1);
        }
        if n_old - off > overlap {
            self.list_state.splice(overlap..(n_old - off), 0);
        }
        if n_new > overlap {
            self.list_state.splice(overlap..overlap, n_new - overlap);
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
        let Some(me) = self.accounts.get(&r.platform).and_then(|a| a.user.as_ref()) else { return false };
        let (id, login) = match &r.body {
            RowBody::Msg(m) => (m.user.id.as_str(), m.user.login.as_str()),
            RowBody::Rumble(m) => (m.user.id.as_str(), m.user.login.as_str()),
            RowBody::Kick(m) => (m.user.id.as_str(), m.user.login.as_str()),
            RowBody::YouTube(m) => (m.user.id.as_str(), ""),
            RowBody::X(m) => (m.user.id.as_str(), m.user.login.as_str()),
            _ => return false,
        };
        (!me.id.is_empty() && id == me.id) || (!me.login.is_empty() && login.to_lowercase() == me.login.to_lowercase())
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
        let mut max_ord = self.seen_ord;
        let mut activity: Option<Activity> = None;
        let mut fire: Vec<(String, AlertWants)> = Vec::new();
        let bombs: HashSet<String> = HashSet::new();
        for r in &rows {
            if r.ord <= self.seen_ord {
                continue;
            }
            max_ord = max_ord.max(r.ord);
            if r.backlog || matches!(r.body, RowBody::ModFeed(_)) || self.is_own(r) || apply_ignores(r, &self.ignores).is_none() {
                continue; // ignored rows never ring
            }
            if let RowBody::Notice(n) = &r.body {
                if !notice_visible(&n.msg_id, n.source_msg_id.as_deref(), n.gift_id.as_deref(), &bombs) {
                    continue;
                }
            }
            activity.get_or_insert(Activity::Message);
            // A whisper is addressed to the user by definition.
            let mention = matches!(&r.body, RowBody::Msg(m) if is_whisper_key(&r.sk) || text_mentions_login(&m.text, &me_login));
            let rule = if mention { None } else { self.rule_of(r) };
            if !mention && rule.is_none() {
                continue;
            }
            activity = Some(Activity::Highlight);
            let key = format!("{}|{}", r.sk.to_lowercase(), &r.key[r.sk.len()..]);
            let wants = if mention {
                AlertWants { sound: self.settings.mention_sound, sound_file: String::new(), flash: self.settings.mention_flash }
            } else {
                let rule = rule.unwrap();
                AlertWants { sound: rule.sound, sound_file: rule.sound_file.clone(), flash: rule.flash }
            };
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
    }

    // ---- scrolling / pausing (MessageList) ------------------------------------------------------------------

    pub(super) fn last_ord(&self) -> u64 {
        self.shown.last().map(|s| s.row.ord).unwrap_or(0)
    }

    /// Called every frame: the user scrolled up (hold) or came back within 40 px of the bottom (resume).
    pub(super) fn track_scroll(&mut self) {
        let following = self.list_state.following();
        if !following && self.list_state.dist_to_bottom() <= 40.0 {
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
        if std::env::var("AGU_TIP").is_ok() {
            self.tip = Some((Rc::new(Tip { img: "https://static-cdn.jtvnw.net/emoticons/v2/25/default/dark/3.0".into(), name: "Kappa".into(), label: "Twitch".into() }), gpui::point(gpui::px(120.), gpui::px(200.))));
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

    /// Ctrl+click on a name: the message box starts with "@name " (kept once if it already does), caret at the
    /// end, box focused. The display name, unless it has characters chat would not match (a localized Twitch
    /// name), then the login.
    pub(super) fn mention_user(&mut self, user: &super::rows::CardUser, window: &mut Window, cx: &mut Context<Self>) {
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
        input.read(cx).focus(window);
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
            RowAction::Pin { sk, id } => {
                let Some(src) = self.ref_of(&sk) else { return };
                cx.spawn(async move |this, cx| {
                    let (_, t) = mod_actions::pin_message(&bridge, &src, &id).await;
                    let _ = this.update(cx, |p, cx| p.toast_msg(t, cx));
                })
                .detach();
            }
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
                None if user.id == me_id => ModReach { ban: false, ..REACH_ALL },
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
                    RowBody::Kick(m) => m.user.id == user.id,
                    RowBody::YouTube(m) => m.user.id == user.id,
                    RowBody::X(m) => m.user.id == user.id,
                    _ => false,
                }
        };
        let recent: Vec<RowRef> = self.store.rows().iter().rev().filter(|r| mine(r)).take(super::user_card::RECENT_MAX).cloned().collect();
        let recent_total = self.store.rows().iter().filter(|r| mine(r)).count();
        let (badges, sub) = recent.first().map(|r| super::user_card::badges_of(r, env.map(|e| &*e.badge_map), &st.rumble_badges)).unwrap_or_default();
        let login = ignore_login(&user.login, &user.name);
        Some(super::user_card::CardCtx {
            source: st.source.clone(),
            caps,
            reach,
            suspicious: st.suspicious.get(&user.id).cloned(),
            co_can_moderate: self.co_caps.get(sk).map(|c| c.can_moderate).unwrap_or(false),
            ignored: !login.is_empty() && super::ignores::is_user_ignored(&self.settings.ignored_users, platform, &login),
            can_ignore: !login.is_empty(),
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
        }
        let styled: Vec<String> = std::mem::take(&mut *self.styled_users.borrow_mut());
        if !styled.is_empty() {
            let set: HashSet<String> = styled.into_iter().collect();
            if set.contains(super::twitch::cosmetics::Cosmetics::DEFS_ARRIVED) {
                // the paint / badge definitions came after the rows were built: every row reads them anew
                self.rebuild_envs();
                self.gen += 1;
                self.invalidate_rows(|r| matches!(&r.body, RowBody::Msg(_)));
            } else {
                self.invalidate_rows(|r| matches!(&r.body, RowBody::Msg(m) if set.contains(&m.user.id)));
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
        let frame_h = |f: &FrameSpec, w: f32, window: &mut Window, cx: &mut gpui::App| -> f32 {
            let side = if f.bar.is_some() { 3.0 + 6.0 + 8.0 } else { 16.0 } + if f.border.is_some() { 2.0 } else { 0.0 };
            let flow = f.content.height_at(w - side, window, cx);
            let before = if f.before.is_some() { fl(base * 0.85 * LINE_HEIGHT) } else { 0.0 } + if f.label.is_some() { super::pane_render::LABEL_LINE } else { 0.0 };
            2.0 + before + flow + if f.border.is_some() { 2.0 } else { 0.0 }
        };
        match &*b {
            BuiltRow::Frame(f) => {
                let h = frame_h(f, width, window, cx);
                ItemMetrics { h, mt: f.margin_y, mb: f.margin_y }
            }
            BuiltRow::Event(e) => {
                // my-px mx-1 border; header `pt-0.5` + a 19.5 px line (+ pb-0.5 when nothing follows)
                let inner_w = width - 10.0;
                let header = 2.0 + 19.5 + if e.inner.is_some() { 0.0 } else { 2.0 };
                let inner = e.inner.as_ref().map(|f| frame_h(f, inner_w, window, cx)).unwrap_or(0.0);
                ItemMetrics { h: 2.0 + header + inner, mt: 1.0, mb: 1.0 }
            }
            BuiltRow::Notice(n) => {
                let w = width - 2.0 - 16.0;
                let h = 4.0 + n.system.height_at(w, window, cx) + n.text.as_ref().map(|t| t.height_at(w, window, cx)).unwrap_or(0.0);
                ItemMetrics { h, mt: 0.0, mb: 0.0 }
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
                let body = a.body.height_at(w, window, cx);
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
        let mod_list = self.mod_list.clone();
        let rule = self.rule_of(row);
        let rs = self.settings.clone();
        // The setting, and only in a split that mixes platforms: with one platform the mark says nothing. (A merged
        // co-chat used to force the marks on regardless of the setting, so switching it off did nothing.)
        let mixed = self.sources.iter().any(|s| s.platform != self.sources[0].platform);
        let show_platform = rs.show_platform_marks && mixed;
        let ctx = RowCtx { rs: &rs, show_platform, mod_buttons: self.mod_mode && rs.show_mod_buttons, mod_list: &mod_list };
        let st = self.store.source(&row.sk);
        match &row.body {
            RowBody::Msg(m) => {
                let env = self.envs.get(&row.sk)?.clone();
                let st = st?;
                let lookup = if rs.seven_tv_cosmetics { self.svc.cosmetics.style_of(&m.user.id, &self.svc.net, cx) } else { StyleLookup::Known(None) };
                let style: Option<ChatterStyle> = match lookup {
                    StyleLookup::Known(s) => s,
                    StyleLookup::Unknown => None,
                };
                let defs = env.seven_defs.clone();
                let susp = st.suspicious.get(&m.user.id).map(|s| s.status.clone());
                Some(twitch_message_row(&TwitchInput { row, msg: m, ctx: &ctx, env: &env, known: &st.known, susp: susp.as_deref(), rule: rule.as_ref(), style: style.as_ref(), defs: defs.as_deref() }))
            }
            RowBody::Notice(n) => Some(notice_row(row, n, &ctx, self.envs.get(&row.sk)?)),
            RowBody::ModFeed(f) => Some(feed_row(f, &ctx)),
            RowBody::Automod(h) => Some(automod_row(row, h, &ctx, self.envs.get(&row.sk)?)),
            RowBody::Rumble(m) => {
                let st = st?;
                let env = CoEnv { source: st.source.clone(), caps: self.co_caps.get(&row.sk).cloned().unwrap_or_default(), can_send: st.conn.as_ref().and_then(|c| c.can_send).unwrap_or(false), rumble_badges: st.rumble_badges.clone(), rumble_emotes: st.rumble_emotes.clone(), viewer_login: self.co_login(Platform::Rumble) };
                Some(rumble_row(row, m, &ctx, &env, rule.as_ref()))
            }
            RowBody::Kick(m) => {
                let st = st?;
                let env = CoEnv { source: st.source.clone(), caps: self.co_caps.get(&row.sk).cloned().unwrap_or_default(), can_send: st.conn.as_ref().and_then(|c| c.can_send).unwrap_or(false), rumble_badges: Default::default(), rumble_emotes: Default::default(), viewer_login: self.co_login(Platform::Kick) };
                Some(kick_row(row, m, &ctx, &env, rule.as_ref()))
            }
            RowBody::YouTube(m) => Some(youtube_row(row, m, &ctx, rule.as_ref())),
            RowBody::X(m) => Some(x_row(row, m, &ctx, rule.as_ref())),
        }
    }
}

fn b_contents(b: &BuiltRow) -> Vec<Rc<super::inline::InlineContent>> {
    match b {
        BuiltRow::Frame(f) => vec![f.content.clone()],
        BuiltRow::Event(e) => {
            let mut v = vec![e.content.clone()];
            if let Some(i) = &e.inner {
                v.push(i.content.clone());
            }
            v
        }
        BuiltRow::Notice(n) => {
            let mut v = vec![n.system.clone()];
            if let Some(t) = &n.text {
                v.push(t.clone());
            }
            v
        }
        BuiltRow::Automod(a) => vec![a.header.clone(), a.body.clone()],
        BuiltRow::Feed(_) => vec![],
    }
}

#[cfg(test)]
mod unpin_tests {
    use super::unpin_secs;

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
