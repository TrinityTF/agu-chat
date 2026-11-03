// Kick platform module (read-only). See docs/ARCHITECTURE.md ("Platform module").
//
// HOW CHAT GETS HERE. The agu.lol site's relay uses Kick's webhooks, which need a public URL and cannot work in a
// desktop app. Kick's own web client reads chat over a public Pusher websocket instead, and so does this:
//   1. GET  kick.com/api/v2/channels/<slug>          -> channel id, chatroom id, live status (honest UA)
//   2. POST web.kick.com/api/v1/realtime/channels/<id>/chat/connection
//                                                    -> the CURRENT Pusher app key + cluster (not hard-coded in
//                                                       Kick's JS any more; fetched on every connect)
//   3. wss://ws-<cluster>.pusher.com/app/<key>      -> subscribe chatrooms.<chatroomId>.v2 and channel.<channelId>
// Everything is unauthenticated and read-only, and every request carries the honest product UA (Kick's WAF
// blocks browser UAs from a Node TLS stack; we never impersonate a browser).
//
// Emits the frames /ws/kick sends — chatlog {source, status, emotes, messages}, chat {message}, status {status},
// emotes {emotes}, banned {...} — plus the uniform conn frame, plus three additions the site's renderer does not
// handle yet: {type:"delete", ids}, {type:"clear"}, {type:"pin", message|null}.
//
// SENDING IS NOT SUPPORTED. docs.kick.com (OAuth 2.1, checked 2026-11-03) lists client_secret as REQUIRED on the
// authorization_code token exchange (alongside the PKCE code_verifier), so there is no public-client PKCE-only
// flow, and a secret cannot live in a desktop app. Hence no routes and no accounts.
import WebSocket from "ws";
import { randomUUID } from "node:crypto";
import { parseKickSource, getKickChannel, getRealtimeDescriptor, fetchHistory, toKickChatMsg, fetchKickEmotes, subBadgeFor } from "./vendor/kick.js";

export const platform = "kick";

const MAX_BACKLOG = 300;
// Kick's chat works whether or not the channel is live, so the socket stays up; this only keeps the header's
// live pill, title, category and viewer count honest (and catches a missed StreamerIsLive event).
const STATUS_POLL_MS = 60_000;
const NOT_FOUND_RETRY_MS = 2 * 60_000;
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 60_000;
// Pusher's server asks for activity within ~120s. We ping when quiet and reconnect when a ping goes unanswered.
const PING_AFTER_MS = 60_000;
const DEAD_AFTER_MS = 150_000;
// Only a fallback for a failed descriptor fetch: the descriptor is authoritative (observed 2026-11-03).
const FALLBACK_DESCRIPTOR = { appKey: "34bf7a0ff419a2a775b9", cluster: "us2" };
const PUSHER_VERSION = "8.4.0";

const READ_ONLY_REASON = "Kick sending needs a server-side app secret; read-only in this beta";

let lastDescriptor = null;

export function normalizeChannel(input) {
  const slug = parseKickSource(input);
  if (!slug) throw new Error("Enter a Kick channel name or a kick.com link.");
  return slug;
}

export function createSource({ channel, emit, ctx }) {
  const slug = normalizeChannel(channel);
  const ac = new AbortController();
  const clientId = randomUUID();

  let info = null; // getKickChannel result
  let status = { userId: "", slug, live: false, title: "", category: "", viewers: 0, url: `https://kick.com/${slug}`, error: null };
  let backlog = [];
  let emotes = {};
  let subBadges = [];
  let ws = null;
  let phase = "connecting"; // connecting | open | unavailable | error
  let phaseDetail = "Connecting to Kick...";
  let reconnectDelay = RECONNECT_BASE_MS;
  let reconnectTimer = null;
  let statusTimer = null;
  let watchdog = null;
  let lastMsgAt = 0;
  let pingedAt = 0;
  let failures = 0;
  let stopped = false;
  let lastConnJson = "";

  function connFrame() {
    let detail = phaseDetail;
    if (phase === "open") detail = status.live ? "Live" : "Not live (chat is connected)";
    return {
      type: "conn",
      state: phase,
      detail,
      live: !!status.live,
      title: status.title || undefined,
      canSend: false,
      readOnlyReason: READ_ONLY_REASON,
    };
  }

  function publishConn() {
    const f = connFrame();
    const json = JSON.stringify(f);
    if (json === lastConnJson) return;
    lastConnJson = json;
    emit(f);
  }

  function setPhase(next, detail = "") {
    phase = next;
    phaseDetail = detail;
    publishConn();
  }

  function chatlogFrame() {
    return { type: "chatlog", source: { userId: status.userId, slug: status.slug }, status, emotes, messages: backlog };
  }

  function setStatus(next) {
    const merged = { ...status, ...next };
    const same = Object.keys(merged).every((k) => merged[k] === status[k]);
    status = merged;
    if (!same) emit({ type: "status", status });
    publishConn();
  }

  /** Put the channel's own artwork on a subscriber badge; everything else keeps its chip. */
  function dressBadges(msg) {
    if (!subBadges.length || !msg.badges.length) return msg;
    msg.badges = msg.badges.map((b) => (b.type === "subscriber" ? { ...b, url: subBadgeFor(subBadges, b.count) } : b));
    return msg;
  }

  function pushBacklog(msg) {
    backlog.push(msg);
    if (backlog.length > MAX_BACKLOG) backlog = backlog.slice(-MAX_BACKLOG);
  }

  const opts = () => ({ userAgent: ctx.userAgent, signal: ac.signal });

  // ---------- channel + status ----------

  function applyChannel(c) {
    info = c;
    subBadges = c.subBadges || subBadges;
    setStatus({ userId: c.userId, slug: c.slug || slug, live: c.live, title: c.title, category: c.category, viewers: c.viewers, url: `https://kick.com/${c.slug || slug}`, error: null });
  }

  async function refreshStatus() {
    try {
      const c = await getKickChannel(slug, opts());
      if (stopped || !c) return;
      applyChannel(c);
    } catch (e) {
      // Status is decoration; the socket keeps working without it.
      if (!stopped) ctx.log(`[kick:${slug}] status refresh failed: ${(e && e.message) || e}`);
    }
  }

  function scheduleStatusPoll() {
    clearTimeout(statusTimer);
    statusTimer = setTimeout(async () => {
      statusTimer = null;
      await refreshStatus();
      if (!stopped) scheduleStatusPoll();
    }, STATUS_POLL_MS);
  }

  // ---------- connect lifecycle ----------

  async function connect() {
    if (stopped) return;
    try {
      const c = await getKickChannel(slug, opts());
      if (stopped) return;
      if (!c) {
        setStatus({ error: `No Kick channel called "${slug}".` });
        setPhase("unavailable", `No Kick channel called "${slug}".`);
        reconnectTimer = setTimeout(() => {
          reconnectTimer = null;
          connect();
        }, NOT_FOUND_RETRY_MS);
        return;
      }
      if (!c.chatroomId) throw new Error("Kick returned a channel without a chat room");
      const firstTime = !info;
      applyChannel(c);

      if (firstTime) {
        // Backlog and emotes are decoration on a working chat: failures leave them empty.
        const [hist, em] = await Promise.all([fetchHistory(c.channelId, opts()).catch(() => []), fetchKickEmotes(slug, opts()).catch(() => ({}))]);
        if (stopped) return;
        emotes = em;
        const seen = new Set(backlog.map((m) => m.id));
        for (const raw of hist) {
          const m = dressBadges(toKickChatMsg(raw));
          if (!m.id || seen.has(m.id)) continue;
          seen.add(m.id);
          pushBacklog(m);
        }
        emit(chatlogFrame());
      }

      let desc;
      try {
        desc = await getRealtimeDescriptor(c.channelId, opts(), clientId);
        lastDescriptor = desc;
      } catch (e) {
        if (stopped) return;
        // A failed descriptor lookup falls back to the last key that worked (or the one observed when this was
        // written) rather than leaving chat dark over a hiccup on a side endpoint.
        ctx.log(`[kick:${slug}] realtime descriptor failed, using last known key: ${(e && e.message) || e}`);
        desc = lastDescriptor || FALLBACK_DESCRIPTOR;
      }
      openSocket(c, desc);
    } catch (e) {
      if (stopped) return;
      failures++;
      const msg = (e && e.message) || "Kick unreachable";
      ctx.log(`[kick:${slug}] connect failed (${failures}): ${msg}`);
      setStatus({ error: msg });
      setPhase("error", msg);
      scheduleReconnect();
    }
  }

  function scheduleReconnect() {
    if (stopped || reconnectTimer) return;
    // Jitter so sources don't retry in lockstep after a shared network blip.
    const jitter = Math.floor(Math.random() * Math.min(1000, Math.max(1, reconnectDelay / 4)));
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, reconnectDelay + jitter);
    reconnectDelay = Math.min(reconnectDelay * 2, RECONNECT_MAX_MS);
  }

  function openSocket(c, desc) {
    const url = `wss://ws-${desc.cluster}.pusher.com/app/${desc.appKey}?protocol=7&client=js&version=${PUSHER_VERSION}&flash=false`;
    const sock = new WebSocket(url, { headers: { "User-Agent": ctx.userAgent } });
    ws = sock;
    lastMsgAt = Date.now();
    pingedAt = 0;
    const room = `chatrooms.${c.chatroomId}.v2`;
    const chan = `channel.${c.channelId}`;

    sock.on("message", (raw) => {
      if (ws !== sock || stopped) return;
      lastMsgAt = Date.now();
      pingedAt = 0;
      let m;
      try {
        m = JSON.parse(String(raw));
      } catch {
        return;
      }
      handlePusher(sock, m, { room, chan });
    });
    sock.on("close", () => {
      if (ws !== sock) return;
      ws = null;
      clearInterval(watchdog);
      watchdog = null;
      if (stopped) return;
      setPhase("connecting", "Reconnecting to Kick chat...");
      scheduleReconnect();
    });
    // 'error' is always followed by 'close'; handling it only stops Node treating it as unhandled.
    sock.on("error", (e) => {
      if (!stopped) ctx.log(`[kick:${slug}] socket error: ${(e && e.message) || e}`);
    });

    clearInterval(watchdog);
    watchdog = setInterval(() => {
      if (ws !== sock) return;
      const quiet = Date.now() - lastMsgAt;
      if (quiet > DEAD_AFTER_MS) {
        ctx.log(`[kick:${slug}] silent for ${Math.round(quiet / 1000)}s, reconnecting`);
        sock.terminate();
      } else if (quiet > PING_AFTER_MS && !pingedAt && sock.readyState === WebSocket.OPEN) {
        pingedAt = Date.now();
        sock.send(JSON.stringify({ event: "pusher:ping", data: {} }));
      }
    }, 15_000);
  }

  function handlePusher(sock, m, { room, chan }) {
    const ev = m && m.event;
    if (!ev) return;
    if (ev === "pusher:connection_established") {
      sock.send(JSON.stringify({ event: "pusher:subscribe", data: { auth: "", channel: room } }));
      sock.send(JSON.stringify({ event: "pusher:subscribe", data: { auth: "", channel: chan } }));
      return;
    }
    if (ev === "pusher:ping") {
      sock.send(JSON.stringify({ event: "pusher:pong", data: {} }));
      return;
    }
    if (ev === "pusher_internal:subscription_succeeded") {
      if (m.channel === room) {
        failures = 0;
        reconnectDelay = RECONNECT_BASE_MS;
        setPhase("open");
        if (!statusTimer) scheduleStatusPoll();
      }
      return;
    }
    if (ev === "pusher:error") {
      ctx.log(`[kick:${slug}] pusher error: ${typeof m.data === "string" ? m.data : JSON.stringify(m.data)}`);
      return;
    }

    let data = m.data;
    if (typeof data === "string") {
      try {
        data = JSON.parse(data);
      } catch {
        return;
      }
    }
    if (m.channel === chan) {
      // Livestream start/stop events: one authoritative read beats trusting a partial payload.
      if (/Stream|Live/i.test(ev)) void refreshStatus();
      return;
    }
    if (m.channel !== room || !data) return;

    switch (ev) {
      case "App\\Events\\ChatMessageEvent": {
        const msg = dressBadges(toKickChatMsg(data));
        if (!msg.id) return;
        pushBacklog(msg);
        emit({ type: "chat", message: msg });
        break;
      }
      case "App\\Events\\MessageDeletedEvent": {
        const id = String((data.message && data.message.id) || data.id || "");
        if (!id) return;
        for (const msg of backlog) if (msg.id === id) msg.deleted = true;
        emit({ type: "delete", ids: [id] });
        break;
      }
      case "App\\Events\\UserBannedEvent": {
        const user = data.user || {};
        const userId = String(user.id ?? "");
        if (!userId) return;
        // Kick, like Twitch, retroactively clears a banned user's messages from view.
        for (const msg of backlog) if (msg.user.id === userId) msg.deleted = true;
        const expiresAt = data.expires_at ? Date.parse(data.expires_at) : 0;
        emit({
          type: "banned",
          userId,
          username: user.username || "",
          by: (data.banned_by && data.banned_by.username) || "",
          reason: "",
          // No expiry (or an explicit permanent flag) is a permanent ban; a real one is a timeout.
          permanent: !!data.permanent || !expiresAt,
          expiresAt: expiresAt || 0,
        });
        break;
      }
      case "App\\Events\\ChatroomClearEvent":
        for (const msg of backlog) msg.deleted = true;
        emit({ type: "clear" });
        break;
      case "App\\Events\\PinnedMessageCreatedEvent":
        emit({ type: "pin", message: data.message ? dressBadges(toKickChatMsg(data.message)) : null });
        break;
      case "App\\Events\\PinnedMessageDeletedEvent":
        emit({ type: "pin", message: null });
        break;
      default:
        // Kick ships new event types (polls, gifts, rewards) without warning; ignoring unknown ones is deliberate.
        break;
    }
  }

  return {
    start() {
      lastConnJson = JSON.stringify(connFrame());
      emit(connFrame());
      connect();
    },
    stop() {
      stopped = true;
      clearTimeout(reconnectTimer);
      clearTimeout(statusTimer);
      clearInterval(watchdog);
      reconnectTimer = statusTimer = watchdog = null;
      ac.abort();
      if (ws) {
        const sock = ws;
        ws = null;
        try {
          sock.terminate();
        } catch {
          /* already closed */
        }
      }
    },
    snapshot() {
      return [chatlogFrame(), connFrame()];
    },
  };
}
