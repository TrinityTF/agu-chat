//! Port of src/renderer/layout/App.tsx.
//! Agu Chat window: tab bar, one SplitView per tab, update bar, dialogs, persistence.
//!
//! React's hooks/state become fields of `AguApp`; every handler is a method that takes ids, like the
//! JS ones, so they stay stable. The tab bar (`notebook_tabs.rs`) and split view/header/leaf
//! (`split_view.rs`, `split_header.rs`) are further `impl AguApp` blocks.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use agu_chat_core::protocol::{AguBridge, Layout, Platform, Settings, SourceRef, SplitNode, Tab, Unsubscribe, UpdaterState};
use agu_chat_core::twitch::whisper;
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::StreamExt;
use gpui::{
    div, prelude::*, px, relative, AnyElement, App, Bounds, ClickEvent, Context, Entity, FocusHandle, Focusable, HighlightStyle, KeyDownEvent, Keystroke,
    MouseMoveEvent, MouseUpEvent, Pixels, Render, SharedString, StyledText, Subscription, Task, Window,
};

use crate::layout::channel_dialog::{ChannelDialog, DialogEvent};
use crate::layout::settings_dialog::{Section, SettingsDialog};
use crate::chat::chat_pane::{Activity, ChatPane, PaneEvent};
use crate::layout::hotkeys::{
    binding_map, event_to_binding, resolve_hotkeys, HotkeyAction, HotkeyTable, KeyLike, HOTKEY_RECORDING,
};
use crate::layout::notebook_tabs::TabsState;
use crate::layout::split_view::SplitViewState;
use crate::layout::window_fit;
use crate::layout::tree::{
    self, default_layout, find_leaf, insert_at, leaves, map_leaf, move_leaf, new_leaf, new_tab, remove_leaf, sanitize_layout, set_sizes, source_key, tab_title, Edge,
};
use crate::layout::ui::{btn, modal, popup_menu, BtnOpts, CloseFn, MenuState};
use crate::theme::{self, c};
use crate::window_state;

// ---- drag payloads (what HTML5 dataTransfer carried) ------------------------------------------

/// A whole split being moved (its header): "application/x-agu-split".
#[derive(Clone)]
pub struct SplitDrag {
    pub leaf_id: String,
    pub label: SharedString,
}

/// Some of a split's channels (a header chip): "application/x-agu-sources".
#[derive(Clone)]
pub struct SourcesDrag {
    pub leaf_id: String,
    pub keys: Vec<String>,
    pub label: SharedString,
}

/// A tab dragged in the tab bar: "application/x-agu-tab".
#[derive(Clone)]
/// What a drop would put at a split's edge, for the room check.
pub enum Adding<'a> {
    /// an existing split moves there
    Move(&'a str),
    /// a new split
    New,
    /// another tab's whole layout
    Tab(&'a str),
}

pub struct TabDrag {
    pub tab_id: String,
    pub label: SharedString,
}

/// The little box that follows the cursor during a drag.
pub struct DragGhost(pub SharedString);

impl Render for DragGhost {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .font_family(theme::FONT)
            .text_size(px(12.))
            .line_height(px(18.))
            .px(px(10.))
            .py(px(2.))
            .rounded(px(4.))
            .border_1()
            .border_color(c(theme::ACCENT))
            .bg(theme::a(theme::RAISED, 0.9))
            .text_color(c(theme::TEXT))
            .child(self.0.clone())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Dialog {
    Add { tab_id: String },
    /// a whisper conversation in this tab (the empty tab's Whisper button)
    Whisper { tab_id: String },
    Change { tab_id: String, leaf_id: String },
    Split { tab_id: String, leaf_id: String, below: bool },
    Settings,
    CloseTab { tab_id: String, others: bool },
}

/// A tab with at least one channel in it: closing it loses something, so it asks first.
fn has_channels(t: Option<&Tab>) -> bool {
    t.map(|t| t.root.is_some() && leaves(t.root.as_ref()).iter().any(|l| !l.sources.is_empty())).unwrap_or(false)
}

pub struct PaneSlot {
    pub sig: String,
    pub view: Entity<ChatPane>,
    _sub: Subscription,
}

/// Runs the backend's unsubscribe when dropped.
pub(crate) struct Unsub(Option<Unsubscribe>);
impl Drop for Unsub {
    fn drop(&mut self) {
        if let Some(u) = self.0.take() {
            u();
        }
    }
}

/// Bridge callbacks fire on backend threads: forward them through a channel the UI task reads.
pub(crate) fn stream<T: Clone + Send + 'static>(register: impl FnOnce(Arc<dyn Fn(&T) + Send + Sync>) -> Unsubscribe) -> (UnboundedReceiver<T>, Unsub) {
    let (tx, rx) = unbounded::<T>();
    let unsub = register(Arc::new(move |v: &T| {
        let _ = tx.unbounded_send(v.clone());
    }));
    (rx, Unsub(Some(unsub)))
}

/// What `fit_window` last saw of the active tab: its tree and the content size (px) that tree was drawn at, so a
/// structure change can keep every chat's pixel size.
struct FitPrev {
    tab_id: String,
    shape: String,
    root: SplitNode,
    w: f32,
    h: f32,
    chrome: i32,
    /// the layout's minimum content size, rounded up
    min: (i32, i32),
    /// when a window resize was ordered, until the host has taken the new size
    pending: Option<Instant>,
}

pub struct AguApp {
    pub bridge: Arc<dyn AguBridge>,
    pub layout: Layout,
    pub focus_by_tab: HashMap<String, String>,
    pub dialog: Option<Dialog>,
    pub dialog_view: Option<(Entity<ChannelDialog>, Subscription)>,
    pub settings_view: Option<(Entity<SettingsDialog>, Subscription)>,
    /// a tab being dragged from the tab bar
    pub tab_drag: Option<String>,
    /// Tabs that got live chat while not selected.
    pub unread: HashMap<String, Activity>,
    pub updater: UpdaterState,
    pub bar_dismissed: Option<String>,
    pub settings: Settings,
    pub hotkey_table: HotkeyTable,
    pub keymap: HashMap<String, HotkeyAction>,
    pub panes: HashMap<String, PaneSlot>,
    /// the Modes button of each split's header, rebuilt when its channels change
    pub modes: HashMap<String, (String, Entity<crate::layout::channel_modes::ModesButton>)>,
    pub tabs: TabsState,
    pub sv: SplitViewState,
    pub menu: Option<MenuState>,
    /// Bounds of elements menus anchor to / drags measure against, recorded while painting.
    pub anchors: Rc<RefCell<HashMap<String, Bounds<Pixels>>>>,
    pub host_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub focus: FocusHandle,
    save_task: Option<Task<()>>,
    win_task: Option<Task<()>>,
    /// the side a split was just added on (consumed by `fit_window`, which grows the window toward it)
    fit_edge: Option<Edge>,
    /// what `fit_window` last saw of the active tab
    fit_prev: Option<FitPrev>,
    fit_hooked: bool,
    /// the biggest content area (logical px) the screen leaves for the splits
    fit_max: Option<(f32, f32)>,
    /// the "Always on top" state last applied to the native window
    on_top_applied: Option<bool>,
    _subs: Vec<Subscription>,
    _unsubs: Vec<Unsub>,
}

impl AguApp {
    pub fn new(bridge: Arc<dyn AguBridge>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let layout = bridge.layout_get().map(|l| sanitize_layout(&serde_json::to_value(&l).unwrap_or_default())).unwrap_or_else(default_layout);
        let settings = bridge.settings_get();
        let hotkey_table = resolve_hotkeys(&serde_json::to_value(&settings.hotkeys).unwrap_or_default());
        let keymap = binding_map(&hotkey_table);
        let updater = bridge.updater_get();

        let mut subs = Vec::new();
        // Closing the window flushes the pending layout write (the JS did it on beforeunload/pagehide).
        {
            let this = cx.entity();
            window.on_window_should_close(cx, move |_w, cx| {
                this.update(cx, |t, _| t.flush_layout());
                true
            });
        }
        // window.json: remember position, size and maximized state.
        subs.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.schedule_window_save(window, cx);
            cx.notify();
        }));

        let mut app = AguApp {
            bridge: bridge.clone(),
            layout,
            focus_by_tab: HashMap::new(),
            dialog: None,
            dialog_view: None,
            settings_view: None,
            tab_drag: None,
            unread: HashMap::new(),
            updater,
            bar_dismissed: None,
            settings,
            hotkey_table,
            keymap,
            panes: HashMap::new(),
            modes: HashMap::new(),
            tabs: TabsState::default(),
            sv: SplitViewState::default(),
            menu: None,
            anchors: Rc::new(RefCell::new(HashMap::new())),
            host_bounds: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
            save_task: None,
            win_task: None,
            fit_edge: None,
            fit_prev: None,
            fit_hooked: false,
            fit_max: None,
            on_top_applied: None,
            _subs: subs,
            _unsubs: Vec::new(),
        };
        app.sync_panes(cx);
        app.listen_for_whispers(cx);

        // settings / updater changes from the backend
        let (mut rx, u) = stream::<Settings>(|cb| bridge.on_settings(cb));
        app._unsubs.push(u);
        cx.spawn(async move |this, cx| {
            while let Some(s) = rx.next().await {
                if this.update(cx, |this, cx| this.set_settings(s, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let (mut rx, u) = stream::<UpdaterState>(|cb| bridge.on_updater(cb));
        app._unsubs.push(u);
        cx.spawn(async move |this, cx| {
            while let Some(s) = rx.next().await {
                if this.update(cx, |this, cx| {
                    this.updater = s;
                    cx.notify();
                })
                .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // alerts: whether the window is active, and the handle to flash
        {
            use raw_window_handle::HasWindowHandle;
            let svc = cx.global::<crate::chat::services::Services>().clone();
            if let Ok(h) = HasWindowHandle::window_handle(&*window) {
                if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                    svc.hwnd.set(w.hwnd.get());
                }
            }
            svc.window_active.set(window.is_window_active());
            let svc2 = svc.clone();
            app._subs.push(cx.observe_window_activation(window, move |_this, window, _cx| svc2.window_active.set(window.is_window_active())));
        }
        window.focus(&app.focus);
        app.demo_state(window, cx);
        app
    }

    /// Dev aid for screenshots: `AGU_SHOT=dialog|menu|confirm|settings` opens that UI state at startup.
    fn demo_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(shot) = std::env::var("AGU_SHOT") else { return };
        let tab = self.active_tab().clone();
        let leaf = leaves(tab.root.as_ref()).first().map(|l| l.id.clone()).unwrap_or_default();
        match shot.as_str() {
            "dialog" => self.open_dialog(Dialog::Split { tab_id: tab.id, leaf_id: leaf, below: false }, window, cx),
            "change" => self.open_dialog(Dialog::Change { tab_id: tab.id, leaf_id: leaf }, window, cx),
            "confirm" => self.open_dialog(Dialog::CloseTab { tab_id: tab.id, others: false }, window, cx),
            s if s == "settings" || s.starts_with("settings:") => self.open_dialog(Dialog::Settings, window, cx),
            "menu" => {
                let entries = self.split_menu_entries(&tab.id, &leaf);
                self.menu = Some(MenuState { key: "demo".into(), pos: gpui::point(px(560.), px(54.)), align_right: true, entries, anchor: None });
            }
            _ => {}
        }
    }

    fn set_settings(&mut self, s: Settings, cx: &mut Context<Self>) {
        self.hotkey_table = resolve_hotkeys(&serde_json::to_value(&s.hotkeys).unwrap_or_default());
        self.keymap = binding_map(&self.hotkey_table);
        self.settings = s;
        for slot in self.panes.values() {
            let s = self.settings.clone();
            slot.view.update(cx, |p, cx| p.set_settings(s, cx));
        }
        cx.notify();
    }

    // ---- derived -----------------------------------------------------------------

    pub fn active_tab(&self) -> &Tab {
        self.layout.tabs.iter().find(|t| t.id == self.layout.active).unwrap_or(&self.layout.tabs[0])
    }

    pub fn focused_of(&self, tab: &Tab) -> Option<String> {
        let ls = leaves(tab.root.as_ref());
        let want = self.focus_by_tab.get(&tab.id);
        ls.iter().find(|l| Some(&l.id) == want).or_else(|| ls.first()).map(|l| l.id.clone())
    }

    // ---- persistence ----------------------------------------------------------------

    /// `setLayout`: change the layout, then sync panes / markers and schedule the debounced save.
    pub fn mutate(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Layout)) {
        f(&mut self.layout);
        // activeId changed -> its unread marker clears
        let active = self.active_tab().id.clone();
        self.unread.remove(&active);
        self.sync_panes(cx);
        self.schedule_layout_save(cx);
        cx.notify();
    }

    fn schedule_layout_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let _ = this.update(cx, |this, _| this.flush_layout());
        }));
    }

    /// Saves now when a save is pending.
    pub fn flush_layout(&mut self) {
        if self.save_task.take().is_some() {
            self.bridge.layout_set(self.layout.clone());
        }
    }

    fn schedule_window_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (b, maximized) = match window.window_bounds() {
            gpui::WindowBounds::Windowed(b) => (b, false),
            gpui::WindowBounds::Maximized(b) => (b, true),
            gpui::WindowBounds::Fullscreen(b) => (b, false),
        };
        let st = window_state::WindowState {
            x: b.origin.x.into(),
            y: b.origin.y.into(),
            width: b.size.width.into(),
            height: b.size.height.into(),
            maximized,
        };
        self.win_task = Some(cx.spawn(async move |_this, cx| {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            window_state::save(&st);
        }));
    }

    /// Chat panes live in `panes`, keyed by split id, so a pane (and its scrollback) survives its tab being
    /// hidden or its split moving; it is rebuilt only when the split's channels change (`key={sig}` in the JS).
    fn sync_panes(&mut self, cx: &mut Context<Self>) {
        let mut keep: HashSet<String> = HashSet::new();
        let all: Vec<tree::Leaf> = self.layout.tabs.iter().flat_map(|t| leaves(t.root.as_ref())).collect();
        for leaf in all {
            keep.insert(leaf.id.clone());
            let sig = leaf.sources.iter().map(source_key).collect::<Vec<_>>().join("|");
            let stale = self.panes.get(&leaf.id).map(|s| s.sig != sig).unwrap_or(true);
            if stale {
                let bridge = self.bridge.clone();
                let sources = leaf.sources.clone();
                let mod_mode = leaf.mod_mode;
                let settings = self.settings.clone();
                let view = cx.new(|cx| ChatPane::new(sources, mod_mode, bridge, settings, cx));
                let leaf_id = leaf.id.clone();
                let sub = cx.subscribe(&view, move |this, _p, ev: &PaneEvent, cx| match ev {
                    PaneEvent::Status => cx.notify(),
                    PaneEvent::Activity(kind) => this.on_activity(&leaf_id, *kind, cx),
                });
                self.panes.insert(leaf.id.clone(), PaneSlot { sig, view, _sub: sub });
            } else if let Some(slot) = self.panes.get(&leaf.id) {
                slot.view.update(cx, |p, cx| p.set_mod_mode(leaf.mod_mode, cx));
            }
        }
        self.panes.retain(|id, _| keep.contains(id));
    }

    /// The whisper inbox (core's `whisper:*` source) says who whispered; a conversation with no tab yet gets one.
    fn listen_for_whispers(&mut self, cx: &mut Context<Self>) {
        let bridge = self.bridge.clone();
        let inbox_sub: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let (tx, mut rx) = unbounded::<String>();
        let ids = inbox_sub.clone();
        let unsub = bridge.on_frame(Arc::new(move |sub, frame| {
            if ids.lock().unwrap().as_deref() == Some(sub.as_str()) && frame["type"] == "whisper" {
                if let Some(login) = frame["login"].as_str() {
                    let _ = tx.unbounded_send(login.to_string());
                }
            }
        }));
        self._unsubs.push(Unsub(Some(unsub)));
        cx.spawn(async move |this, cx| {
            let source = SourceRef { platform: Platform::Twitch, channel: whisper::inbox_channel().into() };
            let Ok(id) = bridge.subscribe(source).await else { return };
            *inbox_sub.lock().unwrap() = Some(id);
            while let Some(login) = rx.next().await {
                if this.update(cx, |this, cx| this.on_whisper(&login, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// A whisper arrived from `login`. Without a tab for the conversation, one opens in the background, marked
    /// like any tab with news: it never takes the selection or the focus.
    fn on_whisper(&mut self, login: &str, cx: &mut Context<Self>) {
        // an ignored user's whispers are kept out of sight like their chat: no tab pops up for them
        if crate::chat::ignores::is_user_ignored(&self.settings.ignored_users, Platform::Twitch, login) {
            return;
        }
        let channel = whisper::channel_for(login);
        let open = self.layout.tabs.iter().any(|t| leaves(t.root.as_ref()).iter().any(|l| l.sources.iter().any(|s| s.platform == Platform::Twitch && s.channel == channel)));
        if open {
            return;
        }
        let tab = new_tab(Some(vec![SourceRef { platform: Platform::Twitch, channel }]));
        let id = tab.id.clone();
        self.mutate(cx, |l| l.tabs.push(tab));
        self.unread.insert(id, Activity::Highlight);
        cx.notify();
    }

    /// `activityFor(tabId)`
    fn on_activity(&mut self, leaf_id: &str, kind: Activity, cx: &mut Context<Self>) {
        let Some(tab) = self.layout.tabs.iter().find(|t| leaves(t.root.as_ref()).iter().any(|l| l.id == leaf_id)) else { return };
        if tab.id == self.layout.active {
            return;
        }
        let id = tab.id.clone();
        if self.unread.get(&id) == Some(&kind) || self.unread.get(&id) == Some(&Activity::Highlight) {
            return;
        }
        self.unread.insert(id, kind);
        cx.notify();
    }

    // ---- tab / split operations (all take ids, so they stay stable) ----------------------------

    pub fn update_tab(&mut self, cx: &mut Context<Self>, tab_id: &str, f: impl FnOnce(&mut Tab)) {
        self.mutate(cx, |l| {
            if let Some(t) = l.tabs.iter_mut().find(|t| t.id == tab_id) {
                f(t);
            }
        });
    }

    pub fn update_root(&mut self, cx: &mut Context<Self>, tab_id: &str, f: impl FnOnce(Option<&SplitNode>) -> Option<SplitNode>) {
        self.update_tab(cx, tab_id, |t| t.root = f(t.root.as_ref()));
    }

    pub fn focus_leaf(&mut self, tab_id: &str, leaf_id: &str, cx: &mut Context<Self>) {
        if self.focus_by_tab.get(tab_id).map(|s| s.as_str()) != Some(leaf_id) {
            self.focus_by_tab.insert(tab_id.to_string(), leaf_id.to_string());
            cx.notify();
        }
    }

    pub fn close_tab_now(&mut self, id: &str, cx: &mut Context<Self>) {
        self.mutate(cx, |l| {
            let Some(idx) = l.tabs.iter().position(|t| t.id == id) else { return };
            l.tabs.remove(idx);
            if l.tabs.is_empty() {
                let t = new_tab(None);
                l.active = t.id.clone();
                l.tabs.push(t);
                return;
            }
            if l.active == id {
                l.active = l.tabs[idx.min(l.tabs.len() - 1)].id.clone();
            }
        });
    }

    pub fn close_others_now(&mut self, id: &str, cx: &mut Context<Self>) {
        self.mutate(cx, |l| {
            l.tabs.retain(|t| t.id == id);
            l.active = id.to_string();
        });
    }

    pub fn select_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.layout.tabs.iter().any(|t| t.id == id) && self.layout.active != id {
            self.mutate(cx, |l| l.active = id.to_string());
        }
    }

    pub fn add_tab(&mut self, cx: &mut Context<Self>) {
        self.mutate(cx, |l| {
            let t = new_tab(None);
            l.active = t.id.clone();
            l.tabs.push(t);
        });
    }

    pub fn close_tab(&mut self, id: &str, cx: &mut Context<Self>) {
        if has_channels(self.layout.tabs.iter().find(|t| t.id == id)) {
            self.dialog = Some(Dialog::CloseTab { tab_id: id.to_string(), others: false });
            cx.notify();
        } else {
            self.close_tab_now(id, cx);
        }
    }

    pub fn close_others(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.layout.tabs.iter().any(|t| t.id != id && has_channels(Some(t))) {
            self.dialog = Some(Dialog::CloseTab { tab_id: id.to_string(), others: true });
            cx.notify();
        } else {
            self.close_others_now(id, cx);
        }
    }

    pub fn rename_tab(&mut self, id: &str, title: String, cx: &mut Context<Self>) {
        self.update_tab(cx, id, |t| t.title = title);
    }

    /// A split (or some of its channels) dragged up to the tab bar becomes a tab of its own. A tab left with
    /// nothing in it goes away, so dragging a tab's only split up just moves it.
    pub fn from_split(&mut self, leaf_id: &str, keys: Option<&[String]>, before: usize, cx: &mut Context<Self>) {
        self.mutate(cx, |l| {
            let Some(src_idx) = l.tabs.iter().position(|t| t.id == l.active) else { return };
            let Some(from) = find_leaf(l.tabs[src_idx].root.as_ref(), leaf_id) else { return };
            let moving: Vec<SourceRef> = match keys {
                Some(k) => from.sources.iter().filter(|s| k.contains(&source_key(s))).cloned().collect(),
                None => from.sources.clone(),
            };
            if moving.is_empty() {
                return;
            }
            let staying: Vec<SourceRef> = from.sources.iter().filter(|s| !moving.contains(s)).cloned().collect();
            let rest = if !staying.is_empty() {
                map_leaf(l.tabs[src_idx].root.as_ref(), leaf_id, &|x| tree::Leaf { sources: staying.clone(), ..x })
            } else {
                remove_leaf(l.tabs[src_idx].root.as_ref(), leaf_id)
            };
            let mut t = new_tab(None);
            t.root = Some(tree::Leaf { mod_mode: from.mod_mode, ..new_leaf(moving) }.into());
            l.tabs[src_idx].root = rest.clone();
            let mut at = before.min(l.tabs.len());
            if rest.is_none() {
                l.tabs.remove(src_idx);
                if src_idx < at {
                    at -= 1;
                }
            }
            l.active = t.id.clone();
            l.tabs.insert(at.min(l.tabs.len()), t);
        });
    }

    /// Move tab `id` to sit before the tab currently at `before` (tabs.len = the end).
    pub fn reorder(&mut self, id: &str, before: usize, cx: &mut Context<Self>) {
        self.mutate(cx, |l| {
            let Some(from) = l.tabs.iter().position(|t| t.id == id) else { return };
            let to = if before > from { before - 1 } else { before };
            if to == from {
                return;
            }
            let t = l.tabs.remove(from);
            l.tabs.insert(to, t);
        });
    }

    pub fn toggle_mod(&mut self, tab_id: &str, leaf_id: &str, cx: &mut Context<Self>) {
        self.update_root(cx, tab_id, |r| map_leaf(r, leaf_id, &|l| tree::Leaf { mod_mode: !l.mod_mode, ..l }));
    }

    pub fn close_leaf(&mut self, tab_id: &str, leaf_id: &str, cx: &mut Context<Self>) {
        self.update_root(cx, tab_id, |r| remove_leaf(r, leaf_id));
    }

    pub fn resize(&mut self, tab_id: &str, container_id: &str, sizes: &[f32], cx: &mut Context<Self>) {
        self.update_root(cx, tab_id, |r| set_sizes(r, container_id, sizes));
    }

    /// Whether `tab_id`'s layout still fits on the screen with `adding` put at `edge` of `target`: every chat keeps
    /// its minimum size inside the biggest window the screen allows.
    pub fn edge_fits(&self, tab_id: &str, target: &str, edge: Edge, adding: Adding) -> bool {
        let Some((mw, mh)) = self.fit_max else { return true };
        let Some(tab) = self.layout.tabs.iter().find(|t| t.id == tab_id) else { return true };
        let root = tab.root.as_ref();
        let cand = match adding {
            Adding::Move(src) => move_leaf(root, src, target, edge),
            Adding::New => insert_at(root, target, new_leaf(vec![]).into(), edge),
            Adding::Tab(t) => match self.layout.tabs.iter().find(|x| x.id == t).and_then(|x| x.root.clone()) {
                Some(add) => insert_at(root, target, add, edge),
                None => return true,
            },
        };
        let Some(c) = cand else { return true };
        let (w, h) = window_fit::tree_min_size(&c);
        w <= mw + 0.5 && h <= mh + 0.5
    }

    /// `want` at `target`, or the other direction when only that one fits (right <-> below).
    fn roomy_edge(&self, tab_id: &str, target: &str, want: Edge) -> Edge {
        let other = if matches!(want, Edge::Left | Edge::Right) { Edge::Bottom } else { Edge::Right };
        if !self.edge_fits(tab_id, target, want, Adding::New) && self.edge_fits(tab_id, target, other, Adding::New) {
            other
        } else {
            want
        }
    }

    pub fn move_split(&mut self, tab_id: &str, src_id: &str, target_id: &str, edge: Edge, cx: &mut Context<Self>) {
        self.fit_edge = Some(edge);
        self.update_root(cx, tab_id, |r| move_leaf(r, src_id, target_id, edge));
        self.focus_leaf(tab_id, src_id, cx);
    }

    /// Take these sources (`source_key`) out of `leaf_id` into a new split at `edge` of `target_id`.
    pub fn split_out(&mut self, tab_id: &str, leaf_id: &str, keys: &[String], target_id: &str, edge: Edge, cx: &mut Context<Self>) {
        self.fit_edge = Some(edge);
        let fresh = new_leaf(vec![]); // id known now: the updater below runs later
        let fresh_id = fresh.id.clone();
        self.update_root(cx, tab_id, |r| {
            let from = find_leaf(r, leaf_id)?.clone();
            let moving: Vec<SourceRef> = from.sources.iter().filter(|s| keys.contains(&source_key(s))).cloned().collect();
            let staying: Vec<SourceRef> = from.sources.iter().filter(|s| !keys.contains(&source_key(s))).cloned().collect();
            if moving.is_empty() {
                return r.cloned();
            }
            // All of them: that is just moving the split.
            if staying.is_empty() {
                return if target_id == leaf_id { r.cloned() } else { move_leaf(r, leaf_id, target_id, edge) };
            }
            let shrunk = map_leaf(r, leaf_id, &|l| tree::Leaf { sources: staying.clone(), ..l });
            insert_at(shrunk.as_ref(), target_id, tree::Leaf { sources: moving, ..fresh }.into(), edge)
        });
        self.focus_leaf(tab_id, &fresh_id, cx); // a missing id falls back to the first split
    }

    /// Move sources (`keys`, or all of them when None) of `leaf_id` into `target_id`, merging the chats.
    pub fn merge(&mut self, tab_id: &str, leaf_id: &str, keys: Option<&[String]>, target_id: &str, cx: &mut Context<Self>) {
        if leaf_id == target_id {
            return;
        }
        self.update_root(cx, tab_id, |r| {
            let from = find_leaf(r, leaf_id)?;
            let to = find_leaf(r, target_id)?;
            let moving: Vec<SourceRef> = match keys {
                Some(k) => from.sources.iter().filter(|s| k.contains(&source_key(s))).cloned().collect(),
                None => from.sources.clone(),
            };
            let have: HashSet<String> = to.sources.iter().map(source_key).collect();
            let added: Vec<SourceRef> = moving.iter().filter(|s| !have.contains(&source_key(s))).cloned().collect();
            let staying: Vec<SourceRef> = from.sources.iter().filter(|s| !moving.contains(s)).cloned().collect();
            let merged = map_leaf(r, target_id, &|l| {
                let mut sources = l.sources.clone();
                sources.extend(added.iter().cloned());
                tree::Leaf { sources, ..l }
            });
            if !staying.is_empty() {
                map_leaf(merged.as_ref(), leaf_id, &|l| tree::Leaf { sources: staying.clone(), ..l })
            } else {
                remove_leaf(merged.as_ref(), leaf_id)
            }
        });
        self.focus_leaf(tab_id, target_id, cx);
    }

    pub fn remove_source(&mut self, tab_id: &str, leaf_id: &str, key: &str, cx: &mut Context<Self>) {
        self.update_root(cx, tab_id, |r| {
            map_leaf(r, leaf_id, &|l| {
                if l.sources.len() > 1 {
                    let sources = l.sources.iter().filter(|s| source_key(s) != key).cloned().collect();
                    tree::Leaf { sources, ..l }
                } else {
                    l
                }
            })
        });
    }

    /// Another tab dragged onto this one's splits: its whole layout goes in at that edge, and the tab closes.
    pub fn take_tab(&mut self, tab_id: &str, src_tab_id: &str, target_id: Option<&str>, edge: Edge, cx: &mut Context<Self>) {
        self.fit_edge = target_id.map(|_| edge);
        self.mutate(cx, |l| {
            let Some(src) = l.tabs.iter().find(|t| t.id == src_tab_id) else { return };
            let Some(add) = src.root.clone() else { return };
            if src_tab_id == tab_id {
                return;
            }
            l.tabs.retain(|t| t.id != src_tab_id);
            for t in l.tabs.iter_mut() {
                if t.id == tab_id {
                    t.root = match target_id {
                        Some(target) => insert_at(t.root.as_ref(), target, add.clone(), edge),
                        None => Some(add.clone()),
                    };
                }
            }
            l.active = tab_id.to_string();
        });
    }

    /// Ctrl+W: close the focused split and hand focus to its neighbour.
    pub fn close_focused_split(&mut self, cx: &mut Context<Self>) {
        let tab = self.active_tab().clone();
        let Some(id) = self.focused_of(&tab) else { return };
        let before = leaves(tab.root.as_ref());
        let idx = before.iter().position(|l| l.id == id).unwrap_or(0);
        let root = remove_leaf(tab.root.as_ref(), &id);
        let after = leaves(root.as_ref());
        self.update_root(cx, &tab.id, |_| root.clone());
        if !after.is_empty() {
            let next = &after[idx.saturating_sub(1).min(after.len() - 1)];
            self.focus_leaf(&tab.id, &next.id, cx);
        }
    }

    // ---- dialogs ---------------------------------------------------------------------

    pub fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.dialog = None;
        self.dialog_view = None;
        self.settings_view = None;
        cx.notify();
    }

    pub fn open_dialog(&mut self, dialog: Dialog, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if dialog == Dialog::Settings {
            // AGU_SHOT=settings:<section> opens that page (screenshots)
            let section = std::env::var("AGU_SHOT").ok().and_then(|v| v.strip_prefix("settings:").and_then(|n| Section::from_name(n))).unwrap_or(Section::Accounts);
            let bridge = self.bridge.clone();
            let view = cx.new(|cx| SettingsDialog::new(bridge, section, window, cx));
            let sub = cx.subscribe_in(&view, window, |this, _v, _ev: &DialogEvent, window, cx| {
                this.close_dialog(cx);
                window.focus(&this.focus);
            });
            self.dialog = Some(dialog);
            self.settings_view = Some((view, sub));
            cx.notify();
            return;
        }
        let (title, initial, label) = match &dialog {
            Dialog::Add { .. } => ("Select channel".to_string(), vec![], "Add channel"),
            Dialog::Whisper { .. } => ("Whisper someone".to_string(), vec![], "Open"),
            Dialog::Change { tab_id, leaf_id } => {
                let tab = self.layout.tabs.iter().find(|t| &t.id == tab_id);
                match tab.and_then(|t| find_leaf(t.root.as_ref(), leaf_id)) {
                    Some(l) => ("Change channel(s)".to_string(), l.sources, "Apply"),
                    None => return,
                }
            }
            Dialog::Split { tab_id, leaf_id, below } => {
                let tab = self.layout.tabs.iter().find(|t| &t.id == tab_id);
                if tab.and_then(|t| find_leaf(t.root.as_ref(), leaf_id)).is_none() {
                    return;
                }
                (if *below { "Add split below" } else { "Add split to the right" }.to_string(), vec![], "Add split")
            }
            Dialog::Settings | Dialog::CloseTab { .. } => {
                self.dialog = Some(dialog);
                cx.notify();
                return;
            }
        };
        let whisper = matches!(dialog, Dialog::Whisper { .. });
        let view = cx.new(|cx| ChannelDialog::new(window, cx, &title, initial, label, whisper));
        let sub = cx.subscribe_in(&view, window, |this, _v, ev: &DialogEvent, window, cx| match ev {
            DialogEvent::Close => {
                this.close_dialog(cx);
                window.focus(&this.focus);
            }
            DialogEvent::Submit(sources) => {
                this.submit_channels(sources.clone(), cx);
                window.focus(&this.focus);
            }
        });
        self.dialog = Some(dialog);
        self.dialog_view = Some((view, sub));
        cx.notify();
    }

    fn submit_channels(&mut self, sources: Vec<SourceRef>, cx: &mut Context<Self>) {
        let d = self.dialog.take();
        self.dialog_view = None;
        let Some(d) = d else { return };
        let leaf = new_leaf(sources.clone());
        let leaf_id = leaf.id.clone();
        match d {
            Dialog::Settings | Dialog::CloseTab { .. } => {}
            Dialog::Add { tab_id } | Dialog::Whisper { tab_id } => {
                let first = self.layout.tabs.iter().find(|t| t.id == tab_id).and_then(|t| leaves(t.root.as_ref()).first().map(|l| l.id.clone()));
                let edge = first.as_deref().map(|f| self.roomy_edge(&tab_id, f, Edge::Right)).unwrap_or(Edge::Right);
                self.fit_edge = Some(edge);
                self.update_root(cx, &tab_id, |r| match r {
                    Some(root) => {
                        let first = leaves(Some(root))[0].id.clone();
                        insert_at(Some(root), &first, leaf.into(), edge)
                    }
                    None => Some(leaf.into()),
                });
                self.focus_leaf(&tab_id, &leaf_id, cx);
            }
            Dialog::Change { tab_id, leaf_id: target } => {
                self.update_root(cx, &tab_id, |r| map_leaf(r, &target, &|l| tree::Leaf { sources: sources.clone(), ..l }));
            }
            Dialog::Split { tab_id, leaf_id: target, below } => {
                let edge = self.roomy_edge(&tab_id, &target, if below { Edge::Bottom } else { Edge::Right });
                self.fit_edge = Some(edge);
                self.update_root(cx, &tab_id, |r| insert_at(r, &target, leaf.into(), edge));
                self.focus_leaf(&tab_id, &leaf_id, cx);
            }
        }
        cx.notify();
    }

    pub fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    // ---- keyboard (bindings from settings.hotkeys over the defaults in hotkeys.rs) -------------------

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if HOTKEY_RECORDING.active() {
            return; // the settings page is recording a shortcut
        }
        let ks = &ev.keystroke;
        let stop = |window: &mut Window, cx: &mut App| {
            window.prevent_default();
            cx.stop_propagation();
        };
        if ks.key == "escape" {
            // Esc closes the focused split's search (MessageList.tsx)
            let tab = self.active_tab().clone();
            if let Some(slot) = self.focused_of(&tab).and_then(|id| self.panes.get(&id)) {
                if self.menu.is_none() && self.dialog.is_none() && slot.view.read(cx).search_open {
                    let v = slot.view.clone();
                    v.update(cx, |p, cx| p.close_search(window, cx));
                    stop(window, cx);
                    return;
                }
            }
            if self.menu.is_some() {
                self.close_menu(cx);
                stop(window, cx);
            } else if self.dialog.is_some() && self.tabs.editing.is_none() {
                self.close_dialog(cx);
                window.focus(&self.focus);
                stop(window, cx);
            }
            return;
        }
        if ks.key == "enter" {
            if let Some(Dialog::CloseTab { tab_id, others }) = self.dialog.clone() {
                self.confirm_close_tab(&tab_id, others, window, cx);
                stop(window, cx);
                return;
            }
        }
        // Ctrl+F: only the focused split's list reacts, and not while a dialog is open
        if crate::text_input::primary_held(&ks.modifiers) && !ks.modifiers.alt && !ks.modifiers.shift && ks.key == "f" && self.dialog.is_none() {
            let tab = self.active_tab().clone();
            if let Some(slot) = self.focused_of(&tab).and_then(|id| self.panes.get(&id)) {
                let v = slot.view.clone();
                v.update(cx, |p, cx| p.open_search(window, cx));
                stop(window, cx);
                return;
            }
        }
        let Some(binding) = event_to_binding(&key_like(ks)) else { return };
        let Some(action) = self.keymap.get(&binding).copied() else { return };

        if action == HotkeyAction::OpenSettings {
            if matches!(&self.dialog, Some(d) if *d != Dialog::Settings) {
                return;
            }
            stop(window, cx);
            if self.dialog == Some(Dialog::Settings) {
                self.close_dialog(cx);
            } else {
                self.open_dialog(Dialog::Settings, window, cx);
            }
            return;
        }
        if self.dialog.is_some() {
            return; // dialogs own the keyboard
        }
        stop(window, cx);
        let n = self.layout.tabs.len();
        let idx = self.layout.tabs.iter().position(|t| t.id == self.layout.active).unwrap_or(0);
        let active = self.layout.active.clone();
        match action {
            HotkeyAction::NewTab => self.add_tab(cx),
            HotkeyAction::CloseTab => self.close_tab(&active, cx),
            HotkeyAction::CloseSplit => self.close_focused_split(cx),
            HotkeyAction::NextTab | HotkeyAction::PrevTab => {
                let step: isize = if action == HotkeyAction::PrevTab { -1 } else { 1 };
                let to = (idx as isize + step).rem_euclid(n as isize) as usize;
                let id = self.layout.tabs[to].id.clone();
                self.select_tab(&id, cx);
            }
            HotkeyAction::SelectLastTab => {
                let id = self.layout.tabs[n - 1].id.clone();
                self.select_tab(&id, cx);
            }
            HotkeyAction::SelectTab(k) => {
                if let Some(t) = self.layout.tabs.get(k as usize - 1) {
                    let id = t.id.clone();
                    self.select_tab(&id, cx);
                }
            }
            HotkeyAction::OpenSettings => {}
        }
    }

    fn confirm_close_tab(&mut self, tab_id: &str, others: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.close_dialog(cx);
        window.focus(&self.focus);
        if others {
            self.close_others_now(tab_id, cx);
        } else {
            self.close_tab_now(tab_id, cx);
        }
    }

    // ---- window.agu.win (the UI owns its window here) --------------------------------------------

    /// A grab-handle position probe: records `key`'s bounds each frame; a changed width/position repaints once.
    pub fn probe(&self, key: String, cx: &mut Context<Self>) -> impl IntoElement {
        let anchors = self.anchors.clone();
        let this = cx.entity().downgrade();
        gpui::canvas(
            move |bounds, _w, cx| {
                let changed = anchors.borrow().get(&key).map(|old| (old.origin.x - bounds.origin.x).abs() > px(0.5) || (old.size.width - bounds.size.width).abs() > px(0.5) || (old.origin.y - bounds.origin.y).abs() > px(0.5) || (old.size.height - bounds.size.height).abs() > px(0.5)).unwrap_or(true);
                if changed {
                    anchors.borrow_mut().insert(key.clone(), bounds);
                    let this = this.clone();
                    cx.defer(move |cx| {
                        let _ = this.update(cx, |_, cx| cx.notify());
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    pub fn anchor(&self, key: &str) -> Option<Bounds<Pixels>> {
        self.anchors.borrow().get(key).copied()
    }

    // ---- render pieces ----------------------------------------------------------------------------

    fn close_fn(&self, cx: &mut Context<Self>) -> CloseFn {
        let e = cx.entity();
        Rc::new(move |w, cx| {
            e.update(cx, |t, cx| {
                t.close_dialog(cx);
                w.focus(&t.focus);
            })
        })
    }

    fn render_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.dialog.clone()?;
        match dialog {
            Dialog::Settings => self.settings_view.as_ref().map(|(v, _)| v.clone().into_any_element()),
            Dialog::CloseTab { tab_id, others } => {
                let tab = self.layout.tabs.iter().find(|t| t.id == tab_id).cloned();
                let n_others = if others { self.layout.tabs.iter().filter(|t| t.id != tab_id).count() } else { 0 };
                let label = if others { format!("Close {} other tab{}", n_others, if n_others == 1 { "" } else { "s" }) } else { "Close tab".to_string() };
                let name = tab.as_ref().map(|t| tab_title(t, false));
                let (pre, name, post) = if others {
                    ("Every tab except ".to_string(), name.unwrap_or_else(|| "this one".into()), " is closed, with its splits and channels.".to_string())
                } else {
                    (String::new(), name.unwrap_or_else(|| "This tab".into()), " is closed, with its splits and channels.".to_string())
                };
                let text = format!("{pre}{name}{post}");
                let bold = pre.len()..pre.len() + name.len();
                let styled = StyledText::new(text).with_highlights(vec![(bold, HighlightStyle { font_weight: Some(gpui::FontWeight::SEMIBOLD), color: Some(c(theme::TEXT).into()), ..Default::default() })]);
                let cancel = {
                    let e = cx.entity();
                    move |_: &ClickEvent, w: &mut Window, cx: &mut App| {
                        e.update(cx, |t, cx| {
                            t.close_dialog(cx);
                            w.focus(&t.focus);
                        })
                    }
                };
                let confirm = cx.listener({
                    let tab_id = tab_id.clone();
                    move |this, _: &ClickEvent, w, cx| this.confirm_close_tab(&tab_id, others, w, cx)
                });
                let footer = div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(btn("cancel-close", "Cancel", BtnOpts::default()).on_click(cancel))
                    .child(btn("confirm-close", label.clone(), BtnOpts::danger()).on_click(confirm))
                    .into_any_element();
                let body = div().px(px(12.)).py(px(12.)).text_size(px(12.)).text_color(c(theme::MUTED)).child(styled);
                Some(modal(&format!("{label}?"), 360., self.close_fn(cx), body, Some(footer)))
            }
            Dialog::Add { .. } | Dialog::Whisper { .. } | Dialog::Change { .. } | Dialog::Split { .. } => self.dialog_view.as_ref().map(|(v, _)| v.clone().into_any_element()),
        }
    }

    /// The content area (the split host) in px: its size from the last frame, else the window minus the chrome.
    fn fit_content(&self, window: &Window, chrome: f32) -> (f32, f32) {
        match self.anchor("split-host") {
            Some(b) => (f32::from(b.size.width), f32::from(b.size.height)),
            None => {
                let v = window.viewport_size();
                (f32::from(v.width), (f32::from(v.height) - chrome).max(0.0))
            }
        }
    }

    /// Keeps the window as big as the active tab's splits need. When a split is added, removed or moved the
    /// chats that were there keep their pixel size, new ones get their neighbours' size, and the window is
    /// resized to match (growing or shrinking, on the side the split went / came from). Otherwise the splits are
    /// kept above their minimum, the minimum window size follows the layout, and a tab switch grows the window
    /// when needed. Runs from `render`, so it sees every layout or tab change.
    fn fit_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.fit_hooked {
            self.fit_hooked = true;
            window_fit::install(window);
        }
        let edge = self.fit_edge.take();
        let chrome = 28.0 + if self.updater.status == "ready" && self.bar_dismissed != self.updater.version { 24.0 } else { 0.0 };
        self.fit_max = window_fit::max_client(window).map(|(w, h)| (w, (h - chrome).max(0.0)));
        let tab_id = self.active_tab().id.clone();
        let mut root = self.active_tab().root.clone();
        let (cw, ch) = self.fit_content(window, chrome);
        let min_of = |r: &Option<SplitNode>| r.as_ref().map(window_fit::tree_min_size).unwrap_or((0.0, 0.0));
        let set_min = |window: &Window, m: (f32, f32)| window_fit::set_min_client(window, m.0.max(window_state::MIN_W), (m.1 + chrome).max(window_state::MIN_H));
        let make_prev = |tab_id: &str, root: &Option<SplitNode>, w: f32, h: f32, pending: Option<Instant>| {
            let m = min_of(root);
            root.as_ref().map(|r| FitPrev { tab_id: tab_id.to_string(), shape: window_fit::shape(r), root: r.clone(), w, h, chrome: chrome as i32, min: (m.0.ceil() as i32, m.1.ceil() as i32), pending })
        };

        // while a divider is being dragged only the minimum follows (the drag keeps every split above it)
        if self.sv.resize.is_some() {
            set_min(window, min_of(&root));
            self.fit_prev = make_prev(&tab_id, &root, cw, ch, None);
            return;
        }

        let prev = self.fit_prev.take();
        if let (Some(prev), Some(cur)) = (&prev, &root) {
            if prev.tab_id == tab_id && prev.shape != window_fit::shape(cur) {
                let old_px = window_fit::leaf_px(&prev.root, prev.w, prev.h);
                let (new_tree, tw, _) = window_fit::replan(cur, &old_px);
                if new_tree != *cur {
                    let t = new_tree.clone();
                    self.update_root(cx, &tab_id, move |_| Some(t));
                }
                let (from_left, from_top) = if window_fit::leaf_px(cur, 1.0, 1.0).len() < window_fit::leaf_px(&prev.root, 1.0, 1.0).len() {
                    window_fit::removed_side(&prev.root, cur)
                } else {
                    (edge == Some(Edge::Left), edge == Some(Edge::Top))
                };
                let new_root = Some(new_tree);
                let need = min_of(&new_root);
                set_min(window, need);
                // the height stays as it is unless the stacked chats need more (they share it)
                let th = prev.h.max(need.1);
                let (rw, rh) = (tw.max(window_state::MIN_W), (th + chrome).max(window_state::MIN_H));
                // The resize runs after this frame: a SetWindowPos inside render lands while gpui is drawing, which
                // drops the new size and leaves the changed part blank until something else repaints.
                cx.spawn_in(window, async move |_, cx| {
                    let plan = cx.update(|window, _| window_fit::plan_resize(window, rw, rh, from_left, from_top)).ok().flatten();
                    if let Some((hwnd, to)) = plan {
                        // outside the update: gpui takes the WM_SIZE this sends
                        window_fit::apply_grow(hwnd, to);
                        let _ = cx.update(|window, _| window.refresh());
                        cx.background_executor().timer(Duration::from_millis(400)).await;
                        let _ = cx.update(|window, _| window.refresh());
                    }
                })
                .detach();
                self.fit_prev = make_prev(&tab_id, &new_root, tw, th, Some(Instant::now()));
                return;
            }
        }

        // a resize just ordered is still on its way: the host size is the old one, so keep the sizes as they are
        let settling = prev.as_ref().and_then(|p| p.pending).is_some_and(|t| {
            let p = prev.as_ref().unwrap();
            t.elapsed() < Duration::from_millis(350) && ((cw - p.w).abs() > 1.5 || (ch - p.h).abs() > 1.5) && !window.is_maximized() && !window.is_fullscreen()
        });
        if settling {
            let p = prev.unwrap();
            set_min(window, min_of(&root));
            self.fit_prev = Some(p);
            return;
        }
        let known = self.anchor("split-host").is_some();
        if known {
            if let Some(fixed) = root.as_ref().and_then(|r| window_fit::enforce_min(r, cw, ch)) {
                let t = fixed.clone();
                self.update_root(cx, &tab_id, move |_| Some(t));
                root = Some(fixed);
            }
        }
        let m = min_of(&root);
        set_min(window, m);
        let next = make_prev(&tab_id, &root, cw, ch, None);
        let changed = match (&prev, &next) {
            (Some(p), Some(n)) => p.tab_id != n.tab_id || p.min != n.min || p.chrome != n.chrome,
            _ => true,
        };
        if changed {
            let (w, h) = (m.0.max(window_state::MIN_W), (m.1 + chrome).max(window_state::MIN_H));
            cx.spawn_in(window, async move |_, cx| {
                let plan = cx.update(|window, _| window_fit::plan_grow(window, w, h, edge)).ok().flatten();
                if let Some((hwnd, to)) = plan {
                    // outside the update: gpui takes the WM_SIZE this sends
                    window_fit::apply_grow(hwnd, to);
                    let _ = cx.update(|window, _| window.refresh());
                }
            })
            .detach();
        }
        self.fit_prev = next;
    }

    fn render_update_bar(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let show = self.updater.status == "ready" && self.bar_dismissed != self.updater.version;
        if !show {
            return None;
        }
        let version = self.updater.version.clone().unwrap_or_default();
        let bridge = self.bridge.clone();
        let v2 = self.updater.version.clone();
        Some(
            div()
                .flex()
                .flex_none()
                .h(px(24.))
                .items_center()
                .gap(px(12.))
                .border_b_1()
                .border_color(c(theme::LINE))
                .bg(c(theme::ACCENT_STRONG))
                .px(px(12.))
                .text_size(px(12.))
                .text_color(c(theme::WHITE))
                .child(div().min_w_0().flex_1().truncate().child(format!("Agu Chat {version} is ready \u{2014} Restart to update")))
                .child(
                    div()
                        .id("update-restart")
                        .rounded(px(4.))
                        .bg(theme::a(theme::WHITE, 0.2))
                        .px(px(8.))
                        .text_size(px(11.))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .hover(|s| s.bg(theme::a(theme::WHITE, 0.3)))
                        .on_click(move |_: &ClickEvent, _w, cx| {
                            cx.background_spawn(bridge.updater_install()).detach();
                        })
                        .child("Restart"),
                )
                .child(
                    div()
                        .id("update-later")
                        .text_size(px(11.))
                        .text_color(theme::a(theme::WHITE, 0.8))
                        .hover(|s| s.text_color(c(theme::WHITE)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _w, cx| {
                            this.bar_dismissed = v2.clone();
                            cx.notify();
                        }))
                        .child("Later"),
                )
                .into_any_element(),
        )
    }
}

/// Builds the DOM-style event `eventToBinding` expects from a GPUI keystroke.
pub(crate) fn key_like(ks: &Keystroke) -> KeyLike {
    let k = ks.key.as_str();
    let mut chars = k.chars();
    let code = match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii_alphabetic() => format!("Key{}", ch.to_ascii_uppercase()),
        (Some(ch), None) if ch.is_ascii_digit() => format!("Digit{ch}"),
        (Some(ch), None) => match ch {
            '-' => "Minus",
            '=' => "Equal",
            '[' => "BracketLeft",
            ']' => "BracketRight",
            ';' => "Semicolon",
            '\'' => "Quote",
            ',' => "Comma",
            '.' => "Period",
            '/' => "Slash",
            '\\' => "Backslash",
            '`' => "Backquote",
            _ => "",
        }
        .to_string(),
        _ => String::new(),
    };
    KeyLike {
        key: k.to_string(),
        code,
        ctrl_key: ks.modifiers.control,
        alt_key: ks.modifiers.alt,
        shift_key: ks.modifiers.shift,
        meta_key: cfg!(target_os = "macos") && ks.modifiers.platform, // the Windows key is not Cmd
    }
}

impl Focusable for AguApp {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for AguApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A drag that ended anywhere (dropped on nothing, or cancelled) leaves no targets behind.
        if !cx.has_active_drag() {
            self.tab_drag = None;
            self.tabs.clear_drag();
            self.sv.clear_drag();
        }
        self.fit_window(window, cx);
        if self.on_top_applied != Some(self.settings.always_on_top) {
            let on = self.settings.always_on_top;
            self.on_top_applied = Some(on);
            // after this frame, like the window resizes: native window calls inside render can be lost
            cx.spawn_in(window, async move |_, cx| {
                let _ = cx.update(|window, _| window_fit::set_always_on_top(window, on));
            })
            .detach();
        }
        let tabs_bar = self.render_tabs(window, cx);
        let update_bar = self.render_update_bar(cx);
        let content = self.render_split_view(window, cx);
        let dialog = self.render_dialog(cx);
        let menu = self.menu.as_ref().map(|m| popup_menu(m, cx));
        let resizing = self.sv.resize.as_ref().map(|r| r.horizontal);

        div()
            .id("agu-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(c(theme::APP))
            .font_family(theme::FONT)
            .text_color(c(theme::TEXT))
            .text_size(px(16.))
            .line_height(relative(1.5))
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .on_modifiers_changed(cx.listener(|this, ev: &gpui::ModifiersChangedEvent, _w, cx| {
                // settings.pauseChat == "alt": the lists freeze while Alt is held
                let alt = ev.modifiers.alt;
                for slot in this.panes.values() {
                    slot.view.update(cx, |p, cx| p.set_alt(alt, cx));
                }
            }))
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| this.on_root_mouse_move(ev, cx)))
            .on_mouse_up(gpui::MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _w, cx| this.on_root_mouse_up(ev, cx)))
            .on_mouse_up_out(gpui::MouseButton::Left, cx.listener(|this, ev: &MouseUpEvent, _w, cx| this.on_root_mouse_up(ev, cx)))
            .when(resizing == Some(true), |d| d.cursor_col_resize())
            .when(resizing == Some(false), |d| d.cursor_row_resize())
            .child(tabs_bar)
            .children(update_bar)
            .child(div().relative().min_h_0().flex_1().child(content))
            .children(dialog)
            .children(menu)
    }
}
