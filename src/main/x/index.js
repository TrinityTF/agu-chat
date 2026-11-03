// X (Twitter) platform module (read-only). See docs/ARCHITECTURE.md ("Platform module").
//
// Reads x.com's live-chat stream (newline-delimited JSON, backfill first) with a GUEST token only: no account,
// no bot session, no paid v2 API calls. Emits the frames /ws/x sends — chatlog {status, messages}, chat
// {messages}, status {status} — plus the uniform conn frame. Sending is not possible without X's paid API, so
// canSend is always false.
//
// WHAT IS AND IS NOT POSSIBLE AS A GUEST (measured; see vendor/xLive.js and render/README.md):
//  * Given a broadcast id, everything works anonymously: state, title, chat history, live chat.
//  * Getting the id from a handle is auth-gated at X's end. The public profile only carries a broadcast link if
//    the streamer tweeted it, so a handle source finds the stream only then; pasting the broadcast link
//    (x.com/i/broadcasts/<id>) always works.
//  * Names: events carry only a user id and the name lookup answers 404 to a guest, so chatters show as
//    "X user <last 5 digits of the id>". A guest-reachable GraphQL UserByRestId answered once in testing and
//    404'd on every retry, so it is deliberately not relied on.
import { broadcastStates, findLive, normalizeHandle } from "./vendor/xLive.js";
import { readLiveChat, toXChatMsg } from "./vendor/xChat.js";
import { setUserAgent } from "./ua.js";

export const platform = "x";

const MAX_BACKLOG = 300;
// x.com's own reconnect delays: a second after a clean end, thirty after an error answer. 503 is X shedding load.
const STREAM_RETRY_MS = 1000;
const STREAM_ERROR_RETRY_MS = 30_000;
// A stream that ends this soon without a single event is not a clean end, whatever its status said.
const STREAM_MIN_HEALTHY_MS = 3000;
const STREAM_FAILS_BEFORE_ERROR = 3;
// How often a live broadcast is asked whether it is still RUNNING.
const LIVE_CHECK_MS = 30_000;
const OFFLINE_POLL_MS = 60_000;
const MAX_RECHECK_MS = 5 * 60_000;
// Events are held this long so the ~100-message backfill arrives as one chatlog rather than 100 frames.
const BATCH_MS = 250;

const READ_ONLY_REASON = "X is read-only (sending needs X's paid API)";
const NAMES_NOTICE = "X does not give guests chatter names; they show as X user + the last digits of their id.";

/** "broadcast:<id>" for a broadcast link/id, otherwise the lowercased X username. */
export function normalizeChannel(input) {
  const raw = String(input || "").trim();
  if (!raw) throw new Error("Enter an X username or a broadcast link.");
  const prefixed = /^broadcast:([A-Za-z0-9]{13,14})$/.exec(raw);
  if (prefixed) return `broadcast:${prefixed[1]}`;
  // Only a URL counts as a broadcast: a bare 13-character word is far likelier a username.
  const fromUrl = /(?:broadcasts?|i\/broadcasts)\/([A-Za-z0-9]{13,14})(?![A-Za-z0-9])/.exec(raw);
  if (fromUrl) return `broadcast:${fromUrl[1]}`;
  const handle = normalizeHandle(raw);
  if (!handle) throw new Error("Enter an X username or a broadcast link (x.com/i/broadcasts/...).");
  return handle.toLowerCase();
}

function sleep(ms, signal) {
  return new Promise((resolve) => {
    if (signal && signal.aborted) return resolve();
    const t = setTimeout(resolve, ms);
    if (signal) {
      signal.addEventListener(
        "abort",
        () => {
          clearTimeout(t);
          resolve();
        },
        { once: true },
      );
    }
  });
}

export function createSource({ channel, emit, ctx }) {
  setUserAgent(ctx.userAgent);
  const norm = normalizeChannel(channel);
  const pinned = norm.startsWith("broadcast:") ? norm.slice("broadcast:".length) : "";
  const handle = pinned ? "" : norm;

  let status = { live: false, broadcastId: null, handle: handle || "", title: "", url: "", state: "", error: null, notice: null };
  let backlog = [];
  let seen = new Set();
  let chat = null; // { broadcastId, cursor, fails, abort, hello }
  let pending = [];
  let pendingTimer = null;
  let pollTimer = null;
  let polling = false;
  let softMisses = 0;
  let resolvedOnce = false;
  let stopped = false;
  let lastConnJson = "";

  const recheckDelay = () => Math.min(OFFLINE_POLL_MS * 2 ** Math.min(softMisses, 3), MAX_RECHECK_MS);

  function connFrame() {
    let state;
    let detail;
    if (!resolvedOnce) {
      state = "connecting";
      detail = "Looking for a live X broadcast...";
    } else if (status.error) {
      state = "error";
      detail = status.error;
    } else if (status.live) {
      state = "open";
      detail = "Live";
    } else {
      state = "offline";
      detail = pinned
        ? status.state && status.state !== "RUNNING"
          ? `This broadcast is not running (${status.state.toLowerCase()}).`
          : "This broadcast is not running."
        : "Not live. X only reveals a broadcast from a handle when the streamer tweets the link: paste the broadcast link to follow it directly.";
    }
    return { type: "conn", state, detail, live: !!status.live, title: status.title || undefined, canSend: false, readOnlyReason: READ_ONLY_REASON };
  }

  function publishConn() {
    const f = connFrame();
    const json = JSON.stringify(f);
    if (json === lastConnJson) return;
    lastConnJson = json;
    emit(f);
  }

  function setStatus(next) {
    const merged = { ...status, ...next };
    const same = Object.keys(merged).every((k) => merged[k] === status[k]);
    status = merged;
    if (!same) emit({ type: "status", status });
    publishConn();
  }

  function chatlogFrame() {
    return { type: "chatlog", status, messages: backlog };
  }

  // ---------- resolution ----------

  /** Which broadcast should this source be reading? A pinned id is the only reliable answer. */
  async function resolve() {
    if (pinned) {
      const states = await broadcastStates([pinned]);
      const s = states[pinned];
      if (s && s.state === "RUNNING") return { live: true, broadcastId: pinned, title: s.title };
      return { live: false, broadcastId: "", state: (s && s.state) || "" };
    }
    // Free when it works, which is only when the streamer tweets the link.
    const found = await findLive(handle);
    return found && found.live ? found : { live: false, broadcastId: "" };
  }

  async function tickResolve() {
    let found;
    try {
      found = await resolve();
    } catch (e) {
      if (stopped) return 0;
      softMisses++;
      resolvedOnce = true;
      setStatus({ live: false, error: (e && e.message) || "lookup failed", notice: null });
      return recheckDelay();
    }
    if (stopped) return 0;
    resolvedOnce = true;

    if (!found.live || !found.broadcastId) {
      // NOT a soft miss: "they are not streaming" is the ordinary answer, and backing off on it would delay
      // noticing a stream that starts. Title and id are cleared so yesterday's doesn't read as running.
      softMisses = 0;
      setStatus({ live: false, broadcastId: null, title: "", state: found.state || "", url: "", error: null, notice: null });
      return OFFLINE_POLL_MS;
    }

    softMisses = 0;
    if (status.broadcastId && status.broadcastId !== found.broadcastId) {
      backlog = [];
      seen.clear();
    }
    chat = { broadcastId: found.broadcastId, cursor: "", fails: 0, abort: new AbortController(), hello: false };
    setStatus({
      live: true,
      broadcastId: found.broadcastId,
      title: found.title || "",
      state: "RUNNING",
      url: `https://x.com/i/broadcasts/${found.broadcastId}`,
      error: null,
      notice: NAMES_NOTICE,
    });
    streamLoop(chat);
    return LIVE_CHECK_MS;
  }

  /** Reading a live broadcast: is it still running? Only an explicit answer ends it. */
  async function tickLive(c) {
    let s;
    try {
      const states = await broadcastStates([c.broadcastId]);
      s = states && states[c.broadcastId];
    } catch (e) {
      ctx.log(`[x:${norm}] state check failed: ${(e && e.message) || e}`);
    }
    if (stopped || chat !== c) return 0;
    if (s && s.state && s.state !== "RUNNING") {
      dropChat();
      setStatus({ live: false, broadcastId: null, title: "", state: s.state, url: "", error: null, notice: null });
      // A handle source goes straight back to resolving, which picks up a follow-on stream.
      return pinned ? OFFLINE_POLL_MS : 0;
    }
    return LIVE_CHECK_MS;
  }

  async function tick() {
    if (stopped || polling) return;
    polling = true;
    let next = OFFLINE_POLL_MS;
    try {
      next = chat ? await tickLive(chat) : await tickResolve();
    } catch (e) {
      // Nothing in either path is meant to throw; if something does, the loop must survive it.
      ctx.log(`[x:${norm}] tick failed: ${(e && e.stack) || e}`);
    } finally {
      polling = false;
      if (!stopped) {
        clearTimeout(pollTimer);
        pollTimer = setTimeout(() => {
          pollTimer = null;
          tick();
        }, next);
      }
    }
  }

  // ---------- the chat stream ----------

  /** Keep X's live-chat stream open while this chat is current, reconnecting the way x.com does. */
  async function streamLoop(c) {
    const signal = c.abort.signal;
    while (chat === c && !signal.aborted) {
      const openedAt = Date.now();
      let events = 0;
      let delay = STREAM_RETRY_MS;
      try {
        await readLiveChat(c.broadcastId, {
          cursor: c.cursor,
          signal,
          onEvent: (ev) => {
            events++;
            onLiveEvent(c, ev);
          },
        });
        if (!events && Date.now() - openedAt < STREAM_MIN_HEALTHY_MS) throw new Error("live-chat ended at once");
        streamRecovered(c);
      } catch (e) {
        if (signal.aborted || chat !== c) break;
        c.fails++;
        const msg = (e && e.message) || String(e);
        delay = e && e.status === 503 ? STREAM_RETRY_MS : Math.min(STREAM_ERROR_RETRY_MS, STREAM_RETRY_MS * 2 ** c.fails);
        ctx.log(`[x:${norm}] live-chat failed (${c.fails}): ${msg}`);
        if (c.fails >= STREAM_FAILS_BEFORE_ERROR) setStatus({ error: `X chat unavailable: ${msg}` });
      }
      if (chat !== c) break;
      await sleep(delay, signal);
    }
  }

  function streamRecovered(c) {
    if (!c.fails) return;
    c.fails = 0;
    if (status.error && status.error.startsWith("X chat unavailable")) setStatus({ error: null });
  }

  function onLiveEvent(c, ev) {
    if (chat !== c || !ev || typeof ev !== "object") return;
    // The cursor is the last `ts` seen, whatever kind of event carried it — that is what x.com reconnects from.
    if (ev.ts && (!c.cursor || String(ev.ts) > String(c.cursor))) c.cursor = String(ev.ts);
    streamRecovered(c);
    if (ev.chatType !== 1 || !ev.message || !ev.userId) return;
    pending.push(ev);
    if (!pendingTimer) {
      pendingTimer = setTimeout(() => {
        pendingTimer = null;
        flush(c);
      }, BATCH_MS);
    }
  }

  function flush(c) {
    const batch = pending;
    pending = [];
    if (!batch.length || chat !== c) return;
    const fresh = [];
    for (const ev of batch) {
      const msg = toXChatMsg(ev);
      if (!msg || seen.has(msg.id)) continue;
      seen.add(msg.id);
      fresh.push(msg);
    }
    fresh.sort((a, b) => a.ts - b.ts);
    backlog = [...backlog, ...fresh].slice(-MAX_BACKLOG);
    if (seen.size > MAX_BACKLOG * 4) seen = new Set(backlog.map((m) => m.id));
    if (!c.hello) {
      // The first batch is the backfill: one chatlog.
      c.hello = true;
      emit(chatlogFrame());
    } else if (fresh.length) {
      emit({ type: "chat", messages: fresh });
    }
  }

  function dropChat() {
    if (chat) chat.abort.abort();
    chat = null;
    pending = [];
    clearTimeout(pendingTimer);
    pendingTimer = null;
  }

  return {
    start() {
      lastConnJson = JSON.stringify(connFrame());
      emit(connFrame());
      tick();
    },
    stop() {
      stopped = true;
      dropChat();
      clearTimeout(pollTimer);
      pollTimer = null;
    },
    snapshot() {
      return [chatlogFrame(), connFrame()];
    },
  };
}
