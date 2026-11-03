// ONE anonymous Twitch IRC connection (wss://irc-ws.chat.twitch.tv, justinfanNNNN) shared by
// every Twitch source in the app: JOIN / PART per channel. The connection-health logic is
// render/lib/ircRelay.js's (connect timeout, join timeout via ROOMSTATE, idle watchdog,
// channel-idle re-JOIN probe, jittered backoff 1s -> 60s), reshaped for several channels on
// one socket: a channel that will not join must not tear down the others' connection.
//
// A listener is {onJoined(roomId, {reconnect}), onDisconnected(reason), onChannelError(detail),
// onNotice(msgId, text), onLine(parsedIrcLine)}.
import { WebSocket } from "ws";
import { parseIrcLine } from "./vendor/irc.js";
import { log as ctxLog, userAgent } from "./rt.js";

export const IRC_URL = "wss://irc-ws.chat.twitch.tv:443";

const DEFAULTS = {
  connectTimeoutMs: 15_000,
  joinTimeoutMs: 15_000,
  watchdogIntervalMs: 30_000,
  idleTimeoutMs: 10 * 60_000, // Twitch PINGs even a quiet chat, so ten silent minutes mean a dead socket
  // Twitch can silently stop delivering a channel while the socket stays healthy (PINGs keep
  // coming, no PART, no RECONNECT). Re-JOIN after this much channel silence; a missing ROOMSTATE
  // reply is proof the membership is gone. A genuinely quiet channel pays one PART/JOIN pair.
  channelIdleMs: 5 * 60_000,
  probeTimeoutMs: 20_000,
  reconnectBaseMs: 1000,
  reconnectMaxMs: 60_000,
  joinSpacingMs: 550, // Twitch allows 20 JOINs per 10s for an anonymous user
  rejoinAfterErrorMs: 60_000,
};

// Inbound commands that prove Twitch is still routing a channel to us. PING and other
// tmi.twitch.tv-level frames are excluded on purpose: a zombied JOIN keeps receiving those.
const CHANNEL_SCOPED = new Set(["PRIVMSG", "USERNOTICE", "ROOMSTATE", "CLEARCHAT", "CLEARMSG", "NOTICE"]);

const errText = (e) => (e && e.message ? e.message : String(e));

export class IrcHub {
  constructor(opts = {}) {
    this.opts = { ...DEFAULTS, ...opts };
    this.createWebSocket = opts.createWebSocket || ((url) => new WebSocket(url, { headers: { "User-Agent": userAgent() } }));
    this.log = opts.log || ((...a) => ctxLog(...a));
    this.channels = new Map(); // login -> entry
    this.ws = null;
    this.state = "off"; // off | connecting | open
    this.nick = null;
    this.reconnectDelay = this.opts.reconnectBaseMs;
    this.reconnectTimer = null;
    this.connectTimer = null;
    this.watchdogTimer = null;
    this.joinQueue = [];
    this.pumpTimer = null;
    this.lastIrcAt = 0;
    this.stats = { connectAttempts: 0, reconnects: 0, staleResets: 0, lastDisconnect: null, lastNotice: null };
  }

  add(login, listener) {
    const old = this.channels.get(login);
    if (old) this._clearEntry(old);
    const entry = { login, listener, joined: false, wasJoined: false, joinFails: 0, joinTimer: null, retryTimer: null, lastChannelAt: 0, probeAt: 0, probeFails: 0 };
    this.channels.set(login, entry);
    if (this.state === "open") this._queueJoin(entry);
    else this._ensureConnected();
  }

  remove(login) {
    const entry = this.channels.get(login);
    if (!entry) return;
    this.channels.delete(login);
    this._clearEntry(entry);
    if (this.state === "open" && this.ws) this._send(`PART #${login}`);
    if (this.channels.size === 0) this.closeAll();
  }

  status() {
    return {
      state: this.state,
      channels: [...this.channels.values()].map((e) => ({ login: e.login, joined: e.joined, joinFails: e.joinFails, probeAt: e.probeAt || null })),
      ...this.stats,
    };
  }

  closeAll() {
    clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    const ws = this.ws;
    this.ws = null;
    this._clearSocketTimers();
    this._closeSocket(ws);
    this.state = "off";
    this.nick = null;
    this.joinQueue = [];
    for (const e of this.channels.values()) this._clearEntry(e, { keepFlags: false });
  }

  // ---------- connection ----------

  _ensureConnected() {
    if (this.state !== "off" || this.reconnectTimer || this.channels.size === 0) return;
    this.state = "connecting";
    this._connect();
  }

  _connect() {
    let ws;
    try {
      ws = this.createWebSocket(IRC_URL);
    } catch (err) {
      this.state = "off";
      this.stats.lastDisconnect = "connect threw: " + errText(err);
      this._notifyDisconnected(this.stats.lastDisconnect);
      this._scheduleReconnect();
      return;
    }
    this.ws = ws;
    this.stats.connectAttempts++;
    this.log("[irc] connecting");
    this.connectTimer = setTimeout(() => this._retire(ws, "WebSocket connect timeout", { stale: true }), this.opts.connectTimeoutMs);
    this.connectTimer.unref?.();

    ws.on("open", () => {
      if (this.ws !== ws) return this._closeSocket(ws);
      clearTimeout(this.connectTimer);
      this.connectTimer = null;
      this.lastIrcAt = Date.now();
      this.nick = "justinfan" + Math.floor(10000 + Math.random() * 80000);
      this.state = "open";
      try {
        ws.send("CAP REQ :twitch.tv/tags twitch.tv/commands");
        ws.send("NICK " + this.nick);
      } catch (err) {
        return this._retire(ws, "registration send failed: " + errText(err));
      }
      for (const entry of this.channels.values()) {
        entry.lastChannelAt = this.lastIrcAt; // baseline the membership clock at connect
        this._queueJoin(entry);
      }
      this._startWatchdog(ws);
    });
    ws.on("message", (data) => {
      if (this.ws !== ws) return;
      this.lastIrcAt = Date.now();
      this._onData(String(data), ws);
    });
    ws.on("close", (code, reason) => {
      const suffix = reason && reason.length ? `: ${reason.toString()}` : "";
      this._retire(ws, `WebSocket closed (${code ?? "unknown"})${suffix}`);
    });
    ws.on("error", (err) => this._retire(ws, "WebSocket error: " + errText(err)));
  }

  _send(line) {
    try {
      this.ws.send(line);
      return true;
    } catch (err) {
      this._retire(this.ws, "send failed: " + errText(err));
      return false;
    }
  }

  _closeSocket(ws) {
    if (!ws) return;
    try {
      if (typeof ws.terminate === "function") ws.terminate();
      else ws.close();
    } catch {
      /* already closed */
    }
  }

  _clearSocketTimers() {
    clearTimeout(this.connectTimer);
    clearInterval(this.watchdogTimer);
    clearTimeout(this.pumpTimer);
    this.connectTimer = this.watchdogTimer = this.pumpTimer = null;
  }

  _clearEntry(entry, { keepFlags = true } = {}) {
    clearTimeout(entry.joinTimer);
    clearTimeout(entry.retryTimer);
    entry.joinTimer = entry.retryTimer = null;
    entry.probeAt = 0;
    if (!keepFlags) entry.joined = false;
    this.joinQueue = this.joinQueue.filter((e) => e !== entry);
  }

  _notifyDisconnected(reason) {
    for (const entry of this.channels.values()) {
      entry.joined = false;
      this._clearEntry(entry);
      try {
        entry.listener.onDisconnected?.(reason);
      } catch (err) {
        this.log("[irc] listener threw in onDisconnected", err);
      }
    }
  }

  // The only failure/replacement path. Nulling the socket first makes error+close, late
  // timers and stale callbacks idempotent.
  _retire(ws, reason, { immediate = false, stale = false } = {}) {
    if (!ws || this.ws !== ws) return;
    this.ws = null;
    this._clearSocketTimers();
    this.state = "off";
    this.nick = null;
    this.joinQueue = [];
    this.stats.lastDisconnect = reason;
    if (stale) this.stats.staleResets++;
    this.log("[irc] " + reason);
    this._closeSocket(ws);
    this._notifyDisconnected(reason);
    this._scheduleReconnect(immediate ? Math.floor(Math.random() * 250) : null);
  }

  _scheduleReconnect(delayOverride = null) {
    if (this.channels.size === 0 || this.reconnectTimer) return;
    const base = delayOverride == null ? this.reconnectDelay : Math.max(0, delayOverride);
    // Jitter regular retries so nothing re-JOINs in the same instant after a shared outage.
    const jitter = delayOverride == null ? Math.floor(Math.random() * Math.min(1000, Math.max(1, base / 4))) : 0;
    this.stats.reconnects++;
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      this._ensureConnected();
    }, base + jitter);
    this.reconnectTimer.unref?.();
    this.reconnectDelay = Math.min(this.reconnectDelay * 2, this.opts.reconnectMaxMs);
  }

  // ---------- joining ----------

  _queueJoin(entry) {
    if (!this.joinQueue.includes(entry)) this.joinQueue.push(entry);
    this._pump();
  }

  _pump() {
    if (this.pumpTimer || this.state !== "open") return;
    const entry = this.joinQueue.shift();
    if (!entry) return;
    if (this.channels.get(entry.login) === entry) this._sendJoin(entry);
    if (this.joinQueue.length) {
      this.pumpTimer = setTimeout(() => {
        this.pumpTimer = null;
        this._pump();
      }, this.opts.joinSpacingMs);
      this.pumpTimer.unref?.();
    }
  }

  _sendJoin(entry) {
    const ws = this.ws;
    if (!this._send(`JOIN #${entry.login}`)) return;
    clearTimeout(entry.joinTimer);
    entry.joinTimer = setTimeout(() => this._joinTimeout(entry, ws), this.opts.joinTimeoutMs);
    entry.joinTimer.unref?.();
  }

  // Transport-open is not channel-open: Twitch confirms a JOIN with ROOMSTATE. No ROOMSTATE in
  // time -> try once more; a second miss is reported to the channel, and the whole socket is
  // retired only when nothing on it has joined (then the socket itself is the suspect).
  _joinTimeout(entry, ws) {
    entry.joinTimer = null;
    if (this.ws !== ws || entry.joined || this.channels.get(entry.login) !== entry) return;
    entry.joinFails++;
    if (entry.joinFails === 1) {
      this.log(`[irc:${entry.login}] join timeout, retrying`);
      if (this._send(`PART #${entry.login}`)) this._queueJoin(entry);
      return;
    }
    try {
      entry.listener.onChannelError?.(`Could not join #${entry.login}. The channel may not exist, or Twitch is not answering.`);
    } catch (err) {
      this.log("[irc] listener threw in onChannelError", err);
    }
    if (![...this.channels.values()].some((e) => e.joined)) {
      this._retire(ws, `channel join timeout (#${entry.login})`, { stale: true });
      return;
    }
    entry.retryTimer = setTimeout(() => {
      entry.retryTimer = null;
      if (this.ws === ws && this.channels.get(entry.login) === entry && !entry.joined) this._queueJoin(entry);
    }, this.opts.rejoinAfterErrorMs);
    entry.retryTimer.unref?.();
  }

  _markJoined(entry, roomId) {
    clearTimeout(entry.joinTimer);
    entry.joinTimer = null;
    entry.joined = true;
    entry.joinFails = 0;
    entry.probeFails = 0;
    entry.lastChannelAt = Date.now();
    this.reconnectDelay = this.opts.reconnectBaseMs;
    const reconnect = entry.wasJoined;
    entry.wasJoined = true;
    this.log(`[irc:${entry.login}] joined`);
    try {
      entry.listener.onJoined?.(roomId, { reconnect });
    } catch (err) {
      this.log("[irc] listener threw in onJoined", err);
    }
  }

  // ---------- watchdog ----------

  _startWatchdog(ws) {
    clearInterval(this.watchdogTimer);
    const { watchdogIntervalMs, idleTimeoutMs } = this.opts;
    if (!(watchdogIntervalMs > 0) || !(idleTimeoutMs > 0)) return;
    this.watchdogTimer = setInterval(() => {
      if (this.ws !== ws) return;
      const now = Date.now();
      const idle = now - this.lastIrcAt;
      if (idle > idleTimeoutMs) return this._retire(ws, `no inbound IRC traffic for ${Math.floor(idle / 1000)}s`, { stale: true });
      for (const entry of [...this.channels.values()]) {
        if (this.ws !== ws) return;
        if (entry.joined) this._checkMembership(entry, ws, now);
      }
    }, watchdogIntervalMs);
    this.watchdogTimer.unref?.();
  }

  _checkMembership(entry, ws, now) {
    const { channelIdleMs, probeTimeoutMs } = this.opts;
    if (!(channelIdleMs > 0)) return;
    if (entry.probeAt) {
      if (now - entry.probeAt <= probeTimeoutMs) return;
      entry.probeAt = 0;
      entry.probeFails++;
      // Another channel still delivering means the socket is fine and only this JOIN died.
      const othersAlive = [...this.channels.values()].some((e) => e !== entry && e.joined && now - e.lastChannelAt <= channelIdleMs);
      if (!othersAlive || entry.probeFails >= 2) return this._retire(ws, `channel membership probe went unanswered (#${entry.login})`, { stale: true });
      entry.joined = false;
      this._queueJoin(entry);
      return;
    }
    if (now - entry.lastChannelAt <= channelIdleMs) return;
    // Set before sending: the PART echo arrives on this same socket and must not read as an eviction.
    entry.probeAt = now;
    this.log(`[irc:${entry.login}] channel silent; re-JOINing to verify membership`);
    if (this._send(`PART #${entry.login}`)) this._send(`JOIN #${entry.login}`);
  }

  // ---------- inbound ----------

  _onData(data, ws) {
    for (const line of data.split("\r\n")) {
      if (!line) continue;
      if (line.startsWith("PING")) {
        // Twitch requires the PONG payload to match the PING payload.
        if (!this._send("PONG" + line.slice(4))) return;
        continue;
      }
      const m = parseIrcLine(line);
      const entry = m.channel ? this.channels.get(m.channel) : null;
      if (entry && CHANNEL_SCOPED.has(m.command)) entry.lastChannelAt = Date.now();
      if (m.command === "RECONNECT") return this._retire(ws, "Twitch requested RECONNECT", { immediate: true });
      if (m.command === "NOTICE") {
        this.stats.lastNotice = m.text || "IRC NOTICE";
        this.log(`[irc] NOTICE${m.channel ? " #" + m.channel : ""}: ${this.stats.lastNotice}`);
        if (entry) {
          try {
            entry.listener.onNotice?.(m.tags["msg-id"] || "", m.text || "");
          } catch (err) {
            this.log("[irc] listener threw in onNotice", err);
          }
        }
        continue;
      }
      if (!entry) continue;
      if (m.command === "ROOMSTATE") {
        entry.probeAt = 0;
        entry.probeFails = 0;
        if (!entry.joined) this._markJoined(entry, m.tags["room-id"] || null);
      } else if (m.command === "PART" && m.user === this.nick) {
        // A membership probe PARTs on purpose before re-JOINing, and a retry does too.
        if (entry.probeAt || !entry.joined) continue;
        return this._retire(ws, `Twitch removed us from #${entry.login}`, { immediate: true });
      }
      try {
        entry.listener.onLine?.(m);
      } catch (err) {
        this.log(`[irc:${entry.login}] listener threw`, err);
      }
      if (this.ws !== ws) return;
    }
  }
}
