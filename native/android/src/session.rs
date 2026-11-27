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
//! - `notify`   `{vk, key, sk, channel, user, text, whisper}`: a mention or highlight that came while the app was in the
//!              background (the app posts a notification; tapping it opens that message).
//! - `whisper`  `{login, name}`: someone whispered the account and it is not an ignored user (the app opens a tab).
//! - `hype`     `{sk, train: {...} | null}`: a Twitch channel's hype train, or none (the bar).
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
use agu_chat_model::hype_train::{contributes, expired, fill_color, info_lines, parse as parse_hype, HypeState, HypeTrain, ACTIVE_POLL, ERROR_POLL, IDLE_POLL, SOON};
use agu_chat_model::ignores::{is_user_ignored, IgnoreCache};
use agu_chat_model::mentions::{classify, is_whisper_key, mention_id, row_deleted, Alert, Mentions};
use agu_chat_core::chat_rules::{chat_gate, GateLevel};
use agu_chat_core::protocol::ConnFrame;
use agu_chat_model::input::autocomplete::{chatters, suggest, ChangeCtx};
use agu_chat_model::input::gate::twitch_viewer;
use agu_chat_model::input::inventory::{build_kick_inventory, build_rumble_inventory, build_twitch_inventory, build_youtube_inventory, merge_inventories, Inventory};
use agu_chat_model::input::slash::{confirmed_command, Confirmed};
use agu_chat_model::mod_caps::{fetch_mod_caps, relay_api, CoCaps, ModCaps};
use agu_chat_model::types::RowBody;
use agu_chat_model::net::Net;
use agu_chat_model::search::{parse_search, row_text, SearchQuery};
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
use crate::compose::{byte_of, picker_json, plan_send, send_failures, suggestions_json, with_custom, SendPlan, Target};
use agu_chat_model::input::custom_commands::{expand_for_message, expand_input, find as find_command, CommandContext};
use agu_chat_model::input::slash::is_me;
use crate::hub::{Added, Hub};
use crate::view::{row_json, style_key, Env, Rules};

#[path = "moderation.rs"]
mod moderation;
use moderation::{build_mod_env, DevRelay};

/// Events to the app: (kind, JSON payload). Called from background threads.
pub type Sink = Arc<dyn Fn(&str, &Value) + Send + Sync>;

const FLUSH_MS: u64 = 60;
/// The source key of the whisper inbox (core's reserved `whisper:*` channel): it only says who whispered.
const INBOX_SK: &str = "twitch:whisper:*";

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
    /// a whisper opened this tab: its conversation comes in as a snapshot (never news by itself), whose newest line
    /// is the mention
    whisper_pending: bool,
    /// the pinned-message bars the app was last sent
    pins_sent: Vec<Value>,
    /// the Twitch pins the user hid for themselves (the newest are remembered)
    hidden_pins: Vec<String>,
}

impl View {
    fn new() -> Self {
        View { store: ChatStore::default(), sent: HashMap::new(), sent_order: Vec::new(), ignore_cache: IgnoreCache::default(), dirty: true, full: true, seen_ord: 0, search: None, restyle: HashSet::new(), whisper_pending: false, pins_sent: Vec::new(), hidden_pins: Vec::new() }
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
        let pins = self.pins(cx);
        let pins_changed = pins != self.pins_sent;
        if !full && upsert.is_empty() && removed.is_empty() && !pins_changed {
            return None;
        }
        let mut ev = json!({"vk": vk, "reset": full, "upsert": upsert, "remove": if full { vec![] } else { removed }});
        if pins_changed {
            ev["pins"] = Value::Array(pins.clone());
            self.pins_sent = pins;
        }
        Some(ev)
    }

    /// The pinned message of each Rumble and Twitch channel here, drawn like a chat line (`row` is null when nothing
    /// is pinned, or the pin is hidden or over). `canMod`: the account may unpin it.
    fn pins(&self, cx: &Ctx) -> Vec<Value> {
        let now = agu_chat_model::pins::now_ms();
        self.store
            .sources
            .iter()
            .filter_map(|s| {
                let env = env_of(&s.sk, cx, Some(&self.store));
                match s.source.platform {
                    Platform::Rumble => {
                        let row = s.rumble_pinned.as_ref().map(|m| {
                            let r = agu_chat_model::types::Row { key: format!("{}|pin", s.sk), ts: m.ts, sk: s.sk.clone(), platform: Platform::Rumble, ord: 0, backlog: true, body: RowBody::Rumble(m.clone()) };
                            row_json(&r, &env)
                        });
                        Some(json!({"sk": s.sk, "row": row, "canMod": env.mods.as_ref().map(|m| m.can_moderate()).unwrap_or(false)}))
                    }
                    Platform::Twitch if agu_chat_core::twitch::whisper::login_of(&s.source.channel).is_none() => {
                        let shown = s.pinned.as_ref().filter(|p| !agu_chat_model::pins::pin_expired(p, now) && !self.hidden_pins.contains(&p.id));
                        Some(match shown {
                            Some(p) => {
                                let by_id = p.pin.as_ref().map(|x| x.by.id.as_str()).unwrap_or("");
                                // the pinner's badges come from their latest line in this chat
                                let badges = self.store.rows().iter().rev().find_map(|r| match &r.body {
                                    RowBody::Msg(m) if r.sk == s.sk && !by_id.is_empty() && m.user.id == by_id => Some(m.badges.clone()),
                                    _ => None,
                                });
                                crate::view::pin_json(&s.sk, p, badges.as_deref().unwrap_or(""), env.mods.as_ref().map(|m| m.twitch.pin).unwrap_or(false), &env)
                            }
                            None => json!({"sk": s.sk, "kind": "twitch", "row": null, "canMod": false}),
                        })
                    }
                    _ => None,
                }
            })
            .collect()
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

/// The `hype` event: the bar of one Twitch channel, or `null` when its train is over.
fn hype_json(sk: &str, t: Option<&HypeTrain>) -> Value {
    use agu_chat_model::hype_train::plural;
    let train = t.map(|t| {
        json!({
            "id": t.id, "level": t.level, "percent": t.percent, "expiresMs": t.expires_ms,
            "fill": format!("#{:06x}", fill_color(t.color)),
            "bits": plural(t.bits, "Bit", "Bits"), "subs": plural(t.subs, "Sub", "Subs"),
            "emotes": t.emotes.iter().map(|e| json!({"token": e.token, "url": e.url})).collect::<Vec<_>>(),
            "info": info_lines(t),
            "conductors": t.conductors.iter().map(|c| json!({"name": c.name, "avatar": c.avatar, "what": if c.source == "BITS" { plural(c.quantity, "Bit", "Bits") } else { plural(c.quantity, "Sub", "Subs") }})).collect::<Vec<_>>(),
        })
    });
    json!({"sk": sk, "train": train})
}

/// The "notify" event for a mention or highlight: who said what, for the background notification.
fn notify_json(vk: &str, r: &agu_chat_model::types::Row, channel: &str) -> Value {
    let user = match &r.body {
        RowBody::Msg(m) => if m.user.name.is_empty() { m.user.login.clone() } else { m.user.name.clone() },
        RowBody::Rumble(m) => m.user.name.clone(),
        RowBody::Kick(m) => m.user.name.clone(),
        RowBody::X(m) => m.user.name.clone(),
        RowBody::YouTube(m) => m.user.name.clone(),
        RowBody::Notice(n) => n.user.name.clone(),
        RowBody::Automod(h) => h.user.name.clone(),
        RowBody::ModFeed(_) => String::new(),
    };
    json!({"vk": vk, "key": r.key, "sk": r.sk, "channel": channel, "user": user, "text": row_text(r), "whisper": is_whisper_key(&r.sk)})
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
    /// per Twitch channel: its hype train and when to ask next
    hype: HashMap<String, HypeState>,
    /// the Twitch pins hidden for the user, newest last (kept in `hidden-pins.json` like the desktop)
    hidden_pins: Vec<String>,
}

type Out = Vec<(&'static str, Value)>;

impl State {
    fn new() -> State {
        State { hub: Hub::default(), views: HashMap::new(), src: HashMap::new(), rules: Rules::default(), alerter: Alerter::default(), mentions: Mentions::default(), mentions_open: false, mentions_sent: (0, 0), active: true, defs: None, defs_requested: false, defs_failures: 0, book: Arc::new(StyleBook::default()), previews: PreviewCache::new(), seven_seq: 0, personal: Vec::new(), personal_requested: HashSet::new(), inv_gen: 0, hype: HashMap::new(), hidden_pins: Vec::new() }
    }

    /// `{vk, sk, parent, thread}`: the lines of the reply thread a reply belongs to (its first message when the chat
    /// still has it, then every reply), drawn like the chat's rows. `{root, hasRoot, canReply, rows}`.
    fn thread_json(&self, args: &Value) -> Result<Value, String> {
        use agu_chat_model::thread::{can_reply, reply_link, thread_root, thread_rows};
        let (vk, sk) = (str_arg(args, "vk")?, str_arg(args, "sk")?);
        let st = self;
        let v = st.views.get(vk).ok_or("that tab is not open")?;
        let all = v.store.rows();
        let root = thread_root(all, sk, args.get("parent").and_then(|p| p.as_str()).unwrap_or(""), args.get("thread").and_then(|p| p.as_str()).unwrap_or(""));
        let list = thread_rows(all, sk, &root);
        let cx = Ctx { src: &st.src, rules: &st.rules, defs: st.defs.clone(), book: &st.book };
        let env = env_of(sk, &cx, Some(&v.store));
        let rows: Vec<Value> = list
            .iter()
            .map(|r| {
                let mut j = row_json(r, &env);
                st.rules.decorate(&mut j, r);
                j
            })
            .collect();
        let has_root = !root.is_empty() && list.first().and_then(|r| reply_link(r)).map(|l| l.id == root).unwrap_or(false);
        let can = st.hub.source(sk).map(|s| can_reply(s.platform, st.src.get(sk).and_then(|d| d.conn.as_ref()).and_then(|c| c.get("canSend")).and_then(|c| c.as_bool()))).unwrap_or(false);
        Ok(json!({"root": root, "hasRoot": has_root, "canReply": can, "rows": rows}))
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
        // A whisper that opened its tab came in with the conversation's snapshot: its newest line is the news.
        if view.whisper_pending {
            if let Some(r) = view.store.rows().iter().rev().find(|r| is_whisper_key(&r.sk) && matches!(r.body, RowBody::Msg(_))).cloned() {
                view.whisper_pending = false;
                if !rules.is_own(&r) && !row_deleted(&r) && !rules.hidden(&r) {
                    activity = Some("highlight");
                    let channel = hub.source(&r.sk).map(|s| whisper_or_channel(&s)).unwrap_or_default();
                    if mentions.add(vk, &channel, r.clone(), *mentions_open) && !*active && (rules.settings.mention_sound || rules.settings.mention_flash) {
                        out.push(("notify", notify_json(vk, &r, &channel)));
                    }
                }
            }
        }
        for r in view.store.rows().iter() {
            if r.ord <= view.seen_ord {
                continue;
            }
            max_ord = max_ord.max(r.ord);
            let (own, hidden) = (rules.is_own(r), rules.hidden(r));
            let Some(alert) = classify(r, own, hidden, rules.me_login(), rules.kick_me(), || rules.rule_of(r)) else { continue };
            activity = activity.or(Some("message"));
            let red = alert.red();
            let mut news = false;
            let mut channel = String::new();
            if red {
                activity = Some("highlight");
                channel = hub.source(&r.sk).map(|s| whisper_or_channel(&s)).unwrap_or_default();
                news = mentions.add(vk, &channel, r.clone(), *mentions_open);
            }
            let wants = match alert {
                Alert::Plain => continue,
                Alert::Mention => Wants { sound: rules.settings.mention_sound, sound_file: String::new(), flash: rules.settings.mention_flash },
                Alert::Rule(rule) => Wants { sound: rule.sound, sound_file: rule.sound_file.clone(), flash: rule.flash },
            };
            // in the background a mention or highlight that alerts at all also becomes a notification
            if news && !*active && (wants.sound || wants.flash) {
                out.push(("notify", notify_json(vk, r, &channel)));
            }
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

    /// The account's own name on a platform ("" when not logged in), for a custom command's `{my.name}`.
    fn me_name(&self, platform: Platform) -> String {
        if platform == Platform::Twitch {
            return self.rules.me_login().to_string();
        }
        self.rules.accounts.get(&platform).and_then(|a| a.user.as_ref()).map(|u| if u.login.is_empty() { u.name.clone() } else { u.login.clone() }).unwrap_or_default()
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

    // ---- hype train ----------------------------------------------------------------------------

    /// A sub or Bits in this Twitch chat: look for a train soon.
    fn hype_poke(&mut self, sk: &str) {
        if let Some(s) = self.hype.get_mut(sk) {
            let soon = Instant::now() + SOON;
            if s.next.map(|n| n > soon).unwrap_or(true) {
                s.next = Some(soon);
            }
        }
    }

    /// One tick of the hype loop: drops what is over (events into `out`) and returns the channels that are due to be
    /// asked, marked busy until [`State::hype_answer`].
    fn hype_step(&mut self, now: Instant, now_ms: i64, out: &mut Out) -> Vec<(String, SourceRef)> {
        let sources: Vec<(String, SourceRef)> = if self.rules.settings.hype_train {
            self.hub.all_sources().into_iter().filter(|(_, s)| s.platform == Platform::Twitch && whisper::login_of(&s.channel).is_none()).collect()
        } else {
            Vec::new()
        };
        let gone: Vec<String> = self.hype.keys().filter(|k| !sources.iter().any(|(sk, _)| sk == *k)).cloned().collect();
        for k in gone {
            if self.hype.remove(&k).is_some_and(|s| s.train.is_some()) {
                out.push(("hype", hype_json(&k, None)));
            }
        }
        let mut due = Vec::new();
        for (sk, src) in sources {
            let s = self.hype.entry(sk.clone()).or_default();
            if s.train.as_ref().is_some_and(|t| expired(t, now_ms)) {
                s.train = None;
                s.next = Some(now);
                out.push(("hype", hype_json(&sk, None)));
            }
            if s.busy || s.next.is_some_and(|n| n > now) {
                continue;
            }
            s.busy = true;
            due.push((sk, src));
        }
        due
    }

    /// The core's answer for a channel's train: the `hype` event when what the bar shows changed.
    fn hype_answer(&mut self, sk: &str, ok: bool, body: &Value, now: Instant) -> Option<(&'static str, Value)> {
        let s = self.hype.get_mut(sk)?;
        s.busy = false;
        if !ok {
            s.next = Some(now + ERROR_POLL);
            return None;
        }
        let t = parse_hype(body);
        s.next = Some(now + if t.is_some() { ACTIVE_POLL } else { IDLE_POLL });
        if s.train == t {
            return None;
        }
        s.train = t;
        Some(("hype", hype_json(sk, s.train.as_ref())))
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

/// The relay request that sends `text` to a chat: where it goes and its body. A Twitch whisper conversation is sent
/// through the same route as a channel's chat (the core turns it into Helix's whisper call); it has nothing to reply to.
fn send_request(source: &SourceRef, text: &str, reply_to: Option<&str>) -> (&'static str, Value) {
    let path = match source.platform {
        Platform::Twitch => "/api/chat/send",
        Platform::Rumble => "/api/rumble/send",
        Platform::Kick => "/api/kick/send",
        Platform::X => "/api/x/send",
        Platform::Youtube => "/api/youtube/send",
    };
    let mut body = json!({"text": text});
    let whisper = source.platform == Platform::Twitch && whisper::login_of(&source.channel).is_some();
    if let Some(r) = reply_to.filter(|r| !r.is_empty() && !whisper) {
        body["replyTo"] = json!(r);
    }
    (path, body)
}

/// `{endsAtMs?}` -> `{left, text, over}`: what is left on a pin ("5 min", "30s"); `left` is null for a pin without an end.
fn pin_left(args: &Value) -> Value {
    let now = agu_chat_model::pins::now_ms();
    match args.get("endsAtMs").and_then(|e| e.as_i64()) {
        Some(e) => {
            let left = agu_chat_model::pins::pin_left_secs(e, now);
            json!({"left": left, "text": agu_chat_model::pins::left_text(left), "over": e <= now})
        }
        None => json!({"left": null, "text": null, "over": false}),
    }
}

/// `{endsAtMs?}` -> where "Unpin after" starts: `{timeMode, unitMin, amount, hint}`.
fn pin_start(args: &Value) -> Value {
    let now = agu_chat_model::pins::now_ms();
    let left = args.get("endsAtMs").and_then(|e| e.as_i64()).map(|e| agu_chat_model::pins::pin_left_secs(e, now));
    let (time_mode, unit_min, amount) = agu_chat_model::pins::unpin_after_start(left);
    json!({"timeMode": time_mode, "unitMin": unit_min, "amount": amount, "hint": agu_chat_model::pins::PIN_RANGE_HINT})
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
        state.hidden_pins = std::fs::read(data_dir.join("hidden-pins.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
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
        let s = session.clone();
        net.rt.spawn(async move { s.listen_whispers().await });
        let s = session.clone();
        net.rt.spawn(async move { s.hype_loop().await });
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

    /// The whisper inbox, for the app's whole life: whispers from people with no tab open yet come through it.
    async fn listen_whispers(&self) {
        let source = SourceRef { platform: Platform::Twitch, channel: whisper::inbox_channel().into() };
        self.inner.routes.lock().unwrap().inflight += 1;
        let res = self.inner.bridge.subscribe(source).await;
        let mut r = self.inner.routes.lock().unwrap();
        r.inflight -= 1;
        if let Ok(id) = &res {
            r.subs.insert(id.clone(), INBOX_SK.to_string());
            for f in r.pending.remove(id).unwrap_or_default() {
                let _ = self.inner.inbox.send(Inbox::Frame { sk: INBOX_SK.to_string(), only: None, f });
            }
        }
        if r.inflight == 0 {
            r.pending.clear();
        }
    }

    /// The hype train bars: once a second, drop trains that ran out and ask the core for the channels that are due.
    async fn hype_loop(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let mut out: Out = Vec::new();
            let due = {
                let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
                self.inner.st.lock().unwrap().hype_step(Instant::now(), now_ms, &mut out)
            };
            self.emit(out);
            for (sk, source) in due {
                let s = self.clone();
                self.inner.net.rt.spawn(async move {
                    let r = relay_api(&s.inner.bridge, &source, "/api/twitch/hype-train", None, None).await;
                    let ev = s.inner.st.lock().unwrap().hype_answer(&sk, r.ok, &r.body, Instant::now());
                    if let Some((kind, v)) = ev {
                        (s.inner.sink)(kind, &v);
                    }
                });
            }
        }
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
        // the whisper inbox: someone whispered the account
        if let Inbox::Frame { sk, f, .. } = &m {
            if sk == INBOX_SK {
                if f.get("type").and_then(|t| t.as_str()) == Some("whisper") {
                    if let Some(login) = f.get("login").and_then(|l| l.as_str()).filter(|l| !l.is_empty()) {
                        // an ignored user's whispers are kept out of sight like their chat: no tab pops up
                        let ignored = is_user_ignored(&self.inner.st.lock().unwrap().rules.settings.ignored_users, Platform::Twitch, login);
                        if !ignored {
                            (self.inner.sink)("whisper", &json!({"login": login, "name": f.get("name").and_then(|n| n.as_str()).unwrap_or(login)}));
                        }
                    }
                }
                return;
            }
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
                    // a sub or Bits can start a train: ask the core soon
                    if only.is_none() && contributes(&f) {
                        st.hype_poke(&sk);
                    }
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
            let hidden = st.hidden_pins.clone();
            let view = st.views.entry(vk.clone()).or_insert_with(|| View { hidden_pins: hidden, ..View::new() });
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
        let (path, body) = send_request(&source, text, reply_to);
        let r = self.relay(&source, path, Some("POST"), Some(body)).await;
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
        let found = with_custom(suggest(text, caret, &cx), text, caret, &st.rules.settings.custom_commands);
        Ok(found.map(|r| suggestions_json(text, &r)).unwrap_or(Value::Null))
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
    /// the box, `confirm` asks first (`/clear`, `/raid`; answered with `compose.confirm`). A custom command's name
    /// sends what it stands for instead, line by line.
    async fn compose_send(&self, args: &Value) -> Result<Value, String> {
        let (vk, text) = (str_arg(args, "vk")?, str_arg(args, "text")?);
        let sks = sks_arg(args);
        let reply = args.get("reply").filter(|r| r.is_object()).and_then(|r| Some((r.get("sk")?.as_str()?.to_string(), r.get("id")?.as_str()?.to_string())));
        let custom = {
            let st = self.inner.st.lock().unwrap();
            let cmds = &st.rules.settings.custom_commands;
            match sks.first().and_then(|sk| st.hub.source(sk)) {
                Some(first) if !cmds.is_empty() => {
                    let ctx = CommandContext { channel: first.channel.clone(), me: st.me_name(first.platform), ..Default::default() };
                    expand_input(text, cmds, &ctx)
                }
                _ => None,
            }
        };
        match custom {
            Some(lines) => self.send_lines(vk, &sks, lines, reply).await,
            None => self.send_text(vk, &sks, text, reply.as_ref(), true).await,
        }
    }

    /// `{vk, sk, name, login, id, text}`: a custom command run from a message's menu, sent to that message's chat.
    async fn command_run(&self, args: &Value) -> Result<Value, String> {
        let (vk, sk, name) = (str_arg(args, "vk")?, str_arg(args, "sk")?, str_arg(args, "name")?);
        let lines = {
            let st = self.inner.st.lock().unwrap();
            let cmd = find_command(&st.rules.settings.custom_commands, name).ok_or("That command is gone")?;
            let source = st.hub.source(sk).ok_or("that chat is not open")?;
            let ctx = CommandContext {
                channel: source.channel.clone(),
                me: st.me_name(source.platform),
                user: str_arg(args, "login").unwrap_or("").to_string(),
                msg_id: str_arg(args, "id").unwrap_or("").to_string(),
                msg_text: str_arg(args, "text").unwrap_or("").to_string(),
            };
            expand_for_message(cmd, &ctx)
        };
        self.send_lines(vk, &[sk.to_string()], lines, None).await
    }

    /// What a custom command expanded to: each line goes the way a typed line would (a "/..." line runs as that slash
    /// command on a single Twitch chat, anything else is a message to the targets), in order, stopping at the first
    /// failure. Answers like `compose.send`.
    async fn send_lines(&self, vk: &str, sks: &[String], lines: Vec<String>, reply: Option<(String, String)>) -> Result<Value, String> {
        if lines.is_empty() {
            return Ok(json!({"clear": false, "toast": "That command has no text", "warn": true}));
        }
        let mut toasts: Vec<String> = Vec::new();
        let mut reply = reply;
        for line in &lines {
            let r = self.send_text(vk, sks, line, reply.as_ref(), false).await?;
            if let Some(t) = r.get("toast").and_then(|t| t.as_str()) {
                toasts.push(t.to_string());
            }
            if r.get("clear") != Some(&Value::Bool(true)) {
                return Ok(json!({"clear": false, "toast": toasts.join(" - "), "warn": true}));
            }
            // the reply goes with the first message line only
            if !(line.starts_with('/') && !is_me(line)) {
                reply = None;
            }
        }
        Ok(if toasts.is_empty() { json!({"clear": true}) } else { json!({"clear": true, "toast": toasts.join(" - "), "warn": false}) })
    }

    /// One typed text to the chats: the desktop's `SendBox::send` after the custom commands. `confirm`: whether `/clear`
    /// and `/raid` may ask (a custom command's lines can't: the user types them).
    async fn send_text(&self, vk: &str, sks: &[String], text: &str, reply: Option<&(String, String)>, confirm: bool) -> Result<Value, String> {
        let (targets, last, pinned) = {
            let st = self.inner.st.lock().unwrap();
            let mut targets = Vec::new();
            for sk in sks {
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
            SendPlan::Confirm { target, what, text } => Ok(if !confirm {
                toast(format!("Type /{} yourself to confirm it", if what == Confirmed::Clear { "clear" } else { "raid" }), true, false)
            } else {
                match what {
                    Confirmed::Clear => json!({"clear": false, "confirm": {"kind": "clear", "sk": target, "text": text}}),
                    Confirmed::Raid { login } => json!({"clear": false, "confirm": {"kind": "raid", "sk": target, "login": login, "text": text}}),
                }
            }),
            SendPlan::Slash { target, plan } => {
                let source = self.source_of(&target)?;
                match self.relay_post(&source, plan.path, plan.body, plan.method).await {
                    Ok(_) => Ok(toast(plan.ok, false, true)),
                    Err(e) => Ok(toast(e, true, false)),
                }
            }
            SendPlan::Plain { text, targets: sks } => {
                let multi = sks.len() > 1;
                let sends = sks.iter().map(|sk| {
                    let reply_to = reply.filter(|(rsk, _)| rsk == sk).map(|(_, id)| id.clone());
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

    /// `{id}`: "Hide for yourself" on a Twitch pin: gone from every tab, and remembered.
    fn pin_hide(&self, id: &str) {
        let mut guard = self.inner.st.lock().unwrap();
        let st = &mut *guard;
        agu_chat_model::pins::hide_pin(&mut st.hidden_pins, id);
        if let Ok(b) = serde_json::to_vec(&st.hidden_pins) {
            let _ = std::fs::write(self.inner.data_dir.join("hidden-pins.json"), b);
        }
        for v in st.views.values_mut() {
            v.hidden_pins = st.hidden_pins.clone();
            v.dirty = true;
        }
    }

    fn thread_rows(&self, args: &Value) -> Result<Value, String> {
        self.inner.st.lock().unwrap().thread_json(args)
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
                    let whisper_opened = args.get("whisper").and_then(|w| w.as_bool()).unwrap_or(false);
                    let r = s.set_view(vk, channels_arg(&args)?, lenient).await?;
                    // a tab a whisper opened: its newest line is a mention (see `scan`)
                    if whisper_opened {
                        let opened = r.get("vk").and_then(|v| v.as_str()).unwrap_or("");
                        if let Some(v) = s.inner.st.lock().unwrap().views.get_mut(opened) {
                            v.whisper_pending = true;
                        }
                    }
                    Ok(r)
                }
                // the app's screens came back after being closed while the process lived on (the background service):
                // its tabs are opened again, so the views the old screens left are closed first
                "views.reset" => {
                    let ids: Vec<String> = s.inner.st.lock().unwrap().hub.view_ids();
                    for vk in ids {
                        s.close_view(&vk).await;
                    }
                    Ok(Value::Null)
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
                "commands.run" => s.command_run(&args).await,
                "commands.check" => Ok(crate::commands_api::check(&b.settings_get().custom_commands, &args)),
                "commands.import" => {
                    let text = str_arg(&args, "text")?;
                    let out = crate::commands_api::import_json(&b.settings_get().custom_commands, text);
                    if let Some(list) = out.get("commands") {
                        b.settings_set(json!({"customCommands": list})).await;
                    }
                    Ok(json!({"status": out["status"], "warn": out["warn"]}))
                }
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
                "thread.rows" => s.thread_rows(&args),
                // Twitch's pinned message: hide it for the user, what is left of it, the "Pin for" and "Unpin after" choices
                "pin.hide" => {
                    s.pin_hide(str_arg(&args, "id")?);
                    Ok(Value::Null)
                }
                "pin.left" => Ok(pin_left(&args)),
                "pin.choices" => Ok(json!(agu_chat_model::mod_actions::PIN_CHOICES.iter().map(|(l, s)| json!([l, s])).collect::<Vec<_>>())),
                "pin.start" => Ok(pin_start(&args)),
                "pin.secs" => Ok(json!(agu_chat_model::pins::unpin_secs(str_arg(&args, "amount")?, args.get("unitMin").and_then(|u| u.as_bool()).unwrap_or(true)))),
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
                "rumble.muted" => s.rumble_muted(&args).await,
                "rumble.unmute" => s.rumble_unmute(&args).await,
                "modes.get" => s.modes_get(&args).await,
                "modes.patch" => s.modes_patch(&args).await,
                "modes.shield" => s.modes_shield(&args).await,
                "terms.list" => s.terms(&args, "list").await,
                "terms.add" => s.terms(&args, "add").await,
                "terms.remove" => s.terms(&args, "remove").await,
                "dev.caps" => s.dev_caps(&args),
                "dev.relay" => s.dev_relay(&args),
                // debuggable builds only: show a hype train as if the core had answered, `{sk, body}` (the core's /api/twitch/hype-train answer)
                "dev.hype" => {
                    let sk = str_arg(&args, "sk")?.to_string();
                    let ev = s.inner.st.lock().unwrap().hype_answer(&sk, true, args.get("body").unwrap_or(&Value::Null), Instant::now());
                    if let Some((kind, v)) = ev {
                        (s.inner.sink)(kind, &v);
                    }
                    Ok(Value::Null)
                }
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
                // {platform, channel}: the page the channel's stream plays on, or null (a whisper has none)
                "stream.url" => {
                    let src = SourceRef { platform: platform_of(&args)?, channel: str_arg(&args, "channel")?.to_string() };
                    Ok(agu_chat_model::stream_link::stream_url(&src).map(Value::from).unwrap_or(Value::Null))
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
    fn a_rumble_pin_becomes_a_bar_with_the_chats_emotes_and_goes_away_again() {
        let mut st = State::new();
        let r = SourceRef { platform: Platform::Rumble, channel: "agu".into() };
        st.hub.set_view("v", &[r.clone()]);
        let mut v = View::new();
        v.store.sync(&[r]);
        st.views.insert("v".into(), v);
        let log = |pinned: Value| json!({"type": "chatlog", "messages": [], "pinned": pinned, "emotes": {"r+thisisfine": {"file": "https://e/fine.png"}}});
        let m = json!({"id": "9", "ts": 3, "user": {"id": "1", "login": "bob", "name": "Bob", "color": ""}, "text": "read :r+thisisfine:", "badges": []});
        feed(&mut st, "rumble:agu", &log(m));
        let ev = |st: &mut State| {
            let State { views, rules, src, defs, book, .. } = st;
            let cx = Ctx { src, rules, defs: defs.clone(), book };
            views.get_mut("v").unwrap().diff("v", &cx)
        };
        let e = ev(&mut st).unwrap();
        let pin = &e["pins"][0];
        assert_eq!(pin["sk"], "rumble:agu");
        assert_eq!(pin["row"]["id"], "9");
        assert!(pin["row"]["parts"].as_array().unwrap().iter().any(|p| p["t"] == "emote" && p["url"] == "https://e/fine.png"), "the pinned line draws the chat's emotes");
        assert_eq!(pin["canMod"], false);
        // nothing changed: no new pins
        st.views.get_mut("v").unwrap().dirty = true;
        assert!(ev(&mut st).is_none());
        // a pin frame replaces it, an empty one clears it
        feed(&mut st, "rumble:agu", &json!({"type": "pin", "msg": null}));
        assert!(ev(&mut st).unwrap()["pins"][0]["row"].is_null());
    }
    fn pin_frame(id: &str, by_id: &str, ends: Option<&str>) -> Value {
        json!({"type": "pin", "msg": {"id": id, "ts": 1, "user": {"id": "9", "login": "bob", "name": "Bob", "color": ""}, "text": "read this Kappa", "emotes": "25:10-14", "badges": "",
            "pin": {"by": {"id": by_id, "login": "mod", "name": "Mod"}, "startsAt": "2026-01-01T00:00:00Z", "endsAt": ends}}})
    }

    #[test]
    fn a_twitch_pin_is_sent_with_its_pinner_end_and_goes_when_hidden_or_cleared() {
        let mut st = state_with_two_views();
        // the pinner's latest line gives the badges
        let mut line = chat("m0", "hi");
        line["msg"]["user"] = json!({"id": "7", "login": "mod", "name": "Mod", "color": ""});
        line["msg"]["badges"] = json!("moderator/1");
        feed(&mut st, "twitch:agu", &line);
        feed(&mut st, "twitch:agu", &pin_frame("p1", "7", Some("2099-01-01T00:00:00Z")));
        let ev = |st: &mut State| {
            let State { views, rules, src, defs, book, .. } = st;
            let cx = Ctx { src, rules, defs: defs.clone(), book };
            views.get_mut("v1").unwrap().diff("v1", &cx)
        };
        let e = ev(&mut st).unwrap();
        let pin = &e["pins"][0];
        assert_eq!((pin["kind"].clone(), pin["sk"].clone(), pin["id"].clone()), (json!("twitch"), json!("twitch:agu"), json!("p1")));
        assert_eq!(pin["by"]["name"], "Mod");
        assert!(pin["endsAtMs"].as_i64().unwrap() > 4_000_000_000_000);
        assert_eq!(pin["row"]["id"], "p1");
        assert_eq!(pin["canMod"], false);
        // hiding it for myself takes it out of the tab it was hidden in
        let hidden = vec!["p1".to_string()];
        st.views.get_mut("v1").unwrap().hidden_pins = hidden;
        st.views.get_mut("v1").unwrap().dirty = true;
        assert!(ev(&mut st).unwrap()["pins"][0]["row"].is_null());
        // an ended pin is not shown either, and a cleared one is gone
        feed(&mut st, "twitch:agu", &pin_frame("p2", "7", Some("2020-01-01T00:00:00Z")));
        st.views.get_mut("v2").unwrap().dirty = true;
        let State { views, rules, src, defs, book, .. } = &mut st;
        let cx = Ctx { src, rules, defs: defs.clone(), book };
        assert!(views.get_mut("v2").unwrap().diff("v2", &cx).unwrap()["pins"][0]["row"].is_null());
    }

    #[test]
    fn a_reply_thread_lists_its_root_and_every_reply_and_only_those() {
        let mut st = state_with_two_views();
        let reply = |id: &str, parent: &str| {
            let mut m = chat(id, "re");
            m["msg"]["reply"] = json!({"user": "Bob", "text": "orig", "login": "bob", "parentId": parent, "threadId": "r1"});
            m
        };
        feed(&mut st, "twitch:agu", &chat("r1", "root"));
        feed(&mut st, "twitch:agu", &chat("x", "elsewhere"));
        feed(&mut st, "twitch:agu", &reply("a", "r1"));
        feed(&mut st, "twitch:agu", &reply("b", "a"));
        let t = st.thread_json(&json!({"vk": "v1", "sk": "twitch:agu", "parent": "a", "thread": "r1"})).unwrap();
        assert_eq!(t["root"], "r1");
        assert_eq!(t["hasRoot"], true);
        let ids: Vec<&str> = t["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["r1", "a", "b"]);
        // nobody has said this chat may be posted in yet
        assert_eq!(t["canReply"], false);
        // a thread whose first message left the store says so
        let t = st.thread_json(&json!({"vk": "v1", "sk": "twitch:agu", "parent": "gone", "thread": "gone"})).unwrap();
        assert_eq!((t["hasRoot"].clone(), t["rows"].as_array().unwrap().len()), (json!(false), 0));
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
    fn me_agu() -> AccountsState {
        let mut acc = AccountsState::new();
        acc.insert(Platform::Twitch, serde_json::from_value(json!({"connected": true, "user": {"id": "1", "login": "agu", "name": "Agu"}})).unwrap());
        acc
    }

    #[test]
    fn a_channels_stream_link_comes_from_the_shared_rules() {
        let u = |p, c: &str| agu_chat_model::stream_link::stream_url(&SourceRef { platform: p, channel: c.into() });
        assert_eq!(u(Platform::Twitch, "A_g_u__").as_deref(), Some("https://www.twitch.tv/a_g_u__"));
        assert_eq!(u(Platform::Twitch, "whisper:bob"), None);
        assert_eq!(u(Platform::Kick, "agu").as_deref(), Some("https://kick.com/agu"));
    }

    #[test]
    fn a_whisper_is_sent_as_plain_text_through_the_chat_route() {
        let w = SourceRef { platform: Platform::Twitch, channel: whisper::channel_for("bob") };
        assert_eq!(send_request(&w, "hi bob", Some("m1")), ("/api/chat/send", json!({"text": "hi bob"})));
        let c = SourceRef { platform: Platform::Twitch, channel: "agu".into() };
        assert_eq!(send_request(&c, "hi", Some("m1")), ("/api/chat/send", json!({"text": "hi", "replyTo": "m1"})));
        assert_eq!(send_request(&c, "hi", Some("")).1, json!({"text": "hi"}));
    }

    #[test]
    fn a_mention_in_the_background_becomes_a_notification_once() {
        let mut st = state_with_two_views();
        st.rules = Rules::new(Settings::default(), me_agu());
        st.active = false;
        feed(&mut st, "twitch:agu", &chat("1", "hello @agu"));
        feed(&mut st, "twitch:agu", &chat("2", "plain talk"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        st.scan("v2", &mut out);
        let notes: Vec<&Value> = out.iter().filter(|(k, _)| *k == "notify").map(|(_, v)| v).collect();
        assert_eq!(notes.len(), 1, "two tabs, one message, one notification");
        assert_eq!((notes[0]["user"].as_str(), notes[0]["text"].as_str(), notes[0]["channel"].as_str()), (Some("Bob"), Some("hello @agu"), Some("agu")));
        assert_eq!(notes[0]["whisper"], false);
        assert!(notes[0]["key"].as_str().unwrap().contains('1'), "the key is the row's, for the jump");
    }

    #[test]
    fn nothing_is_posted_while_the_app_is_on_screen_or_when_the_alert_is_off() {
        let mut st = state_with_two_views();
        st.rules = Rules::new(Settings::default(), me_agu());
        feed(&mut st, "twitch:agu", &chat("1", "hello @agu"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        assert!(out.iter().all(|(k, _)| *k != "notify"), "on screen");
        assert_eq!(st.mentions.len(), 1);

        let mut st = state_with_two_views();
        let mut s = Settings::default();
        s.mention_sound = false;
        s.mention_flash = false;
        st.rules = Rules::new(s, me_agu());
        st.active = false;
        feed(&mut st, "twitch:agu", &chat("1", "hello @agu"));
        let mut out: Out = Vec::new();
        st.scan("v1", &mut out);
        assert!(out.iter().all(|(k, _)| *k != "notify"), "mention alerts are off");
        assert_eq!(st.mentions.len(), 1, "it is still a mention");
    }

    fn whisper_view(login: &str) -> (State, String) {
        let mut st = State::new();
        let w = SourceRef { platform: Platform::Twitch, channel: whisper::channel_for(login) };
        st.hub.set_view("w", std::slice::from_ref(&w));
        let mut v = View::new();
        v.store.sync(std::slice::from_ref(&w));
        v.whisper_pending = true;
        st.views.insert("w".into(), v);
        (st, source_key(&w))
    }

    fn snapshot(from_id: &str, from: &str) -> Value {
        json!({"type": "chatlog", "channelId": "1", "messages": [
            {"id": "w1", "ts": 1, "user": {"id": from_id, "login": from, "name": from, "color": ""}, "text": "psst", "emotes": "", "badges": ""}],
            "pinned": null, "modfeed": [], "automod": [], "suspicious": [], "esub": {"active": false, "reason": null}})
    }

    #[test]
    fn a_whisper_that_opened_its_tab_puts_its_newest_line_in_the_mentions() {
        let (mut st, sk) = whisper_view("bob");
        st.active = false;
        feed(&mut st, &sk, &snapshot("9", "bob"));
        let mut out: Out = Vec::new();
        st.scan("w", &mut out);
        assert_eq!(st.mentions.len(), 1);
        assert!(!st.views["w"].whisper_pending);
        let note = out.iter().find(|(k, _)| *k == "notify").map(|(_, v)| v.clone()).expect("notified");
        assert_eq!((note["whisper"].as_bool(), note["channel"].as_str(), note["text"].as_str()), (Some(true), Some("bob"), Some("psst")));
        assert!(out.iter().any(|(k, v)| *k == "activity" && v["kind"] == "highlight"));
        // a later whisper in the open tab is a mention too (addressed to you by definition)
        feed(&mut st, &sk, &json!({"type": "chat", "msg": {"id": "w2", "ts": 2, "user": {"id": "9", "login": "bob", "name": "bob", "color": ""}, "text": "again", "emotes": "", "badges": ""}}));
        let mut out: Out = Vec::new();
        st.scan("w", &mut out);
        assert_eq!(st.mentions.len(), 2);
    }

    #[test]
    fn an_ignored_users_whisper_and_your_own_do_not_count() {
        let (mut st, sk) = whisper_view("bob");
        let mut s = Settings::default();
        s.ignored_users = vec![agu_chat_core::protocol::IgnoredUser { platform: Platform::Twitch, login: "bob".into() }];
        st.rules = Rules::new(s, AccountsState::new());
        feed(&mut st, &sk, &snapshot("9", "bob"));
        let mut out: Out = Vec::new();
        st.scan("w", &mut out);
        assert!(st.mentions.is_empty());

        let (mut st, sk) = whisper_view("bob");
        st.rules = Rules::new(Settings::default(), me_agu());
        feed(&mut st, &sk, &snapshot("1", "agu"));
        let mut out: Out = Vec::new();
        st.scan("w", &mut out);
        assert!(st.mentions.is_empty(), "your own line");
    }

    fn train_body(percent: u64, expires: &str) -> Value {
        json!({"active": true, "id": "t1", "level": 2, "percent": percent, "bits": 3300, "subs": 7, "expiresAt": expires,
            "emotes": [{"id": "e1", "token": "Kitty", "url": "https://x/e1"}], "conductors": [{"source": "BITS", "name": "Cheerer", "avatar": "", "quantity": 500}],
            "conductorRewards": ["BITS"], "treasure": false, "goldenKappa": false, "shared": false, "color": "ffbf00", "record": {"level": 3}})
    }

    #[test]
    fn the_hype_loop_asks_for_twitch_channels_only_and_reports_changes() {
        let mut st = State::new();
        let (t, k, w) = (SourceRef { platform: Platform::Twitch, channel: "agu".into() }, SourceRef { platform: Platform::Kick, channel: "agu".into() }, SourceRef { platform: Platform::Twitch, channel: "whisper:bob".into() });
        st.hub.set_view("v", &[t, k, w]);
        let now = Instant::now();
        let mut out: Out = Vec::new();
        let due = st.hype_step(now, 0, &mut out);
        assert_eq!(due.iter().map(|(sk, _)| sk.as_str()).collect::<Vec<_>>(), vec!["twitch:agu"], "not Kick, not a whisper");
        assert!(st.hype_step(now, 0, &mut out).is_empty(), "one request at a time");
        // a train is on: the bar appears, and the next ask is soon
        let ev = st.hype_answer("twitch:agu", true, &train_body(17, "2099-01-01T00:00:00Z"), now).expect("event");
        assert_eq!(ev.0, "hype");
        assert_eq!((ev.1["train"]["level"].as_u64(), ev.1["train"]["percent"].as_u64()), (Some(2), Some(17)));
        assert_eq!(ev.1["train"]["bits"], "3,300 Bits");
        assert_eq!(ev.1["train"]["subs"], "7 Subs");
        assert_eq!(ev.1["train"]["conductors"][0]["what"], "500 Bits");
        assert!(ev.1["train"]["fill"].as_str().unwrap().starts_with('#'));
        assert!(st.hype_step(now + Duration::from_secs(4), 0, &mut out).is_empty());
        assert_eq!(st.hype_step(now + ACTIVE_POLL, 0, &mut out).len(), 1);
        // the same answer again changes nothing; a new level is an event
        assert!(st.hype_answer("twitch:agu", true, &train_body(17, "2099-01-01T00:00:00Z"), now).is_none());
        assert!(st.hype_answer("twitch:agu", true, &train_body(20, "2099-01-01T00:00:00Z"), now).is_some());
        // an error keeps the bar and waits longer
        st.hype.get_mut("twitch:agu").unwrap().busy = true;
        assert!(st.hype_answer("twitch:agu", false, &Value::Null, now).is_none());
        assert!(st.hype.get("twitch:agu").unwrap().train.is_some());
        // the train says it is over
        let ev = st.hype_answer("twitch:agu", true, &json!({"active": false}), now).unwrap();
        assert!(ev.1["train"].is_null());
    }

    #[test]
    fn a_train_that_ran_out_goes_and_the_setting_turns_the_bars_off() {
        let mut st = State::new();
        st.hub.set_view("v", &[SourceRef { platform: Platform::Twitch, channel: "agu".into() }]);
        let now = Instant::now();
        let mut out: Out = Vec::new();
        st.hype_step(now, 0, &mut out);
        st.hype_answer("twitch:agu", true, &train_body(50, "1970-01-01T00:00:10Z"), now);
        let mut out: Out = Vec::new();
        st.hype_step(now, 10_000, &mut out);
        assert!(out.is_empty(), "still within the grace");
        st.hype_step(now, 14_000, &mut out);
        assert_eq!(out.len(), 1);
        assert!(out[0].1["train"].is_null());
        // the setting off: no asking, and a bar already up goes
        st.hype_answer("twitch:agu", true, &train_body(50, "2099-01-01T00:00:00Z"), now);
        let mut s = Settings::default();
        s.hype_train = false;
        st.rules = Rules::new(s, AccountsState::new());
        let mut out: Out = Vec::new();
        assert!(st.hype_step(now + Duration::from_secs(60), 0, &mut out).is_empty());
        assert!(out[0].1["train"].is_null());
    }

    #[test]
    fn a_sub_or_bits_pokes_the_next_ask() {
        let mut st = State::new();
        st.hub.set_view("v", &[SourceRef { platform: Platform::Twitch, channel: "agu".into() }]);
        let now = Instant::now();
        let mut out: Out = Vec::new();
        st.hype_step(now, 0, &mut out);
        st.hype_answer("twitch:agu", true, &json!({"active": false}), now);
        assert!(st.hype.get("twitch:agu").unwrap().next.unwrap() >= now + IDLE_POLL);
        st.hype_poke("twitch:agu");
        assert!(st.hype.get("twitch:agu").unwrap().next.unwrap() < now + IDLE_POLL);
        assert!(contributes(&json!({"type": "chatnotice", "notice": {"msgId": "subgift"}})));
    }
}
