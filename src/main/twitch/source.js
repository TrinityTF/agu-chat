// One Twitch channel's chat: the state ircRelay.js's ChannelRelay kept for a channel
// (backlog, named mod feed, AutoMod queue, suspicious users, pin, room modes) and the frames
// its /ws/chat socket sent, emitted straight to the renderer instead of over a socket.
// IRC comes from the shared hub (ircHub.js), EventSub from the shared session (eventsub.js).
import { buildChatMsg, buildNotice } from "./vendor/irc.js";
import {
  buildModerateEntry,
  buildHeldMsg,
  mapAutomodStatus,
  followersModeText,
  slowModeText,
  toggleModeText,
  textFromFragments,
} from "./vendor/modfeedText.js";
import { hub, esub } from "./shared.js";
import * as auth from "./auth.js";
import { access as getAccess } from "./access.js";
import { helix } from "./helix.js";
import { fetchRecent, shapeRecent } from "./backlog.js";
import { log } from "./rt.js";

const MAX_BACKLOG = 200;
const MAX_AUTOMOD_QUEUE = 50;
const MAX_SUSPICIOUS = 300;
const MAX_MOD_FEED = 50;
const LIVE_POLL_MS = 60_000;
// Twitch has no IRC line or EventSub event for a pin, so the pin banner polls GET /chat/pins
// while the source is open and the user can read it (mods and the broadcaster only).
const PIN_POLL_MS = 15_000;
const REWARDS_TTL_MS = 5 * 60_000;
const BACKLOG_REFETCH_MIN_MS = 30_000;

// NOTICEs that mean this channel cannot be read at all.
const FATAL_NOTICES = new Set(["msg_channel_suspended", "msg_channel_blocked", "tos_ban"]);

/** login -> live source, so routes can reach the source a request is about (to broadcast a mode change, set a pin). */
export const sources = new Map();

export class TwitchSource {
  constructor({ channel, emit }) {
    this.login = channel;
    this.emit = (frame) => {
      try {
        emit(frame);
      } catch (err) {
        log("emit failed", err);
      }
    };
    this.channelId = null; // from the first ROOMSTATE (room-id): anonymous IRC needs no Helix to know it
    this.stopped = false;
    this.backlog = [];
    this.modFeed = [];
    this.automodQueue = [];
    this.suspicious = new Map();
    this.pinned = null;
    this.esub = { active: false, reason: null };
    this.namedFeed = false; // channel.moderate is live: the anonymous IRC-derived feed would double every entry
    this.roomState = null;
    this.rewards = new Map(); // rewardId -> {title, cost, bits?} (only learnable on the user's own channel)
    this.rewardsAt = 0;
    // connection
    this.state = "connecting";
    this.detail = "Connecting…";
    this.live = null;
    this.title = null;
    // timers / bookkeeping
    this.liveTimer = null;
    this.pinTimer = null;
    this.pinSyncing = null;
    this.backlogAt = 0;
    this.canReadPin = false;
    this.authUser = auth.current()?.user.id || null;
    this.onAuth = () => this._onAuthChange();
    this.listener = {
      onJoined: (roomId, info) => this._onJoined(roomId, info),
      onDisconnected: (reason) => this._onDisconnected(reason),
      onChannelError: (detail) => this._setConn("error", detail),
      onNotice: (msgId, text) => this._onNotice(msgId, text),
      onLine: (m) => this._onLine(m),
    };
  }

  // ---------- platform module surface ----------

  start() {
    sources.set(this.login, this);
    this._emitConn();
    auth.events.on("change", this.onAuth);
    hub.add(this.login, this.listener);
    void this._loadBacklog();
  }

  stop() {
    this.stopped = true;
    if (sources.get(this.login) === this) sources.delete(this.login);
    auth.events.off("change", this.onAuth);
    clearInterval(this.liveTimer);
    clearInterval(this.pinTimer);
    this.liveTimer = this.pinTimer = null;
    hub.remove(this.login);
    esub.detach(this);
  }

  snapshot() {
    return [this._connFrame(), this._chatlog()];
  }

  // ---------- frames ----------

  _chatlog() {
    return {
      type: "chatlog",
      messages: [...this.backlog],
      pinned: this.pinned,
      modfeed: [...this.modFeed],
      automod: [...this.automodQueue],
      suspicious: [...this.suspicious.values()],
      esub: this.esub,
    };
  }

  _sendability() {
    const rec = auth.current();
    if (!rec) {
      const err = auth.state().error;
      return { canSend: false, readOnlyReason: err || "Log in to Twitch in Settings to send messages." };
    }
    if (!rec.scopes.includes("user:write:chat")) return { canSend: false, readOnlyReason: "Log in to Twitch again to allow sending messages." };
    return { canSend: true };
  }

  _connFrame() {
    const f = { type: "conn", state: this.state, detail: this.detail, ...this._sendability() };
    if (this.live !== null) f.live = this.live;
    if (this.title) f.title = this.title;
    return f;
  }

  _emitConn() {
    this.emit(this._connFrame());
  }

  _setConn(state, detail) {
    if (this.state === state && this.detail === detail) return;
    this.state = state;
    this.detail = detail;
    if (!this.stopped) this._emitConn();
  }

  // ---------- IRC ----------

  _onJoined(roomId, { reconnect }) {
    if (this.stopped) return;
    this.channelId = roomId ? String(roomId) : this.channelId;
    this.roomState = null; // the join ROOMSTATE is a fresh baseline, never a diff
    this._setConn("open", "");
    if (reconnect && Date.now() - this.backlogAt > BACKLOG_REFETCH_MIN_MS) void this._loadBacklog(); // fill the gap the outage left
    if (this.channelId) {
      void esub.attach(this);
      void this._refreshModeration();
      this._startLivePoll();
    }
  }

  _onDisconnected(reason) {
    if (this.stopped) return;
    this.roomState = null;
    this._setConn("connecting", `Reconnecting… (${reason})`);
  }

  _onNotice(msgId, text) {
    if (FATAL_NOTICES.has(msgId)) this._setConn("error", text || "This channel cannot be read.");
  }

  _onLine(m) {
    if (m.command === "PRIVMSG") {
      const msg = buildChatMsg(m, m.tags["display-name"] || m.user);
      if (msg.rewardId) {
        const info = this.rewards.get(msg.rewardId);
        if (info) msg.reward = info;
        else this.refreshRewards();
      }
      this.pushBacklog(msg);
      this.emit({ type: "chat", msg });
    } else if (m.command === "USERNOTICE") {
      const notice = buildNotice(m);
      if (notice) {
        notice.ts = Number(m.tags["tmi-sent-ts"]) || Date.now();
        this.pushBacklog({ ...notice, notice: true });
        this.emit({ type: "chatnotice", notice });
      }
    } else if (m.command === "CLEARCHAT") {
      const userId = m.tags["target-user-id"] || null;
      const login = m.text || null;
      const duration = Number(m.tags["ban-duration"]) || null;
      this.emit({ type: "clearchat", userId, login, duration });
      for (const e of this.backlog) if (!e.notice && (!userId || e.user?.id === userId)) e.deleted = true;
      // Twitch decides whether clearing the pinned message's author unpins it, so ask rather than guess.
      if (this.pinned && (!userId || this.pinned.user?.id === userId)) this._syncPinSoon();
      if (!this.namedFeed) {
        if (!userId) this.pushModFeed("clear", "chat was cleared");
        else if (duration) this.pushModFeed("timeout", (login || userId) + " was timed out for " + duration + "s");
        else this.pushModFeed("ban", (login || userId) + " was banned");
      }
    } else if (m.command === "CLEARMSG") {
      const id = m.tags["target-msg-id"];
      if (!id) return;
      this.emit({ type: "clearmsg", id });
      const entry = this.backlog.find((e) => e.id === id);
      if (entry) entry.deleted = true;
      if (this.pinned?.id === id) this._syncPinSoon();
      if (!this.namedFeed) this.pushModFeed("delete", "a message from " + (m.tags.login || "someone") + " was deleted");
    } else if (m.command === "ROOMSTATE") {
      if (m.tags["room-id"] && !this.channelId) this.channelId = String(m.tags["room-id"]);
      this._handleRoomState(m.tags);
    }
  }

  pushBacklog(entry) {
    this.backlog.push(entry);
    if (this.backlog.length > MAX_BACKLOG) this.backlog = this.backlog.slice(-MAX_BACKLOG);
  }

  /** Merge history into the live backlog (live entries win on a duplicate id) and tell every subscriber. */
  async _loadBacklog() {
    this.backlogAt = Date.now();
    const lines = await fetchRecent(this.login);
    if (this.stopped || !lines.length) return;
    const { entries, roomId } = shapeRecent(lines);
    if (roomId && !this.channelId) this.channelId = String(roomId);
    const have = new Set(this.backlog.map((e) => e.id));
    const merged = [...entries.filter((e) => !have.has(e.id)), ...this.backlog];
    merged.sort((a, b) => a.ts - b.ts); // stable: same-millisecond lines keep their order
    this.backlog = merged.slice(-MAX_BACKLOG);
    this.emit(this._chatlog()); // a chatlog replaces the view, so this carries everything, live lines included
  }

  // ---------- anonymous mod feed ----------

  pushModFeed(kind, text) {
    const feed = { ts: Date.now(), kind, text };
    this.modFeed.push(feed);
    if (this.modFeed.length > MAX_MOD_FEED) this.modFeed = this.modFeed.slice(-MAX_MOD_FEED);
    this.emit({ type: "modfeed", feed });
  }

  // ROOMSTATE carries the FULL tag snapshot on join, then only the tags that changed. Diff
  // against the last seen value so only genuine changes make a feed entry; the first one is
  // recorded as a baseline and never diffed.
  _handleRoomState(tags) {
    const prev = this.roomState;
    const pick = (k, p) => (k in tags ? tags[k] : prev && prev[p]);
    const next = {
      followers: pick("followers-only", "followers"),
      slow: pick("slow", "slow"),
      emoteOnly: pick("emote-only", "emoteOnly"),
      subsOnly: pick("subs-only", "subsOnly"),
      r9k: pick("r9k", "r9k"),
    };
    this.roomState = next;
    if (!prev || this.namedFeed) return; // the named feed covers every mode change once channel.moderate is live
    if (next.followers !== undefined && next.followers !== prev.followers) this.pushModFeed("mode", followersModeText(next.followers));
    if (next.slow !== undefined && next.slow !== prev.slow) this.pushModFeed("mode", slowModeText(next.slow));
    if (next.emoteOnly !== undefined && next.emoteOnly !== prev.emoteOnly) this.pushModFeed("mode", toggleModeText("Emote-only mode", next.emoteOnly === "1"));
    if (next.subsOnly !== undefined && next.subsOnly !== prev.subsOnly) this.pushModFeed("mode", toggleModeText("Subscribers-only mode", next.subsOnly === "1"));
    if (next.r9k !== undefined && next.r9k !== prev.r9k) this.pushModFeed("mode", toggleModeText("Unique chat mode", next.r9k === "1"));
  }

  // ---------- EventSub ----------

  setEsub(next, { namedFeed } = {}) {
    if (namedFeed !== undefined) this.namedFeed = !!namedFeed;
    else if (!next.active) this.namedFeed = false; // falls back to the anonymous feed the moment EventSub drops
    if (this.esub.active === next.active && this.esub.reason === (next.reason || null)) return;
    this.esub = { active: !!next.active, reason: next.reason || null };
    if (!this.stopped) this.emit({ type: "esub", esub: this.esub });
  }

  handleModerate(event) {
    if (!event?.action) return;
    const actor = event.moderator_user_name || event.moderator_user_login || "A moderator";
    const built = buildModerateEntry(event.action, actor, event);
    if (built) this.pushModFeed(built.kind, built.text);
  }

  handleAutomodHold(event) {
    if (!event?.message_id) return;
    const held = buildHeldMsg(event, "held", null);
    this._upsertAutomod(held);
    this.emit({ type: "automod", held: { ...held } }); // a copy: handleAutomodUpdate later edits the queued object in place
  }

  // Resolves an existing hold (or synthesizes one if a mod resolved it before we connected).
  handleAutomodUpdate(event) {
    if (!event?.message_id) return;
    const status = mapAutomodStatus(event.status);
    const resolvedBy = status === "expired" ? null : event.moderator_user_name || event.moderator_user_login || null;
    const existing = this.automodQueue.find((h) => h.id === event.message_id);
    if (existing) {
      existing.status = status;
      existing.resolvedBy = resolvedBy;
    } else {
      this._upsertAutomod(buildHeldMsg(event, status, resolvedBy));
    }
    this.emit({ type: "automodupdate", id: event.message_id, status, resolvedBy });
  }

  _upsertAutomod(held) {
    const i = this.automodQueue.findIndex((h) => h.id === held.id);
    if (i >= 0) this.automodQueue[i] = held;
    else this.automodQueue.push(held);
    if (this.automodQueue.length > MAX_AUTOMOD_QUEUE) this.automodQueue = this.automodQueue.slice(-MAX_AUTOMOD_QUEUE);
  }

  // A restricted user's messages never reach IRC, so this builds a synthetic ChatMsg for late
  // joiners WITHOUT a {type:"chat"} frame (it would double-render); the "suspicious" frame
  // carries `msg` and the view appends it itself.
  handleSuspiciousMessage(event) {
    if (!event || event.low_trust_status === "none") return;
    const message = event.message || {};
    const msgId = message.message_id || "";
    const user = this._upsertSuspicious(event);
    let msg = null;
    if (event.low_trust_status === "restricted") {
      msg = {
        platform: "twitch",
        id: msgId,
        ts: Date.now(),
        user: { id: event.user_id || "", login: event.user_login || "", name: event.user_name || event.user_login || "", color: "" },
        badges: "",
        text: message.text || "",
        emotes: "",
        gifs: "",
        action: false,
        firstMsg: false,
        reply: null,
        rewardId: null,
        msgId: null,
      };
      this.pushBacklog(msg);
    }
    this.emit({ type: "suspicious", user, msgId, msg });
  }

  _upsertSuspicious(event) {
    const su = {
      id: event.user_id || "",
      login: event.user_login || "",
      name: event.user_name || event.user_login || "",
      status: event.low_trust_status === "restricted" ? "restricted" : "active_monitoring",
      types: Array.isArray(event.types) ? event.types : [],
      evaluation: event.ban_evasion_evaluation && event.ban_evasion_evaluation !== "unknown" ? event.ban_evasion_evaluation : null,
      sharedBanChannels: Array.isArray(event.shared_ban_channel_ids) ? event.shared_ban_channel_ids.length : 0,
    };
    this._setSuspicious(su.id, su);
    return su;
  }

  _setSuspicious(userId, su) {
    this.suspicious.set(userId, su);
    if (this.suspicious.size > MAX_SUSPICIOUS) this.suspicious.delete(this.suspicious.keys().next().value);
  }

  // "none" removes the user; otherwise upsert, keeping previously known types/evaluation (this event carries neither).
  handleSuspiciousUpdate(event) {
    if (!event?.user_id) return;
    const userId = event.user_id;
    const actor = event.moderator_user_name || event.moderator_user_login || "A moderator";
    const target = event.user_name || event.user_login || "the user";
    if (event.low_trust_status === "none") {
      this.suspicious.delete(userId);
      this.emit({ type: "suspicioususer", user: null, userId });
      this.pushModFeed("mode", `${actor} removed ${target} from suspicious-user monitoring`);
      return;
    }
    const existing = this.suspicious.get(userId);
    const status = event.low_trust_status === "restricted" ? "restricted" : "active_monitoring";
    const user = {
      id: userId,
      login: event.user_login || existing?.login || "",
      name: event.user_name || existing?.name || event.user_login || "",
      status,
      types: existing ? existing.types : [],
      evaluation: existing ? existing.evaluation : null,
      sharedBanChannels: existing ? existing.sharedBanChannels : 0,
    };
    this._setSuspicious(userId, user);
    this.emit({ type: "suspicioususer", user, userId });
    this.pushModFeed("mode", status === "restricted" ? `${actor} restricted ${target}'s messages` : `${actor} set ${target} to monitored`);
  }

  // ---------- channel-point redeems (the user's own channel only) ----------

  // A redeem with no text never reaches chat over IRC, so it goes in as a notice; one with
  // text is already there as a message, which gets its reward patched in if it was unknown.
  _redeemToChat(event, reward) {
    const id = reward?.id;
    const title = reward?.title;
    if (!id || !title) return;
    const known = this.rewards.has(id);
    const bits = Number.isFinite(reward.bits) && reward.bits > 0 ? reward.bits : undefined;
    const info = bits ? { title, cost: null, bits } : { title, cost: Number.isFinite(reward.cost) ? reward.cost : null };
    this.rewards.set(id, info);
    if (!String(event.user_input || "").trim()) {
      const user = { id: event.user_id || "", login: event.user_login || "", name: event.user_name || event.user_login || "", color: "" };
      const notice = {
        id: event.id || crypto.randomUUID(),
        ts: Date.now(),
        system: `${user.name || user.login} redeemed ${title}`,
        msgId: bits ? "powerup" : "redeem", // Bits Power-ups file under the page's Bits toggle, not channel points
        user,
        text: "",
        emotes: "",
        reward: info,
      };
      this.pushBacklog({ ...notice, notice: true });
      this.emit({ type: "chatnotice", notice });
    } else if (!known) {
      this._patchReward(id, info);
    }
  }

  handleRedeem(event) {
    this._redeemToChat(event, event?.reward);
  }

  handlePowerUp(event) {
    this._redeemToChat(event, event?.custom_power_up);
  }

  // A redeem's PRIVMSG can land before the reward is known; when it is learned, patch the messages already sent.
  _patchReward(rewardId, reward) {
    let touched = false;
    for (const e of this.backlog) {
      if (!e.notice && e.rewardId === rewardId && !e.reward) {
        e.reward = reward;
        touched = true;
      }
    }
    if (touched) this.emit({ type: "rewardinfo", rewardId, reward });
  }

  /** Titles and costs of the channel's rewards: Helix lists them only for the broadcaster's own token. */
  refreshRewards() {
    const rec = auth.current();
    if (!rec || !this.channelId || this.channelId !== rec.user.id || Date.now() - this.rewardsAt < REWARDS_TTL_MS) return;
    this.rewardsAt = Date.now();
    void (async () => {
      const [rewards, powerUps] = await Promise.allSettled([
        helix("/channel_points/custom_rewards", { query: { broadcaster_id: this.channelId } }),
        helix("/bits/custom_power_ups", { query: { broadcaster_id: this.channelId } }), // needs bits:read
      ]);
      if (rewards.status === "fulfilled") for (const r of rewards.value?.data || []) this.rewards.set(r.id, { title: r.title, cost: Number.isFinite(r.cost) ? r.cost : null });
      if (powerUps.status === "fulfilled") for (const p of powerUps.value?.data || []) this.rewards.set(p.id, { title: p.title, cost: null, bits: p.bits });
      for (const [id, info] of this.rewards) this._patchReward(id, info);
    })();
  }

  // ---------- account changes, live status, pin ----------

  _onAuthChange() {
    if (this.stopped) return;
    const user = auth.current()?.user.id || null;
    this._emitConn(); // canSend / readOnlyReason follow the login
    if (!this.channelId) return;
    void this._refreshModeration(); // scopes may have changed (a re-login for new permissions)
    if (user === this.authUser) return; // a pending login or a scope refresh: the live poll does not depend on it
    this.authUser = user;
    this.live = null;
    this.title = null;
    this._startLivePoll();
  }

  _startLivePoll() {
    clearInterval(this.liveTimer);
    this.liveTimer = null;
    if (!auth.current() || !this.channelId) return;
    void this._pollLive();
    this.liveTimer = setInterval(() => void this._pollLive(), LIVE_POLL_MS);
    this.liveTimer.unref?.();
  }

  async _pollLive() {
    if (this.stopped || !this.channelId) return;
    try {
      const page = await helix("/streams", { query: { user_id: this.channelId } });
      const s = page?.data?.[0];
      const live = !!s;
      const title = s?.title || null;
      if (live !== this.live || title !== this.title) {
        this.live = live;
        this.title = title;
        if (!this.stopped) this._emitConn();
      }
    } catch {
      /* the next poll tries again; the header just keeps what it had */
    }
  }

  async _refreshModeration() {
    const acc = await getAccess(this.channelId);
    if (this.stopped) return;
    const rec = auth.current();
    const can = !!rec && (acc.isBroadcaster || acc.isMod === true) && (rec.scopes.includes("moderator:manage:chat_messages") || rec.scopes.includes("moderator:read:chat_messages"));
    this.canReadPin = can;
    clearInterval(this.pinTimer);
    this.pinTimer = null;
    if (can) {
      this.pinTimer = setInterval(() => void this.syncPin(), PIN_POLL_MS);
      this.pinTimer.unref?.();
      void this.syncPin();
      if (acc.isBroadcaster) this.refreshRewards();
    } else if (this.pinned) {
      this._setPinned(null);
    }
  }

  // Reads the current pin and updates the banner if it changed. A failed read leaves the banner
  // alone; only Twitch saying nothing is pinned clears it. Concurrent callers share one read.
  syncPin() {
    if (!this.channelId || !this.canReadPin) return Promise.resolve();
    if (this.pinSyncing) return this.pinSyncing;
    this.pinSyncing = (async () => {
      try {
        const rec = auth.current();
        if (!rec) return;
        const page = await helix("/chat/pins", { query: { broadcaster_id: this.channelId, moderator_id: rec.user.id } });
        const raw = page?.data?.[0] || null;
        this._setPinned(raw ? this._pinnedFromHelix(raw) : null);
      } catch {
        /* the next poll tries again */
      } finally {
        this.pinSyncing = null;
      }
    })();
    return this.pinSyncing;
  }

  _syncPinSoon() {
    setTimeout(() => void this.syncPin(), 1500).unref?.();
  }

  // Twitch's pin -> what the banner shows. The message as it came through IRC is used when
  // it is still in the backlog (it has badges, colour and emote positions); otherwise it is
  // rebuilt from Twitch's fragments with the badges and colour of the sender's latest line here.
  _pinnedFromHelix(raw) {
    const id = String(raw.message_id || "");
    if (!id) return null;
    const pin = {
      by: { id: raw.pinned_by_user_id || "", login: raw.pinned_by_user_login || "", name: raw.pinned_by_user_name || raw.pinned_by_user_login || "" },
      startsAt: raw.starts_at || null,
      endsAt: raw.ends_at || null,
    };
    const seen = this._findMsg(id);
    if (seen) return { ...seen, pin };
    const senderId = raw.sender_user_id || "";
    const last = senderId ? [...this.backlog].reverse().find((e) => !e.notice && e.user?.id === senderId) : null;
    const login = raw.sender_user_login || "";
    const { text, emotes } = textFromFragments(raw.message);
    return {
      platform: "twitch",
      id,
      ts: raw.starts_at ? Date.parse(raw.starts_at) || Date.now() : Date.now(),
      user: { id: senderId, login, name: raw.sender_user_name || login, color: last ? last.user.color : "" },
      badges: last ? last.badges : "",
      text,
      emotes,
      gifs: "",
      action: false,
      firstMsg: false,
      reply: null,
      rewardId: null,
      msgId: null,
      pin,
    };
  }

  _findMsg(id) {
    return this.backlog.find((e) => e.id === id && !e.notice) || null;
  }

  _setPinned(next) {
    const key = (p) => (p ? `${p.id}|${p.pin?.endsAt || ""}|${p.pin?.by?.id || ""}|${p.text}` : "");
    if (key(next) === key(this.pinned)) return;
    this.pinned = next;
    this.emit({ type: "pin", msg: this.pinned });
  }

  // A pin made from this app: shown at once from the backlog, then confirmed (who pinned it,
  // when it ends) by the syncPin() that follows.
  pinLocal(id, by, durationSeconds) {
    const msg = this._findMsg(id);
    if (!msg) return;
    const now = Date.now();
    const secs = Number(durationSeconds);
    this._setPinned({
      ...msg,
      pin: { by, startsAt: new Date(now).toISOString(), endsAt: Number.isFinite(secs) && secs > 0 ? new Date(now + secs * 1000).toISOString() : null },
    });
  }

  unpin() {
    this._setPinned(null);
  }
}
