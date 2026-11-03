// YouTube platform module (read-only). See docs/ARCHITECTURE.md ("Platform module").
//
// Emits the frames the agu.lol /ws/youtube relay sends, so YouTubeChat.tsx row code works unchanged:
//   {type:"chatlog", source, status, messages}, {type:"chat", messages:[...]}, {type:"status", status}
// plus the uniform {type:"conn"} frame.
//
// One poll loop per source, paced by YouTube's own timeoutMs; repeated failure backs off exponentially so a
// deleted channel or a YouTube outage cannot become a fetch loop. Sending is not supported: see
// render/README.md "Sending — not supported" (the official API's quota and sensitive-scope verification
// make it unviable, and the alternatives need the user's Google session cookie).
import { parseYouTubeSource, sourceString, youtubeSourceUrl, readWatchPage, pollChat, fetchMetadata, toYouTubeChatMsg } from "./vendor/youtube.js";

export const platform = "youtube";

const MAX_BACKLOG = 300;
// How often to re-check an offline channel for a new broadcast: a ~1 MB page fetch, so deliberately slow.
const OFFLINE_POLL_MS = 60_000;
// Only used when YouTube declines to tell us; its own timeoutMs is always preferred.
const FALLBACK_POLL_MS = 10_000;
const MAX_BACKOFF_MS = 5 * 60_000;
// The title/viewer count is decoration; a minute is roughly right for a viewer counter.
const METADATA_TTL_MS = 60_000;
// How soon to look again after one unconvincing answer. Only the FIRST retry is quick: if YouTube keeps
// giving unconvincing answers it is likely unhappy with us, and rechecking hard is the wrong response.
const RECHECK_MS = 12_000;
const MAX_RECHECK_MS = 2 * 60_000;

const READ_ONLY_REASON = "YouTube is read-only in this beta";

export function normalizeChannel(input) {
  const src = parseYouTubeSource(input);
  if (!src) throw new Error("Enter a YouTube @handle, channel id, or a channel / stream link.");
  return sourceString(src);
}

export function createSource({ channel, emit, ctx }) {
  const src = parseYouTubeSource(channel);
  if (!src) throw new Error("Not a YouTube source: " + channel);
  const ac = new AbortController();

  let status = {
    live: false,
    videoId: null,
    category: "",
    channelId: "",
    author: "",
    title: "",
    viewers: null,
    url: youtubeSourceUrl(src),
    notice: null,
    error: null,
  };
  // connecting | open | offline | unavailable | error
  let phase = "connecting";
  let backlog = [];
  let seen = new Set();
  let chat = null; // { key, clientVersion, continuation, videoId }
  let metaAt = 0;
  let offlineStreak = 0;
  let softMisses = 0;
  let failures = 0;
  let timer = null;
  let polling = false;
  let stopped = false;
  let lastConnJson = "";

  const opts = () => ({ userAgent: ctx.userAgent, signal: ac.signal });
  const recheckDelay = () => Math.min(RECHECK_MS * 2 ** Math.min(softMisses, 3), MAX_RECHECK_MS);

  function connFrame() {
    const s = status;
    let detail;
    if (phase === "open") detail = "Live";
    else if (phase === "offline") detail = s.notice || "Not live";
    else if (phase === "unavailable") detail = s.notice || "Chat is unavailable for this stream.";
    else if (phase === "error") detail = s.error || "YouTube unreachable";
    else detail = "Connecting to YouTube...";
    return { type: "conn", state: phase, detail, live: s.live, title: s.title || undefined, canSend: false, readOnlyReason: READ_ONLY_REASON };
  }

  // Emit the conn frame only when it changed, so a quiet poll loop does not spam the renderer.
  function publishConn() {
    const f = connFrame();
    const json = JSON.stringify(f);
    if (json === lastConnJson) return;
    lastConnJson = json;
    emit(f);
  }

  function setStatus(next, nextPhase) {
    const merged = { ...status, ...next };
    const same = Object.keys(merged).every((k) => merged[k] === status[k]);
    status = merged;
    if (nextPhase) phase = nextPhase;
    if (!same) emit({ type: "status", status });
    publishConn();
  }

  function chatlogFrame() {
    return { type: "chatlog", source: { kind: src.kind, id: src.id }, status, messages: backlog };
  }

  /** Normalize actions into rows, de-duplicated (bootstrap backlog and first poll overlap). */
  function ingest(actions) {
    const fresh = [];
    for (const a of actions) {
      const msg = toYouTubeChatMsg(a);
      if (!msg || !msg.id || seen.has(msg.id)) continue;
      seen.add(msg.id);
      fresh.push(msg);
    }
    if (!fresh.length) return fresh;
    backlog = [...backlog, ...fresh].slice(-MAX_BACKLOG);
    // Bounded alongside the backlog: without this the id set grows for as long as the source lives.
    if (seen.size > MAX_BACKLOG * 4) seen = new Set(backlog.map((m) => m.id));
    return fresh;
  }

  async function refreshMetadata() {
    if (!chat || Date.now() - metaAt < METADATA_TTL_MS) return;
    metaAt = Date.now();
    try {
      const meta = await fetchMetadata(chat, opts());
      if (stopped) return;
      setStatus({ title: meta.title || status.title, viewers: meta.viewers != null ? meta.viewers : status.viewers });
    } catch {
      /* decoration on a working chat; a metadata hiccup must not back off the poll */
    }
  }

  async function pollStep(first) {
    const res = await pollChat(chat, opts());
    if (stopped) return 0;
    if (!res || !res.continuation) {
      // Chat ended or the token expired. Re-resolve promptly rather than declaring the channel offline here:
      // an expired continuation on a still-running stream is common, and the resolve decides.
      chat = null;
      seen.clear();
      softMisses++;
      return recheckDelay();
    }
    chat.continuation = res.continuation;
    const fresh = ingest(res.actions);
    // The very first poll is the history: one chatlog instead of ~75 chat frames.
    if (first) emit(chatlogFrame());
    else if (fresh.length) emit({ type: "chat", messages: fresh });
    setStatus({ error: null, notice: null }, "open");
    await refreshMetadata();
    failures = 0;
    softMisses = 0;
    return res.timeoutMs || FALLBACK_POLL_MS;
  }

  async function resolveStep() {
    const res = await readWatchPage(src, opts());
    if (stopped) return 0;

    if (!res.exists) {
      setStatus({ live: false, videoId: null, error: src.kind === "video" ? "No such YouTube video." : "No such YouTube channel.", notice: null }, "error");
      return MAX_BACKOFF_MS;
    }
    if (res.blocked) {
      softMisses++;
      setStatus({ live: false, error: res.error || "YouTube served a consent or bot-check page instead of the channel.", notice: null }, "error");
      return recheckDelay();
    }
    if (!res.live) {
      offlineStreak++;
      softMisses++;
      // One negative answer is not proof of going offline (the served page shape can vary between requests).
      if (status.live && offlineStreak < 2) return recheckDelay();
      setStatus(
        {
          live: false,
          videoId: res.videoId || null,
          channelId: res.channelId || status.channelId,
          author: res.author || status.author,
          // Cleared on purpose: a stale title and viewer count would read as though that broadcast were running.
          title: "",
          viewers: null,
          notice: src.kind === "video" ? "This video is not a live stream right now." : null,
          error: null,
        },
        "offline",
      );
      return OFFLINE_POLL_MS;
    }

    offlineStreak = 0;
    const base = {
      live: true,
      videoId: res.videoId,
      channelId: res.channelId || status.channelId,
      author: res.author || status.author,
      url: `https://www.youtube.com/watch?v=${res.videoId}`,
    };
    if (status.videoId && status.videoId !== res.videoId) {
      // A different broadcast: the old chat's rows belong to another video.
      backlog = [];
      seen.clear();
    }
    if (!res.continuation) {
      // Live, but no chat. YouTube's own words are shown, and NOT latched for a full offline poll: the same
      // stream can bootstrap fine seconds later. A genuinely members-only/disabled chat answers the same way.
      softMisses++;
      setStatus({ ...base, title: res.title || status.title, notice: res.notice, error: null }, "unavailable");
      return recheckDelay();
    }

    chat = { key: res.key, clientVersion: res.clientVersion, continuation: res.continuation, videoId: res.videoId };
    metaAt = 0;
    setStatus({ ...base, title: res.title || status.title, notice: null, error: null });
    return await pollStep(true);
  }

  async function tick() {
    if (stopped || polling) return;
    polling = true;
    let next = OFFLINE_POLL_MS;
    try {
      next = chat ? await pollStep(false) : await resolveStep();
    } catch (e) {
      if (stopped) return;
      failures++;
      chat = null;
      const msg = (e && e.message) || "YouTube unreachable";
      setStatus({ error: msg }, "error");
      ctx.log(`[youtube] ${channel} poll failed (${failures}): ${msg}`);
      // Exponential backoff so a deleted channel, or a YouTube outage, cannot turn into a fetch loop.
      next = Math.min(OFFLINE_POLL_MS * 2 ** Math.min(failures, 4), MAX_BACKOFF_MS);
    } finally {
      polling = false;
      if (!stopped) schedule(next);
    }
  }

  function schedule(ms) {
    clearTimeout(timer);
    timer = setTimeout(() => {
      timer = null;
      tick();
    }, Math.max(1000, ms));
  }

  return {
    start() {
      lastConnJson = JSON.stringify(connFrame());
      emit(connFrame());
      tick();
    },
    stop() {
      stopped = true;
      clearTimeout(timer);
      timer = null;
      ac.abort();
    },
    snapshot() {
      return [chatlogFrame(), connFrame()];
    },
  };
}
