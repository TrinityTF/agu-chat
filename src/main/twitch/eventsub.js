// ONE EventSub WebSocket session for the whole app, carrying subscriptions for every open
// Twitch source the user may moderate. Ported from render/lib/eventsub.js, which ran one
// session per channel on the server; Twitch caps WebSocket connections at 3 per client+user,
// and one session can hold many subscriptions (300), so here it is one session, many channels.
//
// Per attached source (a channel the user broadcasts or moderates) it subscribes to the named
// mod feed (channel.moderate v2), AutoMod holds, and suspicious-user events, all with
// moderator_user_id = the user. On the user's OWN channel it also hears channel-point redeems
// and Bits Power-ups (IRC never sees a redeem without text).
//
// It never sends anything on the socket (Twitch closes with 4001 on client traffic).
import { WebSocket } from "ws";
import { helix } from "./helix.js";
import { HttpError } from "./http.js";
import * as auth from "./auth.js";
import { access as getAccess, invalidate as invalidateAccess } from "./access.js";
import { log, userAgent } from "./rt.js";

const WS_URL = "wss://eventsub.wss.twitch.tv/ws";
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 60_000;
const DEDUPE_CAP = 500;
const WELCOME_GRACE_MS = 5000; // on top of Twitch's own keepalive_timeout_seconds
const RETRY_SOURCE_MS = 45_000;
const MAX_SUBSCRIPTIONS = 300; // Twitch's per-WebSocket cap

const MOD_SUBS = [
  { type: "channel.moderate", version: "2" },
  { type: "automod.message.hold", version: "2" },
  { type: "automod.message.update", version: "2" },
  { type: "channel.suspicious_user.message", version: "1" },
  { type: "channel.suspicious_user.update", version: "1" },
];
const REDEEM_SUB = { type: "channel.channel_points_custom_reward_redemption.add", version: "1" };
const POWER_UP_SUB = { type: "channel.custom_power_up_redemption.add", version: "1" };

// Scope precheck, so a token that predates one of these scopes does not make Twitch refuse
// subscriptions one by one: the 4 scopes below plus one of read/manage per channel.moderate category.
const REQUIRED_SCOPES = ["moderator:manage:automod", "moderator:read:suspicious_users", "moderator:read:moderators", "moderator:read:vips"];
const ONE_OF_SCOPE_PAIRS = [
  ["moderator:read:blocked_terms", "moderator:manage:blocked_terms"],
  ["moderator:read:chat_settings", "moderator:manage:chat_settings"],
  ["moderator:read:unban_requests", "moderator:manage:unban_requests"],
  ["moderator:read:banned_users", "moderator:manage:banned_users"],
  ["moderator:read:chat_messages", "moderator:manage:chat_messages"],
  ["moderator:read:warnings", "moderator:manage:warnings"],
];

export function hasRequiredScopes(scopeList) {
  const scopes = new Set(scopeList || []);
  for (const s of REQUIRED_SCOPES) if (!scopes.has(s)) return false;
  for (const pair of ONE_OF_SCOPE_PAIRS) if (!pair.some((s) => scopes.has(s))) return false;
  return true;
}

// Routes one decoded event to the owning source's handler. A malformed event must never kill the session.
export function dispatch(source, subType, event) {
  try {
    switch (subType) {
      case "channel.moderate":
        return source.handleModerate(event);
      case "automod.message.hold":
        return source.handleAutomodHold(event);
      case "automod.message.update":
        return source.handleAutomodUpdate(event);
      case "channel.suspicious_user.message":
        return source.handleSuspiciousMessage(event);
      case "channel.suspicious_user.update":
        return source.handleSuspiciousUpdate(event);
      case REDEEM_SUB.type:
        return source.handleRedeem(event);
      case POWER_UP_SUB.type:
        return source.handlePowerUp(event);
      default:
        return undefined;
    }
  } catch (err) {
    log("eventsub handler threw", subType, err);
  }
}

export class EventSubManager {
  constructor({ createWebSocket } = {}) {
    this.createWebSocket = createWebSocket || ((url) => new WebSocket(url, { headers: { "User-Agent": userAgent() } }));
    this.attached = new Map(); // channelId -> source
    this.subs = new Map(); // channelId -> Map(type -> subscription id)
    this.ws = null;
    this.reconnectSocket = null; // session_reconnect handoff in flight
    this.sessionId = null;
    this.sessionUser = null; // user id the session's subscriptions belong to
    this.keepaliveSec = 10;
    this.watchdogTimer = null;
    this.reconnectTimer = null;
    this.reconnectDelay = RECONNECT_BASE_MS;
    this.retryTimers = new Map(); // channelId -> timer
    this.seen = new Set();
    this.seenOrder = [];
    auth.events.on("change", () => this.onAccountChange());
  }

  status() {
    return {
      connected: !!this.sessionId,
      sessionId: this.sessionId,
      attached: [...this.attached.keys()],
      subs: Object.fromEntries([...this.subs].map(([id, m]) => [id, [...m.keys()]])),
    };
  }

  // ---------- source lifecycle ----------

  async attach(source) {
    const id = source.channelId;
    if (!id) return;
    this.attached.set(id, source);
    await this._evaluate(source);
  }

  detach(source) {
    const id = source.channelId;
    if (!id || this.attached.get(id) !== source) return;
    this.attached.delete(id);
    clearTimeout(this.retryTimers.get(id));
    this.retryTimers.delete(id);
    const mine = this.subs.get(id);
    this.subs.delete(id);
    // Free the subscriptions on Twitch's side; cheap and best-effort (they die with the socket anyway).
    if (mine) for (const subId of mine.values()) helix("/eventsub/subscriptions", { method: "DELETE", query: { id: subId } }).catch(() => {});
    if (this.attached.size === 0) this._close(); // nothing left to listen for: let go of the socket
  }

  onAccountChange() {
    const rec = auth.current();
    const uid = rec ? rec.user.id : null;
    if (uid !== this.sessionUser) {
      this._close();
      this.sessionUser = uid;
      invalidateAccess();
    }
    for (const source of this.attached.values()) void this._evaluate(source);
  }

  // Decides what this user may hear on this channel, sets the source's status, and subscribes.
  async _evaluate(source) {
    const id = source.channelId;
    const rec = auth.current();
    if (!rec) return source.setEsub({ active: false, reason: "relogin" });
    if (this.sessionUser === null) this.sessionUser = rec.user.id;
    const acc = await getAccess(id);
    if (this.attached.get(id) !== source) return;
    if (!(acc.isBroadcaster || acc.isMod === true)) {
      if (acc.isMod === false) return source.setEsub({ active: false, reason: null }); // simply not a mod here: nothing to show
      return source.setEsub({ active: false, reason: rec.scopes.includes("user:read:moderated_channels") ? "error" : "relogin" });
    }
    if (!hasRequiredScopes(rec.scopes)) return source.setEsub({ active: false, reason: "relogin" });
    if (!this.subs.get(id)?.has(MOD_SUBS[0].type)) source.setEsub({ active: false, reason: "connecting" });
    this._ensureSession();
    if (this.sessionId) await this._subscribeSource(source);
  }

  _retryLater(source) {
    const id = source.channelId;
    if (this.retryTimers.has(id) || this.attached.get(id) !== source) return;
    const t = setTimeout(() => {
      this.retryTimers.delete(id);
      if (this.attached.get(id) === source) void this._evaluate(source);
    }, RETRY_SOURCE_MS);
    t.unref?.();
    this.retryTimers.set(id, t);
  }

  // ---------- session ----------

  _ensureSession() {
    if (this.ws || this.reconnectTimer) return;
    this._connect();
  }

  _connect() {
    let ws;
    try {
      ws = this.createWebSocket(WS_URL);
    } catch (err) {
      log("eventsub connect failed", err.message);
      return this._scheduleReconnect();
    }
    ws.__reconnect = false;
    this.ws = ws;
    this._bind(ws);
  }

  _bind(ws) {
    ws.on("message", (data) => this._onMessage(ws, data));
    const drop = () => {
      if (ws.__reconnect) {
        // The handoff socket died before its welcome. The old one is still alive and subscribed;
        // its own close (or the watchdog) drives a normal reconnect.
        if (this.reconnectSocket === ws) this.reconnectSocket = null;
        return;
      }
      if (this.ws !== ws) return; // superseded by a completed handoff
      this.ws = null;
      this._sessionLost("error");
      this._scheduleReconnect();
    };
    ws.on("close", drop);
    ws.on("error", drop);
  }

  // The session is gone, so every subscription on it is too.
  _sessionLost(reason) {
    clearTimeout(this.watchdogTimer);
    this.watchdogTimer = null;
    this.sessionId = null;
    this.subs.clear();
    for (const source of this.attached.values()) {
      if (source.esub.active || source.esub.reason === "connecting") source.setEsub({ active: false, reason });
    }
  }

  _close() {
    clearTimeout(this.watchdogTimer);
    clearTimeout(this.reconnectTimer);
    this.watchdogTimer = this.reconnectTimer = null;
    this.reconnectDelay = RECONNECT_BASE_MS;
    for (const s of [this.ws, this.reconnectSocket]) {
      if (!s) continue;
      try {
        s.removeAllListeners();
        s.on("error", () => {});
        s.close();
      } catch {
        /* ignore */
      }
    }
    this.ws = this.reconnectSocket = null;
    this.sessionId = null;
    this.subs.clear();
  }

  _scheduleReconnect() {
    if (this.reconnectTimer || this.attached.size === 0) return;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (this.attached.size) this._connect();
    }, this.reconnectDelay);
    this.reconnectTimer.unref?.();
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
  }

  _resetWatchdog() {
    clearTimeout(this.watchdogTimer);
    this.watchdogTimer = setTimeout(() => this._onWatchdog(), (this.keepaliveSec || 10) * 1000 + WELCOME_GRACE_MS);
    this.watchdogTimer.unref?.();
  }

  // Nothing (not even a keepalive) within keepalive_timeout + grace: assume the connection is dead.
  _onWatchdog() {
    const ws = this.ws;
    this.ws = null;
    if (ws) {
      try {
        ws.removeAllListeners();
        ws.on("error", () => {});
        ws.terminate();
      } catch {
        /* ignore */
      }
    }
    this._sessionLost("error");
    this._scheduleReconnect();
  }

  _onMessage(ws, data) {
    let msg;
    try {
      msg = JSON.parse(data.toString());
    } catch {
      return;
    }
    const type = msg?.metadata?.message_type;
    if (!type) return;
    // Traffic on the adopted socket resets the watchdog; a handoff socket only counts once adopted.
    if (ws === this.ws) this._resetWatchdog();
    if (type === "session_welcome") this._onWelcome(ws, msg);
    else if (type === "notification") {
      if (ws === this.ws) this._onNotification(msg);
    } else if (type === "session_reconnect") this._onSessionReconnect(msg);
    else if (type === "revocation") this._onRevocation(msg);
  }

  async _onWelcome(ws, msg) {
    const session = msg.payload?.session;
    if (!session?.id) return;
    if (ws.__reconnect) return this._completeHandoff(ws, session);
    this.sessionId = session.id;
    this.keepaliveSec = Number(session.keepalive_timeout_seconds) || 10;
    this.reconnectDelay = RECONNECT_BASE_MS;
    this._resetWatchdog();
    // Twitch drops a session that has not subscribed within 10s of the welcome, so go in parallel.
    await Promise.all([...this.attached.values()].map((source) => this._subscribeSource(source)));
  }

  async _subscribeSource(source) {
    const id = source.channelId;
    const sessionId = this.sessionId;
    const rec = auth.current();
    if (!sessionId || !rec || this.attached.get(id) !== source) return;
    const acc = await getAccess(id);
    if (!(acc.isBroadcaster || acc.isMod === true) || !hasRequiredScopes(rec.scopes)) return;
    if (!this.subs.has(id)) this.subs.set(id, new Map());
    const have = this.subs.get(id);

    const wanted = MOD_SUBS.filter((s) => !have.has(s.type)).map((s) => ({ ...s, cond: { broadcaster_user_id: id, moderator_user_id: rec.user.id }, core: true }));
    if (acc.isBroadcaster) {
      if ((rec.scopes.includes("channel:read:redemptions") || rec.scopes.includes("channel:manage:redemptions")) && !have.has(REDEEM_SUB.type))
        wanted.push({ ...REDEEM_SUB, cond: { broadcaster_user_id: id }, core: false });
      if (rec.scopes.includes("bits:read") && !have.has(POWER_UP_SUB.type)) wanted.push({ ...POWER_UP_SUB, cond: { broadcaster_user_id: id }, core: false });
    }
    let total = 0;
    for (const m of this.subs.values()) total += m.size;
    if (total + wanted.length > MAX_SUBSCRIPTIONS) {
      log("eventsub: subscription cap reached, not subscribing", source.login);
      return source.setEsub({ active: false, reason: "error" });
    }

    const results = await Promise.allSettled(
      wanted.map((s) =>
        helix("/eventsub/subscriptions", {
          method: "POST",
          body: { type: s.type, version: s.version, condition: s.cond, transport: { method: "websocket", session_id: sessionId } },
        })
      )
    );
    if (this.sessionId !== sessionId || this.attached.get(id) !== source) return;
    let denied = false;
    let limited = false;
    let coreFailed = false;
    results.forEach((r, i) => {
      const s = wanted[i];
      if (r.status === "fulfilled") {
        have.set(s.type, r.value?.data?.[0]?.id || "");
        return;
      }
      const err = r.reason;
      const status = err instanceof HttpError ? err.status : 0;
      if (status === 409) return void have.set(s.type, ""); // already subscribed on this session
      log(`eventsub: ${s.type} for ${source.login} failed (${status}) ${err?.message}`);
      if (!s.core) return; // redeems are a bonus
      coreFailed = true;
      if (status === 401 || status === 403) denied = true;
      if (status === 429 || (status === 400 && /limit/i.test(err.message))) limited = true;
    });
    if (!coreFailed && MOD_SUBS.every((s) => have.has(s.type))) {
      source.setEsub({ active: true, reason: null }, { namedFeed: true });
      if (acc.isBroadcaster) source.refreshRewards?.();
    } else if (denied && !limited) {
      source.setEsub({ active: false, reason: "relogin" });
      invalidateAccess(); // maybe the mod grant is gone
    } else {
      source.setEsub({ active: false, reason: "error" });
      this._retryLater(source);
    }
  }

  _onNotification(msg) {
    const id = msg.metadata?.message_id;
    if (id) {
      if (this.seen.has(id)) return; // Twitch may redeliver
      this.seen.add(id);
      this.seenOrder.push(id);
      if (this.seenOrder.length > DEDUPE_CAP) this.seen.delete(this.seenOrder.shift());
    }
    const sub = msg.payload?.subscription;
    const event = msg.payload?.event;
    if (!sub || !event) return;
    const source = this.attached.get(String(sub.condition?.broadcaster_user_id || ""));
    if (source) dispatch(source, sub.type, event);
  }

  _onSessionReconnect(msg) {
    const url = msg.payload?.session?.reconnect_url;
    if (!url || this.reconnectSocket) return;
    let ws;
    try {
      ws = this.createWebSocket(url);
    } catch {
      return; // the old socket stays alive; its own close drives a normal reconnect
    }
    ws.__reconnect = true;
    this.reconnectSocket = ws;
    this._bind(ws);
  }

  // Adopt the handoff socket once ITS welcome arrives, then close the old one. Subscriptions
  // carry over on Twitch's side: do NOT subscribe again.
  _completeHandoff(newWs, session) {
    const old = this.ws;
    this.sessionId = session.id;
    this.keepaliveSec = Number(session.keepalive_timeout_seconds) || this.keepaliveSec;
    newWs.__reconnect = false;
    this.ws = newWs;
    this.reconnectSocket = null;
    this._resetWatchdog();
    if (old) {
      try {
        old.removeAllListeners();
        old.on("error", () => {});
        old.close();
      } catch {
        /* ignore */
      }
    }
  }

  _onRevocation(msg) {
    const sub = msg.payload?.subscription;
    if (!sub?.type) return;
    const id = String(sub.condition?.broadcaster_user_id || "");
    const source = this.attached.get(id);
    this.subs.get(id)?.delete(sub.type);
    if (sub.status === "authorization_revoked") void auth.validate(); // the token itself may be dead: validate refreshes or logs out
    if (!source || !MOD_SUBS.some((s) => s.type === sub.type)) return;
    if (sub.status === "moderator_removed") invalidateAccess();
    source.setEsub({ active: false, reason: sub.status === "moderator_removed" ? null : "relogin" });
  }
}
