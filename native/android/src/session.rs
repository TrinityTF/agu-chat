//! The Android app's Rust side: one `Session` owns the core backend and a chat store per open view (tab). Kotlin
//! talks to it with JSON commands (`call`) and hears back through JSON events (the `Sink`):
//!
//! - `rows`     `{vk, reset, upsert: [row], remove: [key]}`: a view's rows to add or replace (in list order) and rows
//!              gone. `reset` means the list is exactly `upsert` (a snapshot, or the emotes changed every line).
//! - `conn`     `{sk, conn}`: a channel's connection state (`ConnFrame`).
//! - `preview`  `{key, provider, title, subtitle, thumb}`: a link preview card that just arrived (`preview.get`).
//! - `activity` `{vk, kind}`: new messages in a view ("message", or "highlight" for a mention / highlight rule).
//! - `alert`    `{sound: {file, volume} | null, vibrate}`: play the sound / vibrate for a highlight.
//! - `mentions` `{unread, total}`: the bell's count (the list is the `mentions.list` call).
//! - `accounts` the `AccountsState` map; `settings` the settings object.
//! - `login.webview` / `login.show` / `login.close` / `login.forget`: the cookie logins' sign-in screen (`login.rs`).
//!
//! A view holds one or more channels from any platform, merged into one list. A channel shown in several views is
//! subscribed once (`hub.rs`); its frames go to every view that holds it. Frames are applied off the UI thread and
//! the diffs are sent at most every `FLUSH_MS`, so a busy chat costs the bridge a few calls a second.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agu_chat_core::link_preview::cache::PreviewCache;
use agu_chat_core::link_preview::{match_url, Preview};
use agu_chat_core::protocol::{AccountsState, AguBridge, Platform, Settings, SourceRef};
use agu_chat_core::twitch::whisper;
use agu_chat_core::{AppMeta, Backend};
use agu_chat_model::alerts::ping_samples;
use agu_chat_model::ignores::IgnoreCache;
use agu_chat_model::mentions::{classify, mention_id, row_deleted, Alert, Mentions};
use agu_chat_core::chat_rules::{chat_gate, GateLevel};
use agu_chat_core::protocol::ConnFrame;
use agu_chat_model::input::autocomplete::{chatters, suggest, ChangeCtx};
use agu_chat_model::input::gate::twitch_viewer;
use agu_chat_model::input::inventory::{build_kick_inventory, build_rumble_inventory, build_twitch_inventory, build_youtube_inventory, merge_inventories, Inventory};
use agu_chat_model::input::slash::{confirmed_command, post_json, Confirmed};
use agu_chat_model::mod_caps::{fetch_mod_caps, relay_api, CoCaps, ModCaps};
use agu_chat_model::types::RowBody;
use agu_chat_model::net::Net;
use agu_chat_model::search::{parse_search, SearchQuery};
use agu_chat_model::store::{source_key, ChatStore};
use agu_chat_model::twitch::badges::BadgeMap;
use agu_chat_model::twitch::cheermotes::{load_cheermotes, Cheermotes};
use agu_chat_model::twitch::cosmetics::{defs_from_raw, fetch_chatter_style, fetch_seven_tv_defs, retry_delay, SevenTvDefs, StyleBook};
use agu_chat_model::twitch::emotes::{fetch_seven_tv_personal, fetch_twitch_picker_emotes, ProviderEmote, TwitchEmotesResp, TwitchPickerEmote, fetch_seven_tv_set, flush_cache, forget_cache, init_cache, merge_emote_sets, seven_tv_channel_key_on, EmoteSets, ProviderScopedMap, SevenTvIds, SevenTvPlatform};
use agu_chat_model::twitch::seven_events::{apply_changes, same_id, Dispatch, EmoteChange, SevenEvent, SevenSocket, EMOTE_SET_UPDATE, USER_UPDATE};
use agu_chat_model::twitch::use_twitch_data::{load_badges, load_cached_provider_sets, load_kick_provider_sets, load_provider_sets, reload_seven_tv, ProviderSets};
use futures::future::BoxFuture;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::alerts::{Alerter, Wants};
use crate::compose::{byte_of, picker_json, plan_send, send_failures, suggestions_json, SendPlan, Target};
use crate::hub::{Added, Hub};
use crate::view::{row_json, style_key, Env, Rules};

#[path = "moderation.rs"]
mod moderation;
use moderation::{build_mod_env, DevRelay};

/// Events to the app: (kind, JSON payload). Called from background threads.
pub type Sink = Arc<dyn Fn(&str, &Value) + Send + Sync>;

const FLUSH_MS: u64 = 60;

/// What is known of one channel, whichever views show it.
#[derive(Default)]
struct SrcData {
    badges: Arc<BadgeMap>,
    /// 7TV / BTTV / FFZ as loaded, kept per provider so the settings and live 7TV changes can re-merge them
    sets: Option<Arc<ProviderSets>>,
    emotes: Option<Arc<EmoteSets>>,
    emotes_loading: bool,
    /// the Twitch channel id the emote sets and cheermotes belong to
    channel_id: Option<String>,
    cheermotes: Option<Arc<Cheermotes>>,
    cheermotes_loading: bool,
    /// the 7TV user and set this channel is listening to for live emote changes
    seven_ids: Option<SevenTvIds>,
    /// the last `conn` frame, to tell a view that opens on a channel that is already connected
    conn: Option<Value>,
    /// the account's own Twitch emotes (what the picker and ":" offer), and what the account may do here
    twitch_emotes: TwitchEmotesResp,
    caps: ModCaps,
    /// Kick / Rumble: whether the account moderates the chat
    co: CoCaps,
    /// debuggable builds: `caps` / `co` were set by hand and are not refreshed
    dev_caps: bool,
    /// Kick: the broadcaster's user id the 7TV set was asked for
    kick7: Option<String>,
    /// the emote inventory built for this generation of the emote data
    inv: Option<(u64, Arc<Inventory>)>,
}

/// What a row is drawn against, besides its own channel's data.
struct Ctx<'a> {
    src: &'a HashMap<String, SrcData>,
    rules: &'a Rules,
    defs: Option<Arc<SevenTvDefs>>,
    book: &'a Arc<StyleBook>,
}

/// One tab's list: a store merging all its channels, and what the app was last sent of it.
struct View {
    store: ChatStore,
    /// what the app was last sent: row key -> the row's Arc address (a changed row is a new Arc)
    sent: HashMap<String, usize>,
    sent_order: Vec<String>,
    ignore_cache: IgnoreCache,
    dirty: bool,
    /// everything must be sent again (snapshot replay, emotes or badges arrived, settings changed)
    full: bool,
    /// rows up to this arrival number were looked at for alerts
    seen_ord: u64,
    /// the tab's search: only matching rows are shown
    search: Option<SearchQuery>,
    /// chatters whose 7TV style just arrived: their rows are sent again
    restyle: HashSet<String>,
}

impl View {
    fn new() -> Self {
        View { store: ChatStore::default(), sent: HashMap::new(), sent_order: Vec::new(), ignore_cache: IgnoreCache::default(), dirty: true, full: true, seen_ord: 0, search: None, restyle: HashSet::new() }
    }

    /// The rows event since the last one, or None when nothing changed.
    fn diff(&mut self, vk: &str, cx: &Ctx) -> Option<Value> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        let rules = cx.rules;
        let rows = rules.shown(self.store.rows(), &mut self.ignore_cache, self.search.as_ref());
        let restyle = std::mem::take(&mut self.restyle);
        let now: Vec<(String, usize)> = rows.iter().map(|r| (r.key.clone(), Arc::as_ptr(r) as usize)).collect();
        let keys_now: HashSet<&str> = now.iter().map(|(k, _)| k.as_str()).collect();
        let removed: Vec<String> = self.sent_order.iter().filter(|k| !keys_now.contains(k.as_str())).cloned().collect();
        let new_count = now.iter().filter(|(k, _)| !self.sent.contains_key(k)).count();
        // new rows must all sit at the end of the list for an incremental update; history arriving in the middle
        // (or anything else unusual) sends the whole list
        let tail_ok = now.iter().skip(now.len() - new_count).all(|(k, _)| !self.sent.contains_key(k));
        let full = self.full || !tail_ok;
        self.full = false;
        let mut envs: HashMap<String, Env> = HashMap::new();
        let mut upsert = Vec::new();
        for (r, (k, p)) in rows.iter().zip(now.iter()) {
            let styled = !restyle.is_empty() && restyle.contains(&style_key(r));
            if full || styled || self.sent.get(k) != Some(p) {
                let env = envs.entry(r.sk.clone()).or_insert_with(|| env_of(&r.sk, cx, Some(&self.store)));
                let mut v = row_json(r, env);
                rules.decorate(&mut v, r);
                upsert.push(v);
            }
        }
        self.sent = now.iter().cloned().collect();
        self.sent_order = now.into_iter().map(|(k, _)| k).collect();
        if !full && upsert.is_empty() && removed.is_empty() {
            return None;
        }
        Some(json!({"vk": vk, "reset": full, "upsert": upsert, "remove": if full { vec![] } else { removed }}))
    }
}

fn env_of(sk: &str, cx: &Ctx, store: Option<&ChatStore>) -> Env {
    let mut env = Env { look: cx.rules.look(), defs: cx.defs.clone(), book: Some(cx.book.clone()), ..Env::default() };
    if let Some(d) = cx.src.get(sk) {
        env.badges = d.badges.clone();
        env.emotes = d.emotes.clone();
        env.cheermotes = d.cheermotes.clone();
    }
    if let Some(st) = store.and_then(|s| s.source(sk)) {
        env.mods = build_mod_env(&st.source, cx.src.get(sk), cx.rules, store);
        env.known = st.known.clone();
        env.rumble_badges = st.rumble_badges.clone();
        env.rumble_emotes = st.rumble_emotes.clone();
    }
    env
}

/// The emotes a channel's rows are drawn with: the providers the settings switch on, merged.
fn merge_for(sets: &ProviderSets, s: &Settings) -> EmoteSets {
    let empty = ProviderScopedMap::default();
    merge_emote_sets(if s.seven_tv { &sets.seven } else { &empty }, if s.bttv { &sets.bttv } else { &empty }, if s.ffz { &sets.ffz } else { &empty })
}

fn whisper_or_channel(s: &SourceRef) -> String {
    if s.platform == Platform::Twitch {
        if let Some(login) = whisper::login_of(&s.channel) {
            return login;
        }
    }
    s.channel.clone()
}

struct State {
    hub: Hub,
    views: HashMap<String, View>,
    src: HashMap<String, SrcData>,
    rules: Rules,
    alerter: Alerter,
    mentions: Mentions,
    mentions_open: bool,
    /// the (unread, total) the app was last told
    mentions_sent: (usize, usize),
    /// the app is on screen (an alert vibrates only in the background)
    active: bool,
    /// 7TV paint and badge definitions (once fetched) and every chatter's style
    defs: Option<Arc<SevenTvDefs>>,
    defs_requested: bool,
    defs_failures: u32,
    book: Arc<StyleBook>,
    /// link preview cards: one cache for every tab, at most 4 requests at a time
    previews: PreviewCache,
    seven_seq: u64,
    /// the logged-in account's 7TV personal emotes (the picker and ":" offer them), and whose they are
    personal: Vec<Arc<ProviderEmote>>,
    personal_requested: HashSet<String>,
    /// counts every change of emote data: an inventory built for an older count is built again
    inv_gen: u64,
}

type Out = Vec<(&'static str, Value)>;

impl State {
    fn new() -> State {
        State { hub: Hub::default(), views: HashMap::new(), src: HashMap::new(), rules: Rules::default(), alerter: Alerter::default(), mentions: Mentions::default(), mentions_open: false, mentions_sent: (0, 0), active: true, defs: None, defs_requested: false, defs_failures: 0, book: Arc::new(StyleBook::default()), previews: PreviewCache::new(), seven_seq: 0, personal: Vec::new(), personal_requested: HashSet::new(), inv_gen: 0 }
    }

    /// Everything shown depends on these (settings, accounts): draw all rows again.
    fn redraw_all(&mut self) {
        for v in self.views.values_mut() {
            v.dirty = true;
            v.full = true;
            v.ignore_cache = IgnoreCache::default();
        }
    }

    fn touch_sk(&mut self, sk: &str) {
        self.inv_gen += 1;
        for vk in self.hub.views_of(sk) {
            if let Some(v) = self.views.get_mut(&vk) {
                v.dirty = true;
                v.full = true;
            }
        }
    }

    /// The settings changed: the emotes each channel draws (7TV / BTTV / FFZ on or off), the link preview cache,
    /// then every row again.
    fn settings_changed(&mut self) {
        let s = self.rules.settings.clone();
        for d in self.src.values_mut() {
            if let Some(sets) = &d.sets {
                d.emotes = Some(Arc::new(merge_for(sets, &s)));
            }
        }
        self.previews.set_enabled(s.link_previews);
        self.inv_gen += 1;
        self.redraw_all();
    }

    /// A chatter's 7TV style arrived: their rows are sent again.
    fn restyled(&mut self, key: &str) {
        for v in self.views.values_mut() {
            v.restyle.insert(key.to_string());
            v.dirty = true;
        }
    }

    /// Alerts, tab activity and the mentions list for the rows that arrived since the view was last looked at.
    fn scan(&mut self, vk: &str, out: &mut Out) {
        let State { hub, views, rules, alerter, mentions, mentions_open, active, .. } = self;
        let Some(view) = views.get_mut(vk) else { return };
        let now = Instant::now();
        let mut max_ord = view.seen_ord;
        let mut activity: Option<&str> = None;
        for r in view.store.rows().iter() {
            if r.ord <= view.seen_ord {
                continue;
            }
            max_ord = max_ord.max(r.ord);
            let (own, hidden) = (rules.is_own(r), rules.hidden(r));
            let Some(alert) = classify(r, own, hidden, rules.me_login(), rules.kick_me(), || rules.rule_of(r)) else { continue };
            activity = activity.or(Some("message"));
            if alert.red() {
                activity = Some("highlight");
                let channel = hub.source(&r.sk).map(|s| whisper_or_channel(&s)).unwrap_or_default();
                mentions.add(vk, &channel, r.clone(), *mentions_open);
            }
            let wants = match alert {
                Alert::Plain => continue,
                Alert::Mention => Wants { sound: rules.settings.mention_sound, sound_file: String::new(), flash: rules.settings.mention_flash },
                Alert::Rule(rule) => Wants { sound: rule.sound, sound_file: rule.sound_file.clone(), flash: rule.flash },
            };
            if let Some(f) = alerter.once(&mention_id(r), &wants, &rules.settings, *active, now) {
                out.push(("alert", json!({"sound": f.sound.map(|(file, volume)| json!({"file": file, "volume": volume})), "vibrate": f.vibrate})));
            }
        }
        view.seen_ord = max_ord;
        if let Some(kind) = activity {
            out.push(("activity", json!({"vk": vk, "kind": kind})));
        }
        // mentions whose message was deleted since leave the list
        if !mentions.is_empty() {
            let gone: Vec<String> = view.store.rows().iter().filter(|r| row_deleted(r)).map(|r| mention_id(r)).collect();
            if !gone.is_empty() {
                mentions.remove(&gone);
            }
        }
    }

    /// The emotes a chat's message box offers (the picker and ":"): Twitch's own, the providers the settings switch
    /// on, the account's 7TV personal set; Kick's, Rumble's and YouTube's come with the chat.
    fn inventory(&mut self, sk: &str) -> Arc<Inventory> {
        let Some(platform) = self.hub.source(sk).map(|s| s.platform) else { return Arc::default() };
        if platform == Platform::Twitch {
            if let Some((g, i)) = self.src.get(sk).and_then(|d| d.inv.clone()) {
                if g == self.inv_gen {
                    return i;
                }
            }
            let rs = &self.rules.settings;
            let d = self.src.get(sk);
            let empty = ProviderScopedMap::default();
            let sets = d.and_then(|d| d.sets.as_ref()).map(|ps| ProviderSets {
                seven: if rs.seven_tv { ps.seven.clone() } else { empty.clone() },
                bttv: if rs.bttv { ps.bttv.clone() } else { empty.clone() },
                ffz: if rs.ffz { ps.ffz.clone() } else { empty.clone() },
                ..Default::default()
            });
            let none = TwitchEmotesResp::default();
            let inv = Arc::new(build_twitch_inventory(sets.as_ref(), d.map(|d| &d.twitch_emotes).unwrap_or(&none), &self.personal));
            let gen = self.inv_gen;
            if let Some(d) = self.src.get_mut(sk) {
                d.inv = Some((gen, inv.clone()));
            }
            return inv;
        }
        let st = self.hub.views_of(sk).iter().find_map(|vk| self.views.get(vk).and_then(|v| v.store.source(sk)));
        Arc::new(match (platform, st) {
            (Platform::Rumble, Some(st)) => build_rumble_inventory(&st.rumble_emotes),
            (Platform::Kick, Some(st)) => build_kick_inventory(&st.kick_emotes),
            (Platform::Youtube, Some(st)) => build_youtube_inventory(&st.youtube_emotes),
            _ => Inventory::default(),
        })
    }

    /// The emotes of the chats a message goes to, merged (one chat: its own).
    fn merged_inventory(&mut self, sks: &[String]) -> Arc<Inventory> {
        let list: Vec<Arc<Inventory>> = sks.iter().map(|sk| self.inventory(sk)).collect();
        match list.len() {
            1 => list[0].clone(),
            _ => Arc::new(merge_inventories(&list.iter().map(|i| (**i).clone()).collect::<Vec<_>>())),
        }
    }

    /// (newest message id, pinned message id) of a chat, for `/pin` and `/unpin`.
    fn message_ids(&self, vk: &str, sk: &str) -> (Option<String>, Option<String>) {
        let Some(store) = self.views.get(vk).map(|v| &v.store) else { return (None, None) };
        let last = store.rows().iter().rev().find_map(|r| match &r.body {
            RowBody::Msg(m) if r.sk == sk => Some(m.id.clone()),
            _ => None,
        });
        (last, store.source(sk).and_then(|s| s.pinned.as_ref().map(|p| p.id.clone())))
    }

    fn me_twitch_id(&self) -> String {
        self.rules.accounts.get(&Platform::Twitch).and_then(|a| a.user.as_ref()).map(|u| u.id.clone()).unwrap_or_default()
    }

    /// How the channel's chat modes apply to the logged-in account (the desktop's `chat_gate`): Kick says who the
    /// account is there; on Twitch it is pieced together from the mod rights and the badges on the account's own
    /// latest line.
    fn gate_of(&self, sk: &str) -> Option<Value> {
        let source = self.hub.source(sk)?;
        let d = self.src.get(sk)?;
        let conn: ConnFrame = serde_json::from_value(d.conn.clone()?).ok()?;
        let modes = conn.chat_modes.as_ref()?;
        let gated = modes.subs_only || modes.members_only || modes.followers_min.is_some() || modes.account_age_min.is_some();
        let viewer = match source.platform {
            Platform::Twitch if gated => {
                let me_login = self.rules.me_login().to_lowercase();
                let broadcaster = d.caps.is_broadcaster.unwrap_or(!me_login.is_empty() && source.channel.to_lowercase().trim_start_matches('#') == me_login);
                let me = self.me_twitch_id();
                let own = self.hub.views_of(sk).iter().find_map(|vk| self.views.get(vk)).and_then(|v| {
                    v.store.rows().iter().rev().find_map(|r| match &r.body {
                        RowBody::Msg(m) if r.sk == sk && !me.is_empty() && m.user.id == me => Some(m.badges.clone()),
                        _ => None,
                    })
                });
                twitch_viewer(broadcaster, d.caps.is_mod == Some(true), own.as_deref())
            }
            _ => conn.chat_viewer.clone().unwrap_or_default(),
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        let g = chat_gate(modes, &viewer, now)?;
        Some(json!({"level": match g.level { GateLevel::Blocked => "blocked", GateLevel::Restricted => "restricted", GateLevel::Info => "info" }, "text": g.text}))
    }

    /// A `mentions` event when the bell's numbers changed.
    fn mentions_event(&mut self, out: &mut Out) {
        let now = (self.mentions.unread(), self.mentions.len());
        if now != self.mentions_sent {
            self.mentions_sent = now;
            out.push(("mentions", json!({"unread": now.0, "total": now.1})));
        }
    }
}

#[derive(Default)]
struct Routes {
    /// the live subscriptions: sub id -> the source key their frames are for
    subs: HashMap<String, String>,
    pending: HashMap<String, Vec<Value>>,
    inflight: usize,
}

enum Inbox {
    /// a frame of a channel: to every view holding it, or only to `only` (a view's replayed snapshot)
    Frame { sk: String, only: Option<String>, f: Value },
    Reset(String),
    /// a live change of a channel's 7TV emotes
    Seven(SevenEvent),
}

struct Inner {
    bridge: Arc<dyn AguBridge>,
    backend: Arc<Backend>,
    net: Arc<Net>,
    sink: Sink,
    data_dir: PathBuf,
    st: Mutex<State>,
    routes: Arc<Mutex<Routes>>,
    inbox: mpsc::UnboundedSender<Inbox>,
    login: crate::login::LoginBridge,
    /// the one socket to 7TV's event API, open while a channel with a 7TV set is
    seven: SevenSocket,
    /// debuggable builds: keeps moderation requests away from the platforms
    dev: Mutex<DevRelay>,
}

#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

/// The Kick broadcaster's user id in a chat's status (what 7TV links its emote set to).
fn kick_owner_id(status: &Value) -> Option<String> {
    match status.get("userId")? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn platform_of(v: &Value) -> Result<Platform, String> {
    serde_json::from_value(v.get("platform").cloned().unwrap_or(Value::Null)).map_err(|_| "unknown platform".to_string())
}

/// `sks: [source key]`
fn sks_arg(v: &Value) -> Vec<String> {
    v.get("sks").and_then(|a| a.as_array()).map(|a| a.iter().filter_map(|s| s.as_str().map(|s| s.to_string())).collect()).unwrap_or_default()
}

fn str_arg<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v.get(k).and_then(|s| s.as_str()).ok_or_else(|| format!("missing {k}"))
}

/// `[{platform, channel}]` -> sources. Twitch and Kick names are case-insensitive, so they are kept lowercase: the
/// same channel typed two ways is one source.
fn channels_arg(v: &Value) -> Result<Vec<SourceRef>, String> {
    let list = v.get("channels").and_then(|c| c.as_array()).ok_or("missing channels")?;
    let mut out = Vec::new();
    for c in list {
        let platform = platform_of(c)?;
        let mut channel = c.get("channel").and_then(|s| s.as_str()).unwrap_or("").trim().trim_start_matches('#').to_string();
        if matches!(platform, Platform::Twitch | Platform::Kick) {
            channel = channel.to_lowercase();
        }
        if channel.is_empty() {
            return Err("enter a channel".into());
        }
        out.push(SourceRef { platform, channel });
    }
    if out.is_empty() {
        return Err("enter a channel".into());
    }
    Ok(out)
}

/// The saved tabs, in the current shape `[{name, channels: [{platform, channel}]}]`. The first Android versions saved
/// `[{platform, channel}]` (one channel per tab).
pub fn migrate_tabs(v: Value) -> Value {
    let Some(list) = v.as_array() else { return json!([]) };
    Value::Array(
        list.iter()
            .filter_map(|t| {
                if let Some(chs) = t.get("channels").and_then(|c| c.as_array()) {
                    let chs: Vec<Value> = chs.iter().filter(|c| c.get("platform").is_some() && c.get("channel").is_some()).cloned().collect();
                    (!chs.is_empty()).then(|| {
                        let mut tab = json!({"name": t.get("name").and_then(|n| n.as_str()).unwrap_or(""), "channels": chs});
                        // mod mode is remembered per tab, like the desktop's per split
                        if t.get("modMode").and_then(|m| m.as_bool()).unwrap_or(false) {
                            tab["modMode"] = json!(true);
                        }
                        tab
                    })
                } else if t.get("platform").is_some() && t.get("channel").is_some() {
                    Some(json!({"name": "", "channels": [{"platform": t["platform"], "channel": t["channel"]}]}))
                } else {
                    None
                }
            })
            .collect(),
    )
}

impl Session {
    pub fn start(data_dir: PathBuf, version: String, sink: Sink) -> Session {
        let _ = std::fs::create_dir_all(&data_dir);
        std::env::set_var("AGU_DATA_DIR", &data_dir);
        let backend = Arc::new(Backend::start_with(data_dir.clone(), AppMeta { version: version.clone(), packaged: true }));
        let bridge: Arc<dyn AguBridge> = backend.clone();
        let net = Net::new(&version);
        init_cache(data_dir.clone());
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = State::new();
        state.rules = Rules::new(bridge.settings_get(), AccountsState::new());
        let txs = tx.clone();
        let seven = SevenSocket::new(&net, Arc::new(move |ev| {
            let _ = txs.send(Inbox::Seven(ev));
        }));
        let inner = Arc::new(Inner { bridge: bridge.clone(), backend, net: net.clone(), sink: sink.clone(), data_dir, st: Mutex::new(state), routes: Arc::new(Mutex::new(Routes::default())), inbox: tx.clone(), login: crate::login::LoginBridge::new(sink.clone()), seven, dev: Mutex::new(DevRelay::default()) });
        inner.login.install();

        // frames can arrive before `subscribe` answers with their id: hold them while a subscribe is in flight
        let routes = inner.routes.clone();
        let tx2 = tx.clone();
        std::mem::forget(bridge.on_frame(Arc::new(move |sub, frame| {
            let mut r = routes.lock().unwrap();
            if let Some(sk) = r.subs.get(sub) {
                let _ = tx2.send(Inbox::Frame { sk: sk.clone(), only: None, f: frame.clone() });
            } else if r.inflight > 0 {
                let list = r.pending.entry(sub.clone()).or_default();
                if list.len() < 4096 {
                    list.push(frame.clone());
                }
            }
        })));
        let (i2, s2) = (inner.clone(), sink.clone());
        std::mem::forget(bridge.on_accounts(Arc::new(move |a| {
            {
                let mut st = i2.st.lock().unwrap();
                let settings = st.rules.settings.clone();
                st.rules = Rules::new(settings, a.clone());
                st.redraw_all();
            }
            // a login brings the cheermotes (they need the account's token)
            let session = Session { inner: i2.clone() };
            let rt = i2.net.rt.handle().clone();
            rt.spawn(async move { session.refresh_account_data().await });
            s2(&"accounts", &serde_json::to_value(a).unwrap_or(Value::Null))
        })));
        let (i3, s3) = (inner.clone(), sink.clone());
        std::mem::forget(bridge.on_settings(Arc::new(move |s| {
            {
                let mut st = i3.st.lock().unwrap();
                let accounts = st.rules.accounts.clone();
                st.rules = Rules::new(s.clone(), accounts);
                st.settings_changed();
            }
            s3(&"settings", &serde_json::to_value(s).unwrap_or(Value::Null))
        })));

        let s4 = sink.clone();
        std::mem::forget(bridge.on_updater(Arc::new(move |u| s4(&"updater", &crate::settings_api::updater_json(u)))));

        let session = Session { inner };
        let s = session.clone();
        net.rt.spawn(async move { s.pump(rx).await });
        // the accounts are needed to tell your own lines and your name apart
        let s = session.clone();
        net.rt.spawn(async move {
            let a = s.inner.bridge.accounts_get().await;
            {
                let mut st = s.inner.st.lock().unwrap();
                let settings = st.rules.settings.clone();
                st.rules = Rules::new(settings, a);
                st.settings_changed();
            }
            s.refresh_account_data().await;
        });
        session
    }

    async fn pump(self, mut rx: mpsc::UnboundedReceiver<Inbox>) {
        let mut tick = tokio::time::interval(Duration::from_millis(FLUSH_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                m = rx.recv() => {
                    let Some(m) = m else { return };
                    self.take(m);
                    while let Ok(m) = rx.try_recv() {
                        self.take(m);
                    }
                }
                _ = tick.tick() => self.flush(),
            }
        }
    }

    fn emit(&self, out: Out) {
        for (kind, v) in out {
            (self.inner.sink)(kind, &v);
        }
    }

    fn take(&self, m: Inbox) {
        if let Inbox::Seven(ev) = m {
            self.on_seven(ev);
            return;
        }
        let mut out: Out = Vec::new();
        {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            match m {
                Inbox::Seven(_) => {}
                Inbox::Reset(sk) => {
                    for vk in st.hub.views_of(&sk) {
                        if let Some(v) = st.views.get_mut(&vk) {
                            v.store.reset_source(&sk);
                            v.dirty = true;
                            v.full = true;
                        }
                    }
                }
                Inbox::Frame { sk, only, f } => {
                    if f.get("type").and_then(|t| t.as_str()) == Some("conn") {
                        if only.is_none() {
                            // a replayed snapshot's conn is the one the app already has
                            out.push(("conn", json!({"sk": sk, "conn": f})));
                        }
                        st.src.entry(sk.clone()).or_default().conn = Some(f.clone());
                    }
                    let platform = st.hub.source(&sk).map(|s| s.platform);
                    let mut emotes_for: Vec<String> = Vec::new();
                    let mut kick_owner: Option<String> = None;
                    for vk in st.hub.views_of(&sk) {
                        if only.as_deref().is_some_and(|o| o != vk) {
                            continue;
                        }
                        let Some(v) = st.views.get_mut(&vk) else { continue };
                        let before = v.store.source(&sk).and_then(|s| s.channel_id.clone());
                        if v.store.apply(&sk, &f) {
                            v.dirty = true;
                        }
                        let after = v.store.source(&sk).and_then(|s| s.channel_id.clone());
                        if platform == Some(Platform::Twitch) && after.is_some() && after != before {
                            emotes_for.extend(after);
                        }
                        if platform == Some(Platform::Kick) && kick_owner.is_none() {
                            kick_owner = v.store.source(&sk).and_then(|s| s.status.as_ref()).and_then(kick_owner_id);
                        }
                    }
                    // Kick: the broadcaster's 7TV emote set, once the chat says whose it is
                    if let Some(id) = kick_owner {
                        let d = st.src.entry(sk.clone()).or_default();
                        if d.kick7.as_deref() != Some(id.as_str()) {
                            d.kick7 = Some(id.clone());
                            let s = self.clone();
                            let sk2 = sk.clone();
                            self.inner.net.rt.spawn(async move { s.load_kick_seven(sk2, id).await });
                        }
                    }
                    if let Some(cid) = emotes_for.into_iter().next() {
                        let d = st.src.entry(sk.clone()).or_default();
                        d.channel_id = Some(cid.clone());
                        if d.emotes.is_none() && !d.emotes_loading {
                            d.emotes_loading = true;
                            let s = self.clone();
                            self.inner.net.rt.spawn(async move { s.load_emotes(sk, cid, false).await });
                        }
                    }
                }
            }
        }
        self.emit(out);
    }

    fn flush(&self) {
        let mut out: Out = Vec::new();
        {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            let vks: Vec<String> = st.views.iter().filter(|(_, v)| v.dirty).map(|(k, _)| k.clone()).collect();
            for vk in vks {
                st.scan(&vk, &mut out);
                let State { views, rules, src, defs, book, .. } = st;
                let cx = Ctx { src, rules, defs: defs.clone(), book };
                if let Some(e) = views.get_mut(&vk).and_then(|v| v.diff(&vk, &cx)) {
                    out.push(("rows", e));
                }
            }
            st.mentions_event(&mut out);
        }
        self.emit(out);
        // chatters the rows just asked about
        self.pump_styles();
    }

    // ---- 7TV cosmetics -------------------------------------------------------------------------

    /// Start the lookups the style book has queued (at most three run at once).
    fn pump_styles(&self) {
        let book = self.inner.st.lock().unwrap().book.clone();
        while let Some(key) = book.next_job() {
            let s = self.clone();
            let client = self.inner.net.client.clone();
            let book = book.clone();
            self.inner.net.rt.spawn(async move {
                match book.finish(&key, fetch_chatter_style(&client, &key).await) {
                    None => {
                        if book.known(&key).is_some_and(|k| k.is_some()) {
                            s.inner.st.lock().unwrap().restyled(&key);
                        }
                    }
                    // a failed lookup is tried again after a wait, so a network blip does not take a paint away
                    Some(wait) => {
                        tokio::time::sleep(wait).await;
                        book.requeue(&key);
                    }
                }
                s.pump_styles();
            });
        }
    }

    /// The paint and badge definitions, fetched once (and again after a wait when that failed).
    fn ensure_defs(&self) {
        {
            let mut st = self.inner.st.lock().unwrap();
            if st.defs_requested || st.defs.is_some() || !st.rules.settings.seven_tv_cosmetics {
                return;
            }
            st.defs_requested = true;
        }
        let s = self.clone();
        let client = self.inner.net.client.clone();
        self.inner.net.rt.spawn(async move {
            match fetch_seven_tv_defs(&client).await {
                Some(raw) => {
                    let defs = defs_from_raw(&raw);
                    let mut st = s.inner.st.lock().unwrap();
                    st.defs = Some(Arc::new(defs));
                    st.redraw_all();
                }
                None => {
                    let wait = {
                        let mut st = s.inner.st.lock().unwrap();
                        st.defs_failures += 1;
                        retry_delay(st.defs_failures)
                    };
                    tokio::time::sleep(wait).await;
                    s.inner.st.lock().unwrap().defs_requested = false;
                    s.ensure_defs();
                }
            }
        });
    }

    // ---- what the message box needs ------------------------------------------------------------

    /// A login (or the start) brings what needs the account: cheermotes, what it may do in each chat, its own emotes.
    async fn refresh_account_data(&self) {
        self.load_cheermotes_all().await;
        let todo: Vec<(String, SourceRef, Option<String>)> = {
            let st = self.inner.st.lock().unwrap();
            st.src.iter().filter_map(|(sk, d)| st.hub.source(sk).filter(|s| s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_none()).map(|s| (sk.clone(), s, d.channel_id.clone()))).collect()
        };
        // what the account may do in every other open chat (a login or a logout changes it)
        let others: Vec<(String, SourceRef)> = {
            let st = self.inner.st.lock().unwrap();
            st.src.keys().filter_map(|sk| st.hub.source(sk).filter(|s| s.platform != Platform::Twitch).map(|s| (sk.clone(), s))).collect()
        };
        for (sk, source) in others {
            self.load_caps(sk, source).await;
        }
        for (sk, source, cid) in todo {
            self.load_caps(sk.clone(), source).await;
            // Twitch's emotes of the account are not those of a logged-out visitor
            if let Some(cid) = cid {
                self.load_picker(sk, cid, true).await;
            }
        }
        self.load_personal(false).await;
    }

    /// Twitch's own emotes for the picker and ":" (`fresh`: past the cache).
    async fn load_picker(&self, sk: String, channel_id: String, fresh: bool) {
        let Some(source) = self.inner.st.lock().unwrap().hub.source(&sk).filter(|s| s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_none()) else { return };
        let r = fetch_twitch_picker_emotes(&self.inner.bridge, &source, &channel_id, fresh).await;
        let mut st = self.inner.st.lock().unwrap();
        st.inv_gen += 1;
        if let Some(d) = st.src.get_mut(&sk) {
            d.twitch_emotes = r;
        }
    }

    /// The account's 7TV personal emotes, once per account.
    async fn load_personal(&self, reload: bool) {
        let id = {
            let mut st = self.inner.st.lock().unwrap();
            let id = st.me_twitch_id();
            if id.is_empty() || (!st.personal_requested.insert(id.clone()) && !reload) {
                return;
            }
            id
        };
        let list = fetch_seven_tv_personal(&self.inner.net.client, &id, reload).await;
        let mut st = self.inner.st.lock().unwrap();
        st.personal = list;
        st.inv_gen += 1;
    }

    // ---- cheermotes ----------------------------------------------------------------------------

    /// Bits cheermotes for every open Twitch channel that has none yet (they need the account's token, so a login
    /// brings them).
    async fn load_cheermotes_all(&self) {
        let todo: Vec<(String, SourceRef)> = {
            let mut st = self.inner.st.lock().unwrap();
            let mut v = Vec::new();
            let sks: Vec<String> = st.src.keys().cloned().collect();
            for sk in sks {
                let Some(source) = st.hub.source(&sk).filter(|s| s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_none()) else { continue };
                let d = st.src.get_mut(&sk).unwrap();
                if d.cheermotes.is_none() && !d.cheermotes_loading {
                    d.cheermotes_loading = true;
                    v.push((sk, source));
                }
            }
            v
        };
        for (sk, source) in todo {
            self.load_cheermotes(sk, source).await;
        }
    }

    async fn load_cheermotes(&self, sk: String, source: SourceRef) {
        let cm = load_cheermotes(&self.inner.bridge, &source, &sk).await;
        let mut st = self.inner.st.lock().unwrap();
        if let Some(d) = st.src.get_mut(&sk) {
            d.cheermotes_loading = false;
            if let Some(cm) = cm {
                d.cheermotes = Some(Arc::new(cm));
                st.touch_sk(&sk);
            }
        }
    }

    // ---- live 7TV changes ----------------------------------------------------------------------

    /// Listen for live changes to a channel's 7TV set (and to which set it uses); a channel with none stops listening.
    fn watch_seven(&self, st: &mut State, sk: &str, ids: Option<SevenTvIds>) {
        let d = st.src.entry(sk.to_string()).or_default();
        if d.seven_ids == ids {
            return;
        }
        let old = std::mem::replace(&mut d.seven_ids, ids.clone());
        let sock = &self.inner.seven;
        if let Some(o) = old {
            sock.unsubscribe(EMOTE_SET_UPDATE, &o.set_id);
            sock.unsubscribe(USER_UPDATE, &o.user_id);
        }
        if let Some(n) = ids {
            sock.subscribe(EMOTE_SET_UPDATE, &n.set_id);
            sock.subscribe(USER_UPDATE, &n.user_id);
        }
    }

    /// A channel's data goes (its last view closed): stop listening for its 7TV changes.
    fn forget_src(&self, d: SrcData) {
        if let Some(o) = d.seven_ids {
            self.inner.seven.unsubscribe(EMOTE_SET_UPDATE, &o.set_id);
            self.inner.seven.unsubscribe(USER_UPDATE, &o.user_id);
        }
    }

    /// The app's own notice of a 7TV change, one compact line in the channel's chat.
    fn seven_notice(&self, sk: &str, note: Value) {
        let (ts, seq) = {
            let mut st = self.inner.st.lock().unwrap();
            st.seven_seq += 1;
            (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0), st.seven_seq)
        };
        let s = |k: &str| note.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let system = match s("action").as_str() {
            "renamed" => format!("{} renamed the 7TV emote {} to {}", s("actor"), s("oldName"), s("name")),
            "set" => format!("{} switched the channel's 7TV emote set to {}", s("actor"), s("name")),
            a => format!("{} {a} the 7TV emote {}", s("actor"), s("name")),
        };
        let notice = json!({"id": format!("7tv-{ts}-{seq}"), "ts": ts, "msgId": "seventv", "system": system.trim(), "seventv": note});
        let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.to_string(), only: None, f: json!({"type": "chatnotice", "notice": notice}) });
    }

    fn on_seven(&self, ev: SevenEvent) {
        match ev {
            SevenEvent::Dispatch(Dispatch::EmoteSet { set_id, actor, changes }) => {
                if changes.is_empty() {
                    return;
                }
                let sks: Vec<(String, Option<(SevenTvPlatform, String)>)> = {
                    let mut guard = self.inner.st.lock().unwrap();
                    let st = &mut *guard;
                    let settings = st.rules.settings.clone();
                    let mut hit = Vec::new();
                    for (sk, d) in st.src.iter_mut() {
                        if !d.seven_ids.as_ref().is_some_and(|i| same_id(&i.set_id, &set_id)) {
                            continue;
                        }
                        if let Some(sets) = d.sets.as_mut() {
                            apply_changes(&mut Arc::make_mut(sets).seven.channel, &changes);
                            d.emotes = Some(Arc::new(merge_for(sets, &settings)));
                        }
                        hit.push((sk.clone(), d.channel_id.clone().map(|c| (SevenTvPlatform::Twitch, c)).or_else(|| d.kick7.clone().map(|k| (SevenTvPlatform::Kick, k)))));
                    }
                    for (sk, _) in &hit {
                        st.touch_sk(sk);
                    }
                    hit
                };
                for (sk, cid) in sks {
                    // a restart must not bring the old set back from the cache
                    if let Some((platform, cid)) = cid {
                        forget_cache(&seven_tv_channel_key_on(platform, &cid));
                        self.inner.net.rt.spawn(async { flush_cache() });
                    }
                    for c in &changes {
                        let (action, e, old_name) = match c {
                            EmoteChange::Added(e) => ("added", e, ""),
                            EmoteChange::Removed(e) => ("removed", e, ""),
                            EmoteChange::Updated { old, new } if old.name != new.name => ("renamed", new, old.name.as_str()),
                            // only its flags changed: nothing to tell
                            EmoteChange::Updated { .. } => continue,
                        };
                        self.seven_notice(&sk, json!({"actor": actor, "action": action, "emoteId": e.id, "name": e.name, "oldName": old_name}));
                    }
                }
            }
            SevenEvent::Dispatch(Dispatch::UserSet { user_id, actor, set_id, set_name }) => {
                let sks: Vec<String> = {
                    let st = self.inner.st.lock().unwrap();
                    st.src.iter().filter(|(_, d)| d.seven_ids.as_ref().is_some_and(|i| same_id(&i.user_id, &user_id) && !same_id(&i.set_id, &set_id))).map(|(k, _)| k.clone()).collect()
                };
                for sk in sks {
                    let s = self.clone();
                    let client = self.inner.net.client.clone();
                    let (user_id, set_id, actor) = (user_id.clone(), set_id.clone(), actor.clone());
                    let name = if set_name.is_empty() { "another set".to_string() } else { set_name.clone() };
                    self.inner.net.rt.spawn(async move {
                        let Some(map) = fetch_seven_tv_set(&client, &set_id).await else { return };
                        {
                            let mut guard = s.inner.st.lock().unwrap();
                            let st = &mut *guard;
                            let settings = st.rules.settings.clone();
                            if let Some(d) = st.src.get_mut(&sk) {
                                if let Some(sets) = d.sets.as_mut() {
                                    Arc::make_mut(sets).seven.channel = map;
                                    d.emotes = Some(Arc::new(merge_for(sets, &settings)));
                                }
                            }
                            s.watch_seven(st, &sk, Some(SevenTvIds { user_id, set_id }));
                            st.touch_sk(&sk);
                        }
                        s.seven_notice(&sk, json!({"actor": actor, "action": "set", "name": name}));
                    });
                }
            }
            SevenEvent::Dispatch(Dispatch::Other) => {}
            SevenEvent::Resynced => {
                // the socket was down for a while: whatever changed meanwhile comes with a fresh load
                let all: Vec<(String, SevenTvPlatform, String)> = {
                    let st = self.inner.st.lock().unwrap();
                    st.src.iter().filter(|(_, d)| d.seven_ids.is_some()).filter_map(|(sk, d)| d.channel_id.clone().map(|c| (sk.clone(), SevenTvPlatform::Twitch, c)).or_else(|| d.kick7.clone().map(|k| (sk.clone(), SevenTvPlatform::Kick, k)))).collect()
                };
                for (sk, platform, cid) in all {
                    let s = self.clone();
                    let client = self.inner.net.client.clone();
                    self.inner.net.rt.spawn(async move {
                        let (seven, ids) = reload_seven_tv(client, platform, cid).await;
                        let mut guard = s.inner.st.lock().unwrap();
                        let st = &mut *guard;
                        let settings = st.rules.settings.clone();
                        if let Some(d) = st.src.get_mut(&sk) {
                            if let Some(sets) = d.sets.as_mut() {
                                let sets = Arc::make_mut(sets);
                                sets.seven.channel = seven.channel;
                                if !seven.global.is_empty() {
                                    sets.seven.global = seven.global;
                                }
                                d.emotes = Some(Arc::new(merge_for(sets, &settings)));
                            }
                        }
                        if ids.is_some() {
                            s.watch_seven(st, &sk, ids);
                        }
                        st.touch_sk(&sk);
                    });
                }
            }
        }
    }

    // ---- link previews -------------------------------------------------------------------------

    fn now_ms() -> u64 {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        T0.get_or_init(Instant::now).elapsed().as_millis() as u64
    }

    fn preview_json(key: &str, p: &Preview) -> Value {
        json!({"key": key, "provider": p.provider.label(), "title": p.title, "subtitle": p.subtitle, "thumb": p.thumb})
    }

    /// The preview card of a link: the answer when it is in; otherwise `null`, and the request is queued (a
    /// `preview` event brings the answer).
    fn preview_get(&self, href: &str) -> Value {
        let Some(t) = match_url(href) else { return Value::Null };
        let got = self.inner.st.lock().unwrap().previews.get(&t, Self::now_ms());
        match got {
            Some(p) => Self::preview_json(&t.key, &p),
            None => {
                self.pump_previews();
                Value::Null
            }
        }
    }

    /// Start the queued requests while fewer than four run.
    fn pump_previews(&self) {
        loop {
            let Some(job) = self.inner.st.lock().unwrap().previews.next_job() else { return };
            let s = self.clone();
            let client = self.inner.net.client.clone();
            self.inner.net.rt.spawn(async move {
                let key = job.key.clone();
                let res = agu_chat_core::link_preview::fetch::fetch(&job, &client).await;
                let got = {
                    let mut st = s.inner.st.lock().unwrap();
                    let ok = st.previews.finish(&key, res, Self::now_ms());
                    if ok {
                        st.previews.peek(&key)
                    } else {
                        None
                    }
                };
                if let Some(p) = got {
                    (s.inner.sink)("preview", &Self::preview_json(&key, &p));
                }
                s.pump_previews();
            });
        }
    }

    // ---- emotes and badges ---------------------------------------------------------------------

    async fn load_badges_for(&self, sk: String, source: SourceRef) {
        let Some((map, cid)) = load_badges(&self.inner.bridge, &source, &sk).await else { return };
        let start_emotes = {
            let mut st = self.inner.st.lock().unwrap();
            let d = st.src.entry(sk.clone()).or_default();
            d.badges = Arc::new(map);
            if cid.is_some() {
                d.channel_id = cid.clone();
            }
            let go = cid.is_some() && d.emotes.is_none() && !d.emotes_loading;
            if go {
                d.emotes_loading = true;
            }
            st.touch_sk(&sk);
            cid.filter(|_| go)
        };
        if whisper::login_of(&source.channel).is_none() {
            let mut todo = false;
            {
                let mut st = self.inner.st.lock().unwrap();
                if let Some(d) = st.src.get_mut(&sk) {
                    if d.cheermotes.is_none() && !d.cheermotes_loading {
                        d.cheermotes_loading = true;
                        todo = true;
                    }
                }
            }
            if todo {
                let s = self.clone();
                let (sk2, src2) = (sk.clone(), source.clone());
                self.inner.net.rt.spawn(async move { s.load_cheermotes(sk2, src2).await });
            }
            let s = self.clone();
            let (sk2, src2) = (sk.clone(), source.clone());
            self.inner.net.rt.spawn(async move { s.load_caps(sk2, src2).await });
        }
        if let Some(cid) = start_emotes {
            self.load_emotes(sk, cid, false).await;
        }
    }

    fn apply_sets(&self, sk: &str, sets: &ProviderSets) {
        let mut guard = self.inner.st.lock().unwrap();
        let st = &mut *guard;
        if !st.hub.is_held(sk) {
            return;
        }
        let merged = merge_for(sets, &st.rules.settings);
        let d = st.src.entry(sk.to_string()).or_default();
        d.sets = Some(Arc::new(sets.clone()));
        d.emotes = Some(Arc::new(merged));
        let ids = sets.seven_ids.clone();
        self.watch_seven(st, sk, ids);
        st.touch_sk(sk);
    }

    /// 7TV for a Kick channel (its broadcaster's set; BTTV and FFZ have none there).
    async fn load_kick_seven(&self, sk: String, kick_user_id: String) {
        let sets = load_kick_provider_sets(self.inner.net.client.clone(), kick_user_id, false).await;
        self.apply_sets(&sk, &sets);
    }

    /// 7TV / BTTV / FFZ for a Twitch channel: the cached sets at once, then fresh ones.
    async fn load_emotes(&self, sk: String, channel_id: String, reload: bool) {
        let client = self.inner.net.client.clone();
        {
            let s = self.clone();
            let (sk2, cid2) = (sk.clone(), channel_id.clone());
            self.inner.net.rt.spawn(async move { s.load_picker(sk2, cid2, reload).await });
        }
        if !reload {
            if let Some(sets) = load_cached_provider_sets(client.clone(), channel_id.clone()).await {
                self.apply_sets(&sk, &sets);
            }
        }
        let (sets, _fresh) = load_provider_sets(client, channel_id, reload).await;
        self.apply_sets(&sk, &sets);
        flush_cache();
        if let Some(d) = self.inner.st.lock().unwrap().src.get_mut(&sk) {
            d.emotes_loading = false;
        }
    }

    /// Subscribe a source nobody held, for good: its frames go to every view that holds it.
    async fn subscribe_new(&self, source: SourceRef) -> Result<(), String> {
        let sk = source_key(&source);
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source.clone()).await;
        {
            let mut r = self.inner.routes.lock().unwrap();
            r.inflight -= 1;
            if let Ok(id) = &res {
                r.subs.insert(id.clone(), sk.clone());
                let _ = self.inner.inbox.send(Inbox::Reset(sk.clone()));
                for f in r.pending.remove(id).unwrap_or_default() {
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: None, f });
                }
            }
            if r.inflight == 0 {
                r.pending.clear();
            }
        }
        let id = res?;
        // every view that held it may have closed meanwhile
        let held = {
            let mut st = self.inner.st.lock().unwrap();
            let held = st.hub.is_held(&sk);
            if held {
                st.hub.set_sub(&sk, id.clone());
            }
            held
        };
        if !held {
            self.end_sub(Some(id)).await;
            return Ok(());
        }
        if source.platform == Platform::Twitch {
            let s = self.clone();
            self.inner.net.rt.spawn(async move { s.load_badges_for(sk, source).await });
        } else {
            // what the account may do here
            let s = self.clone();
            self.inner.net.rt.spawn(async move { s.load_caps(sk, source).await });
        }
        Ok(())
    }

    /// A view opened on a channel another view already holds: replay its current state into that view only. The
    /// replay is a short-lived second subscription (the core sends the snapshot to each new subscriber).
    async fn replay_into(&self, source: SourceRef, vk: String) -> Result<(), String> {
        let sk = source_key(&source);
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source).await;
        {
            let mut r = self.inner.routes.lock().unwrap();
            r.inflight -= 1;
            if let Ok(id) = &res {
                for f in r.pending.remove(id).unwrap_or_default() {
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: Some(vk.clone()), f });
                }
            }
            if r.inflight == 0 {
                r.pending.clear();
            }
        }
        let id = res?;
        self.end_sub(Some(id)).await;
        Ok(())
    }

    async fn end_sub(&self, id: Option<String>) {
        if let Some(id) = id {
            self.inner.routes.lock().unwrap().subs.remove(&id);
            self.inner.bridge.unsubscribe(id).await;
        }
    }

    /// Make a view hold exactly `channels` (creating it when `vk` is new). Returns `{vk, sks}`. A channel that
    /// cannot be opened fails the call and is left out, unless `lenient` (restoring saved tabs): then it stays and
    /// shows its error as its connection state.
    async fn set_view(&self, vk: Option<String>, channels: Vec<SourceRef>, lenient: bool) -> Result<Value, String> {
        let vk = vk.filter(|v| !v.is_empty()).unwrap_or_else(|| format!("v{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
        let (change, before, out) = {
            let mut guard = self.inner.st.lock().unwrap();
            let st = &mut *guard;
            let before = st.hub.sources_of(&vk);
            let change = st.hub.set_view(&vk, &channels);
            let sources = st.hub.sources_of(&vk);
            let view = st.views.entry(vk.clone()).or_insert_with(View::new);
            view.store.sync(&sources);
            view.dirty = true;
            view.full = true;
            let mut out: Out = Vec::new();
            let mut gone: Vec<SrcData> = Vec::new();
            for (sk, _) in &change.released {
                gone.extend(st.src.remove(sk));
            }
            for d in gone {
                self.forget_src(d);
            }
            for a in &change.added {
                if let Added::Held(s) = a {
                    // another view already has the channel: the app already knows its connection
                    if let Some(c) = st.src.get(&source_key(s)).and_then(|d| d.conn.clone()) {
                        out.push(("conn", json!({"sk": source_key(s), "conn": c})));
                    }
                }
            }
            (change, before, out)
        };
        self.emit(out);
        for (_, sub) in &change.released {
            self.end_sub(sub.clone()).await;
        }
        let mut failed: Vec<(String, String)> = Vec::new();
        for a in &change.added {
            let res = match a {
                Added::New(s) => self.subscribe_new(s.clone()).await,
                Added::Held(s) => self.replay_into(s.clone(), vk.clone()).await,
            };
            if let Err(e) = res {
                failed.push((source_key(a.source()), e));
            }
        }
        if !failed.is_empty() {
            if lenient {
                for (sk, e) in &failed {
                    let f = json!({"type": "conn", "state": "error", "detail": e, "canSend": false, "readOnlyReason": e});
                    let _ = self.inner.inbox.send(Inbox::Frame { sk: sk.clone(), only: None, f });
                }
            } else {
                // leave the view as it was (a new view is not created)
                if before.is_empty() {
                    self.close_view(&vk).await;
                } else {
                    let _ = Box::pin(self.set_view(Some(vk.clone()), before, true)).await;
                }
                return Err(failed.remove(0).1);
            }
        }
        let sks: Vec<String> = self.inner.st.lock().unwrap().hub.sources_of(&vk).iter().map(source_key).collect();
        Ok(json!({"vk": vk, "sks": sks}))
    }

    async fn close_view(&self, vk: &str) {
        let released = {
            let mut st = self.inner.st.lock().unwrap();
            st.views.remove(vk);
            let r = st.hub.close_view(vk);
            for (sk, _) in &r {
                if let Some(d) = st.src.remove(sk) {
                    self.forget_src(d);
                }
            }
            r
        };
        for (_, sub) in released {
            self.end_sub(sub).await;
        }
    }

    fn source_of(&self, sk: &str) -> Result<SourceRef, String> {
        self.inner.st.lock().unwrap().hub.source(sk).ok_or_else(|| "that chat is not open".to_string())
    }

    async fn send(&self, sk: &str, text: &str, reply_to: Option<&str>) -> Result<Value, String> {
        let source = self.source_of(sk)?;
        let path = match source.platform {
            Platform::Twitch => "/api/chat/send",
            Platform::Rumble => "/api/rumble/send",
            Platform::Kick => "/api/kick/send",
            Platform::X => "/api/x/send",
            Platform::Youtube => "/api/youtube/send",
        };
        let mut body = json!({"text": text});
        if let Some(r) = reply_to.filter(|r| !r.is_empty()) {
            body["replyTo"] = json!(r);
        }
        let r = relay_api(&self.inner.bridge, &source, path, Some("POST"), Some(body)).await;
        if r.ok {
            return Ok(json!({"ok": true}));
        }
        let msg = match &r.body {
            Value::Object(o) => o.get("error").map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_else(|| e.to_string())).unwrap_or_default(),
            Value::String(s) => s.clone(),
            _ => String::new(),
        };
        Err(if r.status == 403 && msg == "relogin" {
            "Log in again to grant chat permissions".into()
        } else if msg.is_empty() {
            "send failed".into()
        } else {
            msg
        })
    }

    // ---- the message box -----------------------------------------------------------------------

    /// `{vk, sks, text, caret}` (caret in UTF-16 units) -> the strip's suggestions, or null.
    fn compose_suggest(&self, args: &Value) -> Result<Value, String> {
        let (vk, sks) = (str_arg(args, "vk")?, sks_arg(args));
        let text = str_arg(args, "text")?;
        let caret = byte_of(text, args.get("caret").and_then(|c| c.as_u64()).unwrap_or(text.encode_utf16().count() as u64) as usize);
        let mut guard = self.inner.st.lock().unwrap();
        let st = &mut *guard;
        let inv = st.merged_inventory(&sks);
        let single = (sks.len() == 1).then(|| st.hub.source(&sks[0])).flatten();
        let slash_ok = single.as_ref().is_some_and(|s| s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_none());
        let caps = if slash_ok { st.src.get(&sks[0]).map(|d| d.caps.clone()) } else { None };
        let rows = st.views.get(vk).map(|v| v.store.rows());
        let names = || rows.map(|r| chatters(r, &sks)).unwrap_or_default();
        let cx = ChangeCtx { inventory: &inv.inventory, slash_ok, twitch_caps: caps.as_ref(), names: &names };
        Ok(suggest(text, caret, &cx).map(|r| suggestions_json(text, &r)).unwrap_or(Value::Null))
    }

    /// `{sks, tab?, q?}` -> the emote picker: `{tabs, sections}`.
    fn compose_picker(&self, args: &Value) -> Result<Value, String> {
        let sks = sks_arg(args);
        let inv = self.inner.st.lock().unwrap().merged_inventory(&sks);
        Ok(picker_json(&inv, args.get("tab").and_then(|t| t.as_str()).unwrap_or(""), args.get("q").and_then(|t| t.as_str()).unwrap_or("")))
    }

    /// `{vk, sks}` -> `{sk: {level, text}}` for the chats whose modes concern this account.
    fn compose_gates(&self, args: &Value) -> Value {
        let st = self.inner.st.lock().unwrap();
        let mut out = serde_json::Map::new();
        for sk in sks_arg(args) {
            if let Some(g) = st.gate_of(&sk) {
                out.insert(sk, g);
            }
        }
        Value::Object(out)
    }

    /// `{vk, sks, text, reply?: {sk, id}}`: what Send does. Answers `{clear, toast?, warn, confirm?}`: `clear` empties
    /// the box, `confirm` asks first (`/clear`, `/raid`; answered with `compose.confirm`).
    async fn compose_send(&self, args: &Value) -> Result<Value, String> {
        let (vk, text) = (str_arg(args, "vk")?, str_arg(args, "text")?);
        let sks = sks_arg(args);
        let reply = args.get("reply").filter(|r| r.is_object()).and_then(|r| Some((r.get("sk")?.as_str()?.to_string(), r.get("id")?.as_str()?.to_string())));
        let (targets, last, pinned) = {
            let st = self.inner.st.lock().unwrap();
            let mut targets = Vec::new();
            for sk in &sks {
                let src = st.hub.source(sk).ok_or("that chat is not open")?;
                targets.push(Target { sk: sk.clone(), platform: src.platform, whisper: src.platform == Platform::Twitch && whisper::login_of(&src.channel).is_some() });
            }
            let (last, pinned) = if targets.len() == 1 { st.message_ids(vk, &targets[0].sk) } else { (None, None) };
            (targets, last, pinned)
        };
        let toast = |t: String, warn: bool, clear: bool| json!({"clear": clear, "toast": t, "warn": warn});
        match plan_send(text, &targets, last.as_deref(), pinned.as_deref()) {
            SendPlan::Nothing => Ok(json!({"clear": false})),
            SendPlan::Refuse(t) => Ok(toast(t, true, false)),
            SendPlan::Confirm { target, what, text } => Ok(match what {
                Confirmed::Clear => json!({"clear": false, "confirm": {"kind": "clear", "sk": target, "text": text}}),
                Confirmed::Raid { login } => json!({"clear": false, "confirm": {"kind": "raid", "sk": target, "login": login, "text": text}}),
            }),
            SendPlan::Slash { target, plan } => {
                let source = self.source_of(&target)?;
                match post_json(&self.inner.bridge, &source, plan.path, plan.body, plan.method).await {
                    Ok(_) => Ok(toast(plan.ok, false, true)),
                    Err(e) => Ok(toast(e, true, false)),
                }
            }
            SendPlan::Plain { text, targets: sks } => {
                let multi = sks.len() > 1;
                let sends = sks.iter().map(|sk| {
                    let reply_to = reply.as_ref().filter(|(rsk, _)| rsk == sk).map(|(_, id)| id.clone());
                    let text = &text;
                    async move {
                        let platform = self.inner.st.lock().unwrap().hub.source(sk).map(|s| s.platform);
                        (platform, self.send(sk, text, reply_to.as_deref()).await.err())
                    }
                });
                // every chat at once, each its own request; one failing never loses what reached another
                let outcomes: Vec<(Platform, Option<String>)> = futures::future::join_all(sends).await.into_iter().filter_map(|(p, e)| p.map(|p| (p, e))).collect();
                let failures = send_failures(outcomes, multi);
                Ok(if failures.is_empty() { json!({"clear": true}) } else { toast(failures.join(" - "), true, false) })
            }
        }
    }

    /// `{sk, kind: "clear" | "raid", login?}`: the confirmed command. Answers `{toast, warn}`.
    async fn compose_confirm(&self, args: &Value) -> Result<Value, String> {
        let source = self.source_of(str_arg(args, "sk")?)?;
        let what = match str_arg(args, "kind")? {
            "clear" => Confirmed::Clear,
            "raid" => Confirmed::Raid { login: str_arg(args, "login")?.to_string() },
            _ => return Err("unknown command".into()),
        };
        Ok(match confirmed_command(&self.inner.bridge, &source, &what).await {
            Ok(Some((t, warn))) => json!({"toast": t, "warn": warn, "clear": true}),
            Ok(None) => json!({"clear": true}),
            Err(e) => json!({"toast": e, "warn": true, "clear": false}),
        })
    }

    /// The Mentions screen's list, newest first.
    fn mentions_list(&self) -> Value {
        let st = self.inner.st.lock().unwrap();
        let list: Vec<Value> = st
            .mentions
            .entries()
            .iter()
            .rev()
            .map(|e| {
                let cx = Ctx { src: &st.src, rules: &st.rules, defs: st.defs.clone(), book: &st.book };
                let env = env_of(&e.row.sk, &cx, None);
                let mut row = row_json(&e.row, &env);
                st.rules.decorate(&mut row, &e.row);
                json!({"id": e.id, "vk": e.leaf_id, "sk": e.row.sk, "channel": e.channel, "platform": e.row.platform.as_str(), "ts": e.row.ts, "read": e.read, "whisper": agu_chat_model::mentions::is_whisper_key(&e.row.sk), "row": row})
            })
            .collect();
        Value::Array(list)
    }

    pub fn call(&self, method: String, args: Value) -> BoxFuture<'static, Result<Value, String>> {
        let s = self.clone();
        Box::pin(async move {
            let b = &s.inner.bridge;
            match method.as_str() {
                "app.info" => Ok(serde_json::to_value(b.app_info()).unwrap_or(Value::Null)),
                // the app is on screen or in the background
                "app.active" => {
                    s.inner.st.lock().unwrap().active = args.get("active").and_then(|a| a.as_bool()).unwrap_or(true);
                    Ok(Value::Null)
                }
                "accounts.get" => Ok(serde_json::to_value(b.accounts_get().await).unwrap_or(Value::Null)),
                "accounts.login" => b.accounts_login(platform_of(&args)?, args.get("args").cloned().unwrap_or(json!({}))).await,
                "accounts.cancel" => {
                    b.accounts_cancel(platform_of(&args)?).await;
                    Ok(Value::Null)
                }
                "accounts.logout" => {
                    b.accounts_logout(platform_of(&args)?).await;
                    Ok(Value::Null)
                }
                // the sign-in screen's answer to a `login.webview` / `login.show` event
                "login.result" => s.inner.login.answer(&args).map(|_| Value::Null),
                "settings.get" => Ok(serde_json::to_value(b.settings_get()).unwrap_or(Value::Null)),
                "settings.set" => Ok(serde_json::to_value(b.settings_set(args.get("patch").cloned().unwrap_or(json!({}))).await).unwrap_or(Value::Null)),
                "tabs.get" => Ok(migrate_tabs(std::fs::read(s.inner.data_dir.join("mobile-tabs.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!([])))),
                "tabs.set" => {
                    let tabs = migrate_tabs(args.get("tabs").cloned().unwrap_or(json!([])));
                    let tmp = s.inner.data_dir.join("mobile-tabs.json.tmp");
                    std::fs::write(&tmp, serde_json::to_vec(&tabs).unwrap_or_default()).map_err(|e| e.to_string())?;
                    std::fs::rename(&tmp, s.inner.data_dir.join("mobile-tabs.json")).map_err(|e| e.to_string())?;
                    Ok(Value::Null)
                }
                // {vk?, channels: [{platform, channel}], lenient?} -> {vk, sks}: open a view, or change what it holds
                "view.open" | "view.set" => {
                    let vk = args.get("vk").and_then(|v| v.as_str()).map(|v| v.to_string());
                    if method == "view.set" && !vk.as_deref().is_some_and(|v| s.inner.st.lock().unwrap().hub.has_view(v)) {
                        return Err("that tab is not open".into());
                    }
                    let lenient = args.get("lenient").and_then(|l| l.as_bool()).unwrap_or(false);
                    s.ensure_defs();
                    s.set_view(vk, channels_arg(&args)?, lenient).await
                }
                // {vk, query}: show only the lines of a tab that match (an empty query shows them all)
                "view.search" => {
                    let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
                    let q = parse_search(query);
                    let mut st = s.inner.st.lock().unwrap();
                    let v = st.views.get_mut(str_arg(&args, "vk")?).ok_or("that tab is not open")?;
                    v.search = if q.from.is_empty() && q.words.is_empty() { None } else { Some(q) };
                    v.dirty = true;
                    v.full = true;
                    Ok(Value::Null)
                }
                // {href}: the preview card of a link (a clip, a YouTube video, a post on X, ...) or null while it loads;
                // the `preview` event brings it
                "preview.get" => Ok(s.preview_get(str_arg(&args, "href")?)),
                "view.close" => {
                    s.close_view(str_arg(&args, "vk")?).await;
                    Ok(Value::Null)
                }
                "chat.send" => s.send(str_arg(&args, "sk")?, str_arg(&args, "text")?, args.get("replyTo").and_then(|r| r.as_str())).await,
                "compose.suggest" => s.compose_suggest(&args),
                "compose.picker" => s.compose_picker(&args),
                "compose.gates" => Ok(s.compose_gates(&args)),
                "compose.send" => s.compose_send(&args).await,
                "compose.confirm" => s.compose_confirm(&args).await,
                "chat.reloadEmotes" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let cid = {
                        let st = s.inner.st.lock().unwrap();
                        st.hub.views_of(&sk).iter().find_map(|vk| st.views.get(vk).and_then(|v| v.store.source(&sk).and_then(|x| x.channel_id.clone())))
                    };
                    if let Some(cid) = cid {
                        s.load_emotes(sk, cid, true).await;
                    }
                    Ok(Value::Null)
                }
                // the Mentions screen: opening it marks everything read (the bell's count goes), closing it ends that
                "mentions.open" => {
                    let open = args.get("open").and_then(|o| o.as_bool()).unwrap_or(true);
                    let mut out: Out = Vec::new();
                    {
                        let mut st = s.inner.st.lock().unwrap();
                        st.mentions_open = open;
                        if open {
                            st.mentions.mark_all_read();
                        }
                        st.mentions_event(&mut out);
                    }
                    s.emit(out);
                    Ok(Value::Null)
                }
                "mentions.list" => Ok(s.mentions_list()),
                // the built-in alert sound: 16-bit mono PCM
                // Settings screens
                "settings.meta" => Ok(crate::settings_api::meta(&b.app_info().version)),
                "settings.check" => Ok(crate::settings_api::check(&args)),
                "modbtn.duration" => Ok(crate::settings_api::duration(&args)),
                "highlights.test" => Ok(crate::settings_api::highlight_test(&b.settings_get(), &args)),
                "sounds.label" => Ok(json!(crate::settings_api::sound_name(str_arg(&args, "file")?))),
                // a file the app copied out of the phone's file picker: into the sounds folder, by name
                "sounds.import" => {
                    let file = b.sounds_import(PathBuf::from(str_arg(&args, "path")?)).await?;
                    Ok(json!({ "file": file }))
                }
                // where a stored sound is (the app plays it); only bare names inside the sounds folder
                "sounds.path" => {
                    let file = str_arg(&args, "file")?;
                    if !agu_chat_core::highlight_rules::is_sound_file_name(file) {
                        return Err("Not a sound file.".into());
                    }
                    let p = s.inner.backend.sounds_dir().join(file);
                    if p.is_file() { Ok(json!({ "path": p.to_string_lossy() })) } else { Err("That sound is gone.".into()) }
                }
                "updater.get" => Ok(crate::settings_api::updater_json(&b.updater_get())),
                "updater.check" => {
                    b.updater_check().await;
                    Ok(Value::Null)
                }
                "updater.download" => {
                    s.inner.backend.updater_download();
                    Ok(Value::Null)
                }
                // the verified apk, for the system installer (the app hands it over; nothing is installed here)
                "updater.install" => match s.inner.backend.updater_ready_path() {
                    Some(p) => Ok(json!({ "path": p.to_string_lossy() })),
                    None => Err("No update is ready to install.".into()),
                },
                "alerts.ping" => {
                    let rate = 44100u32;
                    let pcm: Vec<i32> = ping_samples(rate).into_iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i32).collect();
                    Ok(json!({"rate": rate, "pcm": pcm}))
                }
                // the relay's whole API (moderation, user cards, ...): {sk, path, method?, body?}
                "api" => {
                    let source = s.source_of(str_arg(&args, "sk")?)?;
                    let r = relay_api(b, &source, str_arg(&args, "path")?, args.get("method").and_then(|m| m.as_str()), args.get("body").cloned()).await;
                    Ok(json!({"ok": r.ok, "status": r.status, "body": r.body}))
                }
                // debuggable builds only (the app checks): feed a frame to an open chat as if the platform sent it,
                // to see real rows without a busy channel
                "mod.action" => s.mod_action(&args).await,
                "mod.command" => s.mod_command(&args).await,
                "mod.automod" => s.mod_automod(&args).await,
                "mod.rules" => s.mod_rules(&args).await,
                "caps.get" => {
                    let st = s.inner.st.lock().unwrap();
                    Ok(st.caps_event(str_arg(&args, "sk")?))
                }
                "card.info" => s.card_info(&args).await,
                "card.messages" => s.card_messages(&args).await,
                "card.comment" => s.card_comment(&args, false).await,
                "card.comment.delete" => s.card_comment(&args, true).await,
                "card.ignore" => s.card_ignore(&args).await,
                "modes.get" => s.modes_get(&args).await,
                "modes.patch" => s.modes_patch(&args).await,
                "modes.shield" => s.modes_shield(&args).await,
                "terms.list" => s.terms(&args, "list").await,
                "terms.add" => s.terms(&args, "add").await,
                "terms.remove" => s.terms(&args, "remove").await,
                "dev.caps" => s.dev_caps(&args),
                "dev.relay" => s.dev_relay(&args),
                "dev.frame" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let frame = args.get("frame").cloned().ok_or("missing frame")?;
                    let _ = s.inner.inbox.send(Inbox::Frame { sk, only: None, f: frame });
                    Ok(Value::Null)
                }
                // debuggable builds only: pretend this Twitch account is logged in (to see your name highlighted)
                "dev.me" => {
                    let login = str_arg(&args, "login")?.to_string();
                    let mut st = s.inner.st.lock().unwrap();
                    let mut accounts = st.rules.accounts.clone();
                    if let Ok(a) = serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": login, "name": login}})) {
                        accounts.insert(Platform::Twitch, a);
                    }
                    let settings = st.rules.settings.clone();
                    st.rules = Rules::new(settings, accounts);
                    st.redraw_all();
                    Ok(Value::Null)
                }
                // debuggable builds only: stand in for 7TV's cosmetics, `{defs: {paints, badges}, users: [{id, paint_id?, badge_id?}]}`
                "dev.cosmetics" => {
                    let mut st = s.inner.st.lock().unwrap();
                    if let Some(d) = args.get("defs") {
                        st.defs = Some(Arc::new(defs_from_raw(d)));
                        st.defs_requested = true;
                    }
                    let book = st.book.clone();
                    for u in args.get("users").and_then(|u| u.as_array()).into_iter().flatten() {
                        let id = u.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let opt = |k: &str| u.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
                        book.want(&id);
                        while let Some(k) = book.next_job() {
                            let style = (k == id).then(|| agu_chat_model::twitch::cosmetics::ChatterStyle { paint_id: opt("paint_id"), badge_id: opt("badge_id"), ..Default::default() });
                            book.finish(&k, Some(style));
                        }
                        st.restyled(&id);
                    }
                    st.redraw_all();
                    Ok(Value::Null)
                }
                // debuggable builds only: stand in for the relay's Twitch emotes of the account, `{sk, channel: [{id, name, url}], global: [...]}`
                "dev.emotes" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let list = |k: &str| -> Vec<TwitchPickerEmote> {
                        let s = |e: &Value, k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
                        args.get(k).and_then(|a| a.as_array()).map(|a| a.iter().map(|e| TwitchPickerEmote { id: s(e, "id"), name: s(e, "name"), url: s(e, "url") }).collect()).unwrap_or_default()
                    };
                    let mut st = s.inner.st.lock().unwrap();
                    st.src.entry(sk).or_default().twitch_emotes = TwitchEmotesResp { channel: list("channel"), global: list("global"), other: Vec::new() };
                    st.inv_gen += 1;
                    Ok(Value::Null)
                }
                "open.url" => {
                    b.open_external(str_arg(&args, "url")?.to_string());
                    Ok(Value::Null)
                }
                other => Err(format!("unknown call {other}")),
            }
        })
    }

    /// Run a call on the session's runtime; `done` gets the answer (on a background thread).
    pub fn call_then(&self, method: String, args: Value, done: impl FnOnce(Result<Value, String>) + Send + 'static) {
        let fut = self.call(method, args);
        self.inner.net.rt.spawn(async move { done(fut.await) });
    }

    pub fn shutdown(&self) {
        flush_cache();
        self.inner.backend.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agu_chat_core::protocol::Settings;

    #[test]
    fn single_channel_tabs_from_the_first_versions_become_one_channel_lists() {
        let old = json!([{"platform": "twitch", "channel": "a_g_u__"}, {"platform": "kick", "channel": "agu"}]);
        let v = migrate_tabs(old);
        assert_eq!(
            v,
            json!([
                {"name": "", "channels": [{"platform": "twitch", "channel": "a_g_u__"}]},
                {"name": "", "channels": [{"platform": "kick", "channel": "agu"}]},
            ])
        );
        // the new shape passes through, junk is dropped
        let new = json!([{"name": "Me", "channels": [{"platform": "twitch", "channel": "a"}, {"platform": "kick", "channel": "a"}]}, {"x": 1}, {"name": "empty", "channels": []}]);
        assert_eq!(migrate_tabs(new.clone()), json!([new[0].clone()]));
        assert_eq!(migrate_tabs(json!("nope")), json!([]));
    }

    #[test]
    fn channels_are_normalised_and_checked() {
        let c = channels_arg(&json!({"channels": [{"platform": "twitch", "channel": "#A_G_U__"}, {"platform": "youtube", "channel": "AbC"}]})).unwrap();
        assert_eq!(c[0].channel, "a_g_u__");
        assert_eq!(c[1].channel, "AbC", "YouTube ids keep their case");
        assert!(channels_arg(&json!({"channels": []})).is_err());
        assert!(channels_arg(&json!({"channels": [{"platform": "twitch", "channel": " "}]})).is_err());
        assert!(channels_arg(&json!({"channels": [{"platform": "nope", "channel": "a"}]})).is_err());
    }

    fn state_with_two_views() -> State {
        let mut st = State::new();
        let a = SourceRef { platform: Platform::Twitch, channel: "agu".into() };
        for vk in ["v1", "v2"] {
            let c = st.hub.set_view(vk, std::slice::from_ref(&a));
            assert!(c.created);
            let mut v = View::new();
            v.store.sync(std::slice::from_ref(&a));
            st.views.insert(vk.into(), v);
        }
        st
    }

    fn feed(st: &mut State, sk: &str, f: &Value) {
        for vk in st.hub.views_of(sk) {
            let v = st.views.get_mut(&vk).unwrap();
            v.dirty |= v.store.apply(sk, f);
        }
    }

    fn chat(id: &str, text: &str) -> Value {
        json!({"type": "chat", "msg": {"id": id, "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": text, "emotes": "", "badges": ""}})
    }

    #[test]
    fn a_kick_chats_7tv_set_belongs_to_its_broadcasters_user_id() {
        assert_eq!(kick_owner_id(&json!({"userId": "123", "slug": "a"})).as_deref(), Some("123"));
        assert_eq!(kick_owner_id(&json!({"userId": 456})).as_deref(), Some("456"));
        assert_eq!(kick_owner_id(&json!({"userId": ""})), None, "not known yet");
        assert_eq!(kick_owner_id(&json!({"slug": "a"})), None);
    }

    #[test]
    fn a_frame_reaches_every_view_with_the_channel_and_alerts_once() {
        let mut st = state_with_two_views();
        let mut s: Settings = serde_json::from_value(json!({"highlightRules": [{"id": "r", "enabled": true, "kind": "phrase", "pattern": "gg", "caseSensitive": false, "color": "#ff9f1a", "sound": true, "soundFile": "", "flash": false}]})).unwrap();
        s.highlight_sound_focused = true;
        st.rules = Rules::new(s, AccountsState::new());
        feed(&mut st, "twitch:agu", &chat("1", "gg"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        st.scan("v2", &mut out);
        let alerts = out.iter().filter(|(k, _)| *k == "alert").count();
        assert_eq!(alerts, 1, "the message is in two tabs but rings once");
        let acts: Vec<&Value> = out.iter().filter(|(k, _)| *k == "activity").map(|(_, v)| v).collect();
        assert_eq!(acts.len(), 2);
        assert!(acts.iter().all(|a| a["kind"] == "highlight"));
        assert_eq!(st.mentions.len(), 1, "one mention however many tabs");
        // both views draw the row with the rule's colour
        for vk in ["v1", "v2"] {
            let State { views, rules, src, defs, book, .. } = &mut st;
            let cx = Ctx { src, rules, defs: defs.clone(), book };
            let e = views.get_mut(vk).unwrap().diff(vk, &cx).expect("rows");
            assert_eq!(e["upsert"][0]["highlight"]["color"], "#ff9f1a");
        }
    }

    #[test]
    fn closing_a_view_leaves_the_other_one_its_rows() {
        let mut st = state_with_two_views();
        feed(&mut st, "twitch:agu", &chat("1", "hi"));
        st.views.remove("v1");
        assert!(st.hub.close_view("v1").is_empty());
        feed(&mut st, "twitch:agu", &chat("2", "again"));
        let State { views, rules, src, defs, book, .. } = &mut st;
        let cx = Ctx { src, rules, defs: defs.clone(), book };
        let e = views.get_mut("v2").unwrap().diff("v2", &cx).unwrap();
        assert_eq!(e["upsert"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_merged_view_lists_both_platforms_in_one_list_with_their_platform() {
        let mut st = State::new();
        let (t, k) = (SourceRef { platform: Platform::Twitch, channel: "agu".into() }, SourceRef { platform: Platform::Kick, channel: "agu".into() });
        st.hub.set_view("v", &[t.clone(), k.clone()]);
        let mut v = View::new();
        v.store.sync(&[t, k]);
        st.views.insert("v".into(), v);
        feed(&mut st, "twitch:agu", &chat("1", "from twitch"));
        feed(&mut st, "kick:agu", &json!({"type": "chat", "msg": {"id": "k1", "ts": 2, "text": "from kick", "user": {"id": "3", "login": "kim", "name": "Kim", "color": "#00ff00"}}}));
        let State { views, rules, src, defs, book, .. } = &mut st;
        let cx = Ctx { src, rules, defs: defs.clone(), book };
        let e = views.get_mut("v").unwrap().diff("v", &cx).unwrap();
        let platforms: Vec<&str> = e["upsert"].as_array().unwrap().iter().map(|r| r["platform"].as_str().unwrap()).collect();
        assert_eq!(platforms.len(), 2);
        assert!(platforms.contains(&"twitch") && platforms.contains(&"kick"));
    }

    #[test]
    fn your_name_goes_to_the_mentions_list_and_the_bell() {
        let mut st = state_with_two_views();
        let mut acc = AccountsState::new();
        acc.insert(Platform::Twitch, serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": "agu", "name": "Agu"}})).unwrap());
        st.rules = Rules::new(Settings::default(), acc);
        feed(&mut st, "twitch:agu", &chat("1", "hello @agu"));
        feed(&mut st, "twitch:agu", &chat("2", "plain"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        st.mentions_event(&mut out);
        assert_eq!(st.mentions.len(), 1);
        let bell = out.iter().find(|(k, _)| *k == "mentions").map(|(_, v)| v.clone()).unwrap();
        assert_eq!(bell, json!({"unread": 1, "total": 1}));
        st.mentions.mark_all_read();
        st.mentions_event(&mut out);
        assert_eq!(out.last().unwrap().1, json!({"unread": 0, "total": 1}));
    }
}
