// Rumble platform module. See docs/ARCHITECTURE.md ("Platform module") and render/README.md ("Rumble co-chat").
//
// Reading: Rumble has no chat WebSocket. Its own player reads Server-Sent Events from
//   GET https://<front-end host>.rumble.com/chat/api/chat/<chatId>/stream
// which is unauthenticated. Chat is per VIDEO, so a source owns re-resolving which video its channel is live
// on and swapping the SSE connection underneath. An offline channel keeps polling and reattaches by itself.
//
// HARD RULES (do not "fix" these): only front-end hosts (wn0, then web7 for chat), never the rumble.com apex;
// never try to get past a Cloudflare challenge (no TLS impersonation, no scripted browsers, no browser UA).
//
// Emits the frames /ws/rumble sends: chatlog, chat {msg}, status, delete, mute, pin, emotes, badges; plus the
// uniform conn frame. Sending and moderating go through the user's OWN Rumble login (see ./vendor/rumbleAuth.js);
// only the u_s session is kept, in ctx.secrets.
import {
  parseRumbleSource,
  rumbleSourceKey,
  rumbleSourceUrl,
  resolveRumbleSource,
  fetchRumbleEmotes,
  toRumbleChatMsg,
  fetchCanModerate,
  rumbleFetch,
} from "./vendor/rumble.js";
import { hostsFor, markHost } from "./vendor/rumbleHosts.js";
import * as rumbleAuth from "./vendor/rumbleAuth.js";
import { setUserAgent } from "./ua.js";

export const platform = "rumble";

const MAX_BACKLOG = 200;
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 60_000;
// Rumble's channel page is a ~500 KB fetch, so an idle source re-checks slowly.
const OFFLINE_POLL_MS = 60_000;
// A live source re-checks liveness rarely; the SSE ending is the primary "it's over" signal.
const LIVE_RECHECK_MS = 5 * 60_000;
// Rumble sends a `: ` heartbeat even when chat is quiet, so a long silence means a dead connection.
const IDLE_TIMEOUT_MS = 5 * 60_000;
const SECRET_KEY = "rumble";
const TWO_FACTOR_TTL_MS = 5 * 60_000;
const CAPS_TTL_MS = 5 * 60_000;

let appCtx = null;

export function init(ctx) {
  appCtx = ctx;
  setUserAgent(ctx.userAgent);
}

export function normalizeChannel(input) {
  const src = parseRumbleSource(input);
  if (!src) throw new Error("Enter a Rumble channel name, a rumble.com channel link, or a video link.");
  return src.kind === "video" ? src.id : `${src.space}/${src.id.toLowerCase()}`;
}

// ---------------------------------------------------------------------------
// Account (the user's own Rumble login)
// ---------------------------------------------------------------------------

const liveSources = new Map(); // normalized channel -> { chatId(), url(), refreshConn() }
let pending2fa = null; // { challenge, username, expiresAt } — in memory only, never persisted

function storedAccount() {
  try {
    const a = appCtx && appCtx.secrets.get(SECRET_KEY);
    return a && a.session ? a : null;
  } catch {
    return null;
  }
}

function refreshAllConns() {
  for (const s of liveSources.values()) s.refreshConn();
}

async function finishLogin(session, fallbackName) {
  const who = await rumbleAuth.whoami(session);
  const username = (who && who.username) || fallbackName;
  appCtx.secrets.set(SECRET_KEY, { session, username });
  appCtx.accountsChanged();
  refreshAllConns();
  return { connected: true, user: { id: "", login: username, name: username } };
}

export const accounts = {
  state() {
    const a = storedAccount();
    return a ? { connected: true, user: { id: "", login: a.username, name: a.username } } : { connected: false };
  },

  /** {username, password} -> {connected:true,user} | {twoFactor:true}; then {code} finishes a 2FA login. */
  async login(args = {}) {
    if (!appCtx) throw new Error("Rumble is not initialised.");
    if (args.code) {
      const p = pending2fa;
      if (!p || Date.now() > p.expiresAt) {
        pending2fa = null;
        throw new Error("That login attempt expired. Start again.");
      }
      const { session } = await rumbleAuth.completeTwoFactor(p.challenge, String(args.code).trim());
      pending2fa = null;
      return await finishLogin(session, p.username);
    }
    const username = String(args.username || "").trim();
    const password = String(args.password || "");
    if (!username || !password) throw new Error("Rumble username and password are required.");
    // The password lives only inside this call: hashed by rumbleAuth.login(), then dropped. It is never
    // stored or logged, and error messages come from Rumble itself.
    const result = await rumbleAuth.login(username, password);
    if (!result.ok) {
      pending2fa = { challenge: result.twoFactor, username, expiresAt: Date.now() + TWO_FACTOR_TTL_MS };
      return { twoFactor: true };
    }
    return await finishLogin(result.session, username);
  },

  cancel() {
    pending2fa = null;
  },

  async logout() {
    pending2fa = null;
    const a = storedAccount();
    if (a) await rumbleAuth.logout(a.session); // best effort; never blocks disconnecting
    try {
      appCtx.secrets.delete(SECRET_KEY);
    } catch {
      /* nothing stored */
    }
    refreshAllConns();
  },
};

// ---------------------------------------------------------------------------
// Routes: POST /api/rumble/send, POST /api/rumble/action, GET /api/rumble/caps
// ---------------------------------------------------------------------------

function httpError(status, message) {
  return Object.assign(new Error(message), { status });
}

// Same mapping as server.js: a refusal Rumble deliberately returned is a 403 the user should read, not a
// gateway failure.
function rumbleErrorStatus(e) {
  if (e && e.status) return e.status;
  if (e && (e.rumbleExpired || e.rumbleRefused)) return 403;
  return 502;
}

const capsCache = new Map(); // chatId -> { canModerate, at }

/** The chat id a SourceRef is live on: a running source's own answer (free), else one fresh resolve. */
async function chatIdFor(sourceRef) {
  let key;
  try {
    key = normalizeChannel(sourceRef && sourceRef.channel);
  } catch {
    return { chatId: null, url: "" };
  }
  const live = liveSources.get(key);
  if (live && live.chatId()) return { chatId: live.chatId(), url: live.url() };
  // No SSE stream is opened just to answer "what is the id": that would leak a connection per request.
  const res = await resolveRumbleSource(parseRumbleSource(key));
  return { chatId: res.live && res.chatId ? res.chatId : null, url: res.url || "" };
}

/** The three things every write needs: the user's session, and the chat id the channel is live on. */
async function requireActor(sourceRef) {
  const acct = storedAccount();
  if (!acct) throw httpError(403, "Connect your Rumble account in Settings first");
  const { chatId, url } = await chatIdFor(sourceRef);
  if (!chatId) throw httpError(409, "That Rumble channel isn't live right now");
  // The page a real client would be sitting on when it sends. Best-effort; a generic referer beats none.
  return { session: acct.session, chatId, referer: url || "https://rumble.com/" };
}

export const routes = {
  "POST /api/rumble/send": async ({ source, body }) => {
    const text = String((body && body.text) || "").trim();
    if (!text) return { status: 400, body: { error: "empty message" } };
    try {
      const g = await requireActor(source);
      await rumbleAuth.sendMessage(g.session, g.chatId, text, { referer: g.referer });
      return { status: 200, body: { ok: true } };
    } catch (e) {
      return { status: rumbleErrorStatus(e), body: { error: e.message || "Rumble rejected the message" } };
    }
  },

  "POST /api/rumble/action": async ({ source, body }) => {
    const b = body || {};
    const action = String(b.action || "");
    const username = String(b.username || "").trim();
    try {
      const g = await requireActor(source);
      if (action === "timeout") {
        if (!username) return { status: 400, body: { error: "missing username" } };
        // Clamp to a week so a typo can't become an effectively permanent mute dressed up as a timeout.
        const seconds = Math.min(Math.max(1, Math.floor(Number(b.seconds) || 300)), 604800);
        await rumbleAuth.timeoutUser(g.session, g.chatId, username, seconds);
      } else if (action === "ban") {
        if (!username) return { status: 400, body: { error: "missing username" } };
        await rumbleAuth.banUser(g.session, g.chatId, username);
      } else if (action === "unban") {
        if (!username) return { status: 400, body: { error: "missing username" } };
        await rumbleAuth.unbanUser(g.session, g.chatId, username);
      } else if (action === "delete") {
        const messageId = String(b.messageId || "");
        if (!messageId) return { status: 400, body: { error: "missing messageId" } };
        await rumbleAuth.deleteMessage(g.session, g.chatId, messageId);
      } else {
        return { status: 400, body: { error: "unknown action" } };
      }
      return { status: 200, body: { ok: true } };
    } catch (e) {
      return { status: rumbleErrorStatus(e), body: { error: e.message || "Rumble rejected the action" } };
    }
  },

  // connected and canModerate are answered independently: being logged in has nothing to do with whether
  // anyone is streaming. can_moderate needs an AUTHENTICATED read of the chat's init frame (the shared
  // anonymous stream always reports false), cached per chat id since it changes only on promotion.
  "GET /api/rumble/caps": async ({ source }) => {
    const acct = storedAccount();
    if (!acct) return { status: 200, body: { connected: false, canModerate: false } };
    const { chatId } = await chatIdFor(source).catch(() => ({ chatId: null }));
    if (!chatId) return { status: 200, body: { connected: true, canModerate: false } };
    const hit = capsCache.get(chatId);
    if (hit && Date.now() - hit.at < CAPS_TTL_MS) return { status: 200, body: { connected: true, canModerate: hit.canModerate } };
    const canModerate = await fetchCanModerate(acct.session, chatId);
    capsCache.set(chatId, { canModerate, at: Date.now() });
    return { status: 200, body: { connected: true, canModerate } };
  },
};

// ---------------------------------------------------------------------------
// The source
// ---------------------------------------------------------------------------

export function createSource({ channel, emit, ctx }) {
  appCtx = appCtx || ctx;
  setUserAgent(ctx.userAgent);
  const src = parseRumbleSource(channel);
  if (!src) throw new Error("Not a Rumble source: " + channel);
  const key = rumbleSourceKey(src);

  let status = { live: false, chatId: null, title: "", author: "", category: "", url: rumbleSourceUrl(src), watching: null, error: null };
  let backlog = [];
  let badges = {};
  let emotes = {};
  // Rumble only re-sends a user with the message that mentions them, so the side tables accumulate per chat id.
  const users = new Map();
  const channels = new Map();

  let streamState = "off"; // off | connecting | open
  let chatDisabled = false; // 204: a property of the video, reported once, not retried
  let abort = null;
  let reconnectDelay = RECONNECT_BASE_MS;
  let reconnectTimer = null;
  let resolveTimer = null;
  let idleTimer = null;
  let lastFrameAt = 0;
  let hostIndex = 0;
  let resolving = false;
  let resolveFailures = 0;
  let lastError = null;
  let stopped = false;
  let started = false;
  let resolvedOnce = false; // until the first answer (or failure), the source is still "connecting"
  let lastConnJson = "";

  function connFrame() {
    let state;
    let detail;
    if (!started || !resolvedOnce) {
      state = "connecting";
      detail = "Connecting to Rumble...";
    } else if (status.error && (chatDisabled || resolveFailures >= 3)) {
      state = chatDisabled ? "unavailable" : "error";
      detail = status.error;
    } else if (!status.live) {
      state = "offline";
      detail = "Not live";
    } else if (streamState === "open") {
      state = "open";
      detail = "Live";
    } else {
      state = "connecting";
      detail = "Connecting to Rumble chat...";
    }
    const connected = !!storedAccount();
    const canSend = connected && !!status.live && !chatDisabled;
    const f = { type: "conn", state, detail, live: !!status.live, title: status.title || undefined, canSend };
    if (!canSend) f.readOnlyReason = !connected ? "Connect your Rumble account to chat." : chatDisabled ? status.error : "Not live";
    return f;
  }
  function publishConn() {
    const f = connFrame();
    const json = JSON.stringify(f);
    if (json === lastConnJson) return;
    lastConnJson = json;
    emit(f);
  }

  function chatlogFrame() {
    return { type: "chatlog", source: { key, kind: src.kind, id: src.id, space: src.space || null }, status, badges, emotes, messages: backlog };
  }

  function broadcast(frame) {
    if (!stopped) emit(frame);
  }

  function pushBacklog(msg) {
    backlog.push(msg);
    if (backlog.length > MAX_BACKLOG) backlog = backlog.slice(-MAX_BACKLOG);
  }

  function setStatus(next) {
    const merged = { ...status, ...next };
    const same =
      merged.live === status.live &&
      merged.chatId === status.chatId &&
      merged.title === status.title &&
      merged.watching === status.watching &&
      merged.error === status.error &&
      merged.canModerate === status.canModerate;
    status = merged;
    if (!same) broadcast({ type: "status", status });
    publishConn();
  }

  async function loadEmotes(chatId) {
    const found = await fetchRumbleEmotes(chatId).catch(() => ({}));
    if (stopped || status.chatId !== chatId) return; // moved on to another broadcast
    emotes = found;
    if (Object.keys(found).length) broadcast({ type: "emotes", emotes: found });
  }

  // Badge definitions change only when Rumble ships new artwork, so broadcast on change.
  function setBadges(next) {
    if (!next || typeof next !== "object") return;
    if (JSON.stringify(next) === JSON.stringify(badges)) return;
    badges = next;
    broadcast({ type: "badges", badges });
  }

  // ---------- resolution lifecycle ----------

  async function ensureResolved() {
    if (resolving || stopped) return;
    resolving = true;
    try {
      const res = await resolveRumbleSource(src);
      if (stopped) return;
      resolveFailures = 0;
      resolvedOnce = true;
      const changed = res.chatId !== status.chatId;
      if (changed) chatDisabled = false;
      setStatus({
        live: !!res.live,
        chatId: res.chatId || null,
        title: res.title || "",
        // Only a live stream's category means anything; between broadcasts there is no "current" one.
        category: res.live ? res.category || "" : "",
        author: res.author || "",
        url: res.url || status.url,
        watching: res.watching ?? null,
        error: chatDisabled ? status.error : null,
      });

      if (!res.live || !res.chatId) {
        // Between broadcasts: drop the upstream stream but KEEP the backlog, so a viewer who was reading
        // when the stream ended still sees the last messages.
        closeStream();
      } else if (changed || streamState === "off") {
        if (changed) {
          // A new broadcast: the old chat's users/messages belong to a different video.
          users.clear();
          channels.clear();
          backlog = [];
          badges = {};
          emotes = {};
          broadcast(chatlogFrame());
        }
        closeStream();
        openStream();
        void loadEmotes(res.chatId);
      }
    } catch (err) {
      if (stopped) return;
      resolveFailures++;
      lastError = String((err && err.message) || err);
      resolvedOnce = true;
      // Surface only once it has persisted, so one bad fetch doesn't flash an error at the user.
      if (resolveFailures >= 3) setStatus({ error: lastError });
      else publishConn();
    } finally {
      resolving = false;
      scheduleResolve();
    }
  }

  function scheduleResolve() {
    clearTimeout(resolveTimer);
    resolveTimer = null;
    if (stopped) return;
    // Back off hard on repeated failures so a renamed/deleted channel doesn't become a fetch loop.
    const base = status.live ? LIVE_RECHECK_MS : OFFLINE_POLL_MS;
    const delay = resolveFailures > 0 ? Math.min(base * 2 ** Math.min(resolveFailures, 4), 15 * 60_000) : base;
    resolveTimer = setTimeout(() => {
      resolveTimer = null;
      ensureResolved();
    }, delay);
  }

  // ---------- SSE lifecycle ----------

  function openStream() {
    if (streamState !== "off" || !status.chatId || stopped || chatDisabled) return;
    streamState = "connecting";
    const ctl = new AbortController();
    abort = ctl;
    publishConn();
    readStream(status.chatId, ctl).catch((err) => {
      if (ctl.signal.aborted) return; // our own teardown
      lastError = String((err && err.message) || err);
      onStreamEnd();
    });
  }

  function closeStream() {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
    clearInterval(idleTimer);
    idleTimer = null;
    const ctl = abort;
    abort = null;
    streamState = "off";
    if (ctl) {
      try {
        ctl.abort();
      } catch {
        /* already torn down */
      }
    }
  }

  function onStreamEnd() {
    if (streamState === "off") return;
    closeStream();
    publishConn();
    scheduleReconnect();
  }

  function scheduleReconnect() {
    if (stopped || !status.live || chatDisabled || reconnectTimer) return;
    // Jitter so sources don't retry in lockstep after a shared network blip.
    const jitter = Math.floor(Math.random() * Math.min(1000, Math.max(1, reconnectDelay / 4)));
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      // Move to the next host on every retry: a closed host, or a 404 from one front-end for a chat id another
      // serves happily, is exactly the failure the host list exists for.
      hostIndex++;
      openStream();
    }, reconnectDelay + jitter);
    reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
  }

  // Reads one SSE connection to completion. Rumble never closes a healthy stream, so returning at all means the
  // broadcast ended or the connection broke — both handled by the caller's onStreamEnd.
  async function readStream(chatId, ctl) {
    const hosts = hostsFor("chat");
    const host = hosts[hostIndex % hosts.length];
    const url = `https://${host}.rumble.com/chat/api/chat/${encodeURIComponent(chatId)}/stream`;
    let res;
    try {
      // timeoutMs: 0 — the whole point is a connection that stays open; teardown is the AbortController's.
      res = await rumbleFetch(url, { headers: { Accept: "text/event-stream" }, signal: ctl.signal, timeoutMs: 0 });
    } catch (e) {
      if (!ctl.signal.aborted) markHost(host, "chat", false);
      throw e;
    }
    if (res.ok) {
      markHost(host, "chat", true);
      hostIndex = 0;
    }

    // 204 is Rumble's "this video has no chat": a property of the video, so report it and stop rather than retry.
    if (res.status === 204) {
      chatDisabled = true;
      streamState = "off";
      setStatus({ error: "Chat is disabled for this stream." });
      return;
    }
    if (!res.ok || !res.body) throw new Error(`rumble stream ${res.status}`);

    streamState = "open";
    reconnectDelay = RECONNECT_BASE_MS;
    lastFrameAt = Date.now();
    startIdleWatchdog(ctl);
    publishConn();

    const decoder = new TextDecoder();
    let buffer = "";
    for await (const chunk of res.body) {
      if (ctl.signal.aborted) return;
      lastFrameAt = Date.now();
      buffer += decoder.decode(chunk, { stream: true });
      // SSE frames are separated by a blank line; anything after the last separator is a partial frame.
      let sep;
      while ((sep = buffer.indexOf("\n\n")) >= 0) {
        const frame = buffer.slice(0, sep);
        buffer = buffer.slice(sep + 2);
        handleFrame(frame);
      }
      if (buffer.length > 1_000_000) buffer = ""; // an unbounded line must not grow this without limit
    }
    throw new Error("rumble stream ended");
  }

  function startIdleWatchdog(ctl) {
    clearInterval(idleTimer);
    idleTimer = setInterval(() => {
      if (ctl.signal.aborted) return;
      if (Date.now() - lastFrameAt < IDLE_TIMEOUT_MS) return;
      ctx.log(`[rumble] ${key}: idle ${IDLE_TIMEOUT_MS}ms, reconnecting`);
      onStreamEnd();
    }, 30_000);
  }

  function handleFrame(frame) {
    // Lines starting with ':' are heartbeat comments and carry nothing.
    const dataLines = [];
    for (const line of frame.split("\n")) if (line.startsWith("data:")) dataLines.push(line.slice(5).trimStart());
    if (!dataLines.length) return;

    let event;
    try {
      event = JSON.parse(dataLines.join("\n"));
    } catch {
      return; // a truncated or non-JSON frame is not worth killing the stream
    }
    if (!event || !event.data) return;

    // Side tables first: a message in this same frame may reference them.
    for (const u of event.data.users || []) if (u && u.id != null) users.set(String(u.id), u);
    for (const c of event.data.channels || []) if (c && c.id != null) channels.set(String(c.id), c);

    switch (event.type) {
      case "init":
        // Rumble ships badge artwork and labels with the chat itself (icon paths are relative to rumble.com).
        if (event.data.config && event.data.config.badges) setBadges(event.data.config.badges);
        seedBacklog(event.data.messages || []);
        setStatus({ canModerate: !!event.data.can_moderate });
        break;
      case "messages":
        for (const raw of event.data.messages || []) {
          const msg = toRumbleChatMsg(raw, users, channels);
          pushBacklog(msg);
          broadcast({ type: "chat", msg });
        }
        break;
      case "delete_messages":
        applyDeletes(event.data.message_ids || []);
        break;
      case "mute_users":
        // Rumble hides every past message from a muted user rather than deleting individual ones.
        applyMutes(event.data.user_ids || []);
        break;
      case "pin_message":
        broadcast({ type: "pin", msg: event.data.message ? toRumbleChatMsg(event.data.message, users, channels) : null });
        break;
      case "unpin_message":
        broadcast({ type: "pin", msg: null });
        break;
      default:
        // Rumble ships new event types (raids, gifted subs, live gates) without warning; ignoring the unknown
        // ones is the whole reason this is a switch and not an assertion.
        break;
    }
  }

  function seedBacklog(rawMessages) {
    const seen = new Set(backlog.map((m) => m.id));
    let added = 0;
    for (const raw of rawMessages) {
      const msg = toRumbleChatMsg(raw, users, channels);
      if (seen.has(msg.id)) continue;
      seen.add(msg.id);
      pushBacklog(msg);
      added++;
    }
    backlog.sort((a, b) => a.ts - b.ts);
    if (backlog.length > MAX_BACKLOG) backlog = backlog.slice(-MAX_BACKLOG);
    // History lands after the subscriber's hello, so re-send the whole log once it arrives. Doubles as the
    // resync after a reconnect or a swap to a new broadcast; silent when nothing new came.
    if (added > 0) broadcast(chatlogFrame());
  }

  function applyDeletes(ids) {
    const set = new Set(ids.map(String));
    if (!set.size) return;
    for (const msg of backlog) if (set.has(msg.id)) msg.deleted = true;
    broadcast({ type: "delete", ids: [...set] });
  }

  function applyMutes(userIds) {
    const set = new Set(userIds.map(String));
    if (!set.size) return;
    for (const msg of backlog) if (set.has(msg.user.id)) msg.deleted = true;
    broadcast({ type: "mute", userIds: [...set] });
  }

  const handle = {
    chatId: () => (status.live ? status.chatId : null),
    url: () => status.url,
    refreshConn: () => {
      if (!stopped) publishConn();
    },
  };

  return {
    start() {
      started = true;
      liveSources.set(normalizeChannel(channel), handle);
      lastConnJson = JSON.stringify(connFrame());
      emit(connFrame());
      ensureResolved();
    },
    stop() {
      stopped = true;
      if (liveSources.get(normalizeChannel(channel)) === handle) liveSources.delete(normalizeChannel(channel));
      closeStream();
      clearTimeout(resolveTimer);
      resolveTimer = null;
    },
    snapshot() {
      return [chatlogFrame(), connFrame()];
    },
  };
}
