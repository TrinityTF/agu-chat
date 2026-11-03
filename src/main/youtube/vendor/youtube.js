// Vendored from modtools.agu.lol render/lib/youtube.js (render commit 7704aec). Changes: dropped the
// relay class, the ws client fan-out, youtubei.js (live resolution, game category) and every server-only
// dependency; the browser User-Agent is replaced by the caller's honest product UA (verified: the watch
// page, get_live_chat and updated_metadata all answer it); bootstrapChat() read /live_chat?is_popout=1,
// which answers a non-browser UA with an "outdated browser" stub, so resolution and bootstrap now come
// from ONE watch-page fetch (readWatchPage) that carries the live status, the InnerTube key, the client
// version and the first chat continuation; parseYouTubeSource also accepts a bare word as a handle.
//
// WHY NOT THE OFFICIAL API / WHY INNERTUBE: see render/README.md "YouTube (read-only)". In short, the Data
// API's live chat is quota-metered polling; InnerTube is what youtube.com itself calls, with no quota.
//
// Everything here is pure or takes the fetch inputs explicitly, so the source in ../index.js owns timers
// and lifecycle and this file can be exercised with plain `node`.

// SOCS is YouTube's consent cookie. Without it a channel page can answer with a consent bounce. Standard
// practice (yt-dlp does the same); it is a consent flag, not an identity.
const CONSENT_COOKIE = "SOCS=CAI";

// ---------------------------------------------------------------------------
// Source parsing
// ---------------------------------------------------------------------------

const VIDEO_RE = /^[\w-]{11}$/;
const CHANNEL_RE = /^UC[\w-]{22}$/;
const HANDLE_RE = /^@[\w.-]{1,60}$/;

/**
 * Accepts what someone would actually paste: a handle, a channel URL, a video link, or any of those bare.
 * A VIDEO source pins one broadcast; a channel or handle follows whatever that channel is streaming.
 * Returns {kind: "video"|"channel"|"handle", id} or null.
 */
export function parseYouTubeSource(input) {
  let raw = String(input || "").trim();
  if (!raw) return null;
  // "@LofiGirl/live" is how people paste a handle copied off a /live link.
  if (raw.startsWith("@") && raw.includes("/")) raw = raw.split("/")[0];

  if (CHANNEL_RE.test(raw)) return { kind: "channel", id: raw };
  if (HANDLE_RE.test(raw)) return { kind: "handle", id: raw.toLowerCase() };
  if (VIDEO_RE.test(raw)) return { kind: "video", id: raw };

  let u;
  try {
    u = new URL(/^https?:\/\//i.test(raw) ? raw : `https://${raw}`);
  } catch {
    return null;
  }
  if (!/(^|\.)(youtube\.com|youtu\.be)$/i.test(u.hostname)) {
    // A bare word ("lofigirl") is most likely a handle typed without its @. Only when it is not a URL.
    return /^[\w.-]{1,60}$/.test(raw) && !raw.includes(".") ? { kind: "handle", id: "@" + raw.toLowerCase() } : null;
  }

  if (/youtu\.be$/i.test(u.hostname)) {
    const id = u.pathname.split("/").filter(Boolean)[0] || "";
    return VIDEO_RE.test(id) ? { kind: "video", id } : null;
  }

  const v = u.searchParams.get("v");
  if (v && VIDEO_RE.test(v)) return { kind: "video", id: v };

  const seg = u.pathname.split("/").filter(Boolean);
  if (seg[0] === "channel" && CHANNEL_RE.test(seg[1] || "")) return { kind: "channel", id: seg[1] };
  if ((seg[0] === "live" || seg[0] === "shorts" || seg[0] === "embed") && VIDEO_RE.test(seg[1] || "")) return { kind: "video", id: seg[1] };
  if ((seg[0] || "").startsWith("@") && HANDLE_RE.test(seg[0])) return { kind: "handle", id: seg[0].toLowerCase() };
  // /c/Name and /user/Name are legacy vanity URLs with no stable id in them; handled as handles.
  if ((seg[0] === "c" || seg[0] === "user") && seg[1]) return { kind: "handle", id: "@" + seg[1].toLowerCase() };
  return null;
}

/** The string a normalized source is typed back as: normalizeChannel(sourceString(parse(x))) is stable. */
export function sourceString(src) {
  return src.id;
}

export function youtubeSourceUrl(src) {
  if (src.kind === "video") return `https://www.youtube.com/watch?v=${src.id}`;
  if (src.kind === "channel") return `https://www.youtube.com/channel/${src.id}`;
  return `https://www.youtube.com/${src.id}`;
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async function ytFetch(url, { method = "GET", body, timeoutMs = 15_000, userAgent, signal } = {}) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  // The source's own abort (stop()) must cancel an in-flight request too.
  if (signal) {
    if (signal.aborted) ctl.abort();
    else signal.addEventListener("abort", () => ctl.abort(), { once: true });
  }
  try {
    return await fetch(url, {
      method,
      headers: {
        "User-Agent": userAgent,
        "Accept-Language": "en-US,en",
        Cookie: CONSENT_COOKIE,
        ...(body !== undefined ? { "content-type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal: ctl.signal,
      redirect: "follow",
    });
  } finally {
    clearTimeout(timer);
  }
}

function firstContinuation(list) {
  for (const c of list || []) {
    const v = Object.values(c)[0];
    if (v && v.continuation) return v;
  }
  return null;
}

function textOf(node) {
  if (!node) return "";
  if (node.simpleText) return node.simpleText;
  return ((node.runs || []).map((r) => r.text || "").join("") || "").trim();
}

/**
 * The JSON object assigned right after `marker` in a page, found by brace matching (the page inlines it
 * as a JS assignment, not as a JSON script block). Strings are skipped so braces inside text don't count.
 */
export function jsonAfter(html, marker) {
  const i = html.indexOf(marker);
  if (i < 0) return null;
  const start = html.indexOf("{", i + marker.length);
  if (start < 0) return null;
  let depth = 0;
  let inStr = false;
  let esc = false;
  for (let k = start; k < html.length; k++) {
    const c = html[k];
    if (inStr) {
      if (esc) esc = false;
      else if (c === "\\") esc = true;
      else if (c === '"') inStr = false;
      continue;
    }
    if (c === '"') inStr = true;
    else if (c === "{") depth++;
    else if (c === "}") {
      depth--;
      if (depth === 0) {
        try {
          return JSON.parse(html.slice(start, k + 1));
        } catch {
          return null;
        }
      }
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Resolution + bootstrap, from one watch page
// ---------------------------------------------------------------------------

/**
 * Which video is this source live on, and everything needed to read its chat.
 *
 * /@handle/live and /channel/<id>/live answer with the watch page of the current live video when there is
 * one, and with the channel's browse page when there is not. So the page's own data decides: a player
 * response whose videoDetails.isLive is true is a live stream, anything else is "not live". Presence of a
 * video id alone proves nothing (a channel's /live can show its last, ended stream).
 *
 * Returns {exists, blocked, live, videoId, title, author, channelId, key, clientVersion, continuation,
 * notice, error}. `notice` is YouTube's own words when chat is unavailable ("Chat is disabled for this
 * live stream."), surfaced verbatim.
 */
export async function readWatchPage(src, opts) {
  const url = src.kind === "video" ? youtubeSourceUrl(src) : `${youtubeSourceUrl(src)}/live`;
  const r = await ytFetch(url, opts);
  if (r.status === 404) return { exists: false, blocked: false, live: false, videoId: null, title: "", author: "", channelId: "" };
  if (!r.ok) throw new Error(`YouTube answered ${r.status}`);
  // A consent wall or bot check lands off youtube.com; say so rather than reading it as "offline".
  const host = (() => {
    try {
      return new URL(r.url).hostname;
    } catch {
      return "";
    }
  })();
  if (!/(^|\.)youtube\.com$/i.test(host)) {
    return { exists: true, blocked: true, live: false, videoId: null, title: "", author: "", channelId: "" };
  }
  const html = await r.text();

  const key = (html.match(/"INNERTUBE_API_KEY":"([^"]+)"/) || [])[1] || "";
  const clientVersion = (html.match(/"INNERTUBE_CLIENT_VERSION":"([^"]+)"/) || [])[1] || "";
  const player = jsonAfter(html, "var ytInitialPlayerResponse = ") || jsonAfter(html, 'window["ytInitialPlayerResponse"] = ');
  const data = jsonAfter(html, "var ytInitialData = ") || jsonAfter(html, 'window["ytInitialData"] = ');
  if (!key || !data) return { exists: true, blocked: true, live: false, videoId: null, title: "", author: "", channelId: "", error: "YouTube returned a page this client does not recognise" };

  const vd = player && player.videoDetails;
  const channelMeta = data.metadata && data.metadata.channelMetadataRenderer;
  if (!vd || !vd.videoId) {
    // A browse page: a channel that is not streaming.
    return {
      exists: true,
      blocked: false,
      live: false,
      videoId: null,
      title: "",
      author: (channelMeta && channelMeta.title) || "",
      channelId: (channelMeta && channelMeta.externalId) || "",
      key,
      clientVersion,
    };
  }

  const base = {
    exists: true,
    blocked: false,
    videoId: vd.videoId,
    title: vd.title || "",
    author: vd.author || "",
    channelId: vd.channelId || "",
    key,
    clientVersion,
  };
  const micro = player.microformat && player.microformat.playerMicroformatRenderer;
  const live = vd.isLive === true || !!(micro && micro.liveBroadcastDetails && micro.liveBroadcastDetails.isLiveNow);
  if (!live) return { ...base, live: false };

  const bar = data.contents && data.contents.twoColumnWatchNextResults && data.contents.twoColumnWatchNextResults.conversationBar;
  const lc = bar && bar.liveChatRenderer;
  if (!lc || lc.isReplay) {
    // YouTube says this in plain English; passing it through beats a blank column with no explanation.
    const msg = bar && bar.conversationBarRenderer && bar.conversationBarRenderer.availabilityMessage && bar.conversationBarRenderer.availabilityMessage.messageRenderer;
    return { ...base, live: true, continuation: "", notice: (msg && textOf(msg.text)) || "YouTube didn't return a chat for this stream" };
  }
  const cont = firstContinuation(lc.continuations);
  if (!cont) return { ...base, live: true, continuation: "", notice: "YouTube didn't return a chat for this stream" };
  return { ...base, live: true, continuation: cont.continuation, timeoutMs: cont.timeoutMs || 0, notice: null };
}

/**
 * The stream's title and live viewer count. updated_metadata is what youtube.com's own watch page calls
 * to refresh those; ~7 KB. Called far less often than YouTube suggests (its 5s is a ticking counter).
 */
export async function fetchMetadata({ key, clientVersion, videoId }, opts) {
  const r = await ytFetch(`https://www.youtube.com/youtubei/v1/updated_metadata?key=${key}&prettyPrint=false`, {
    ...opts,
    method: "POST",
    body: { context: { client: { clientName: "WEB", clientVersion } }, videoId },
  });
  if (!r.ok) throw new Error(`updated_metadata returned ${r.status}`);
  const j = await r.json();

  const out = { title: "", viewers: null, live: null };
  for (const a of j.actions || []) {
    const vc = a.updateViewershipAction && a.updateViewershipAction.viewCount && a.updateViewershipAction.viewCount.videoViewCountRenderer;
    if (vc) {
      // "10,107 watching now" — separators are locale-shaped, so strip everything that isn't a digit.
      const digits = textOf(vc.viewCount).replace(/[^\d]/g, "");
      out.viewers = digits ? Number(digits) : null;
      if (typeof vc.isLive === "boolean") out.live = vc.isLive;
    }
    if (a.updateTitleAction) out.title = textOf(a.updateTitleAction.title);
  }
  return out;
}

/** One poll. Returns the next continuation, or null when the chat has ended. */
export async function pollChat({ key, clientVersion, continuation }, opts) {
  const r = await ytFetch(`https://www.youtube.com/youtubei/v1/live_chat/get_live_chat?key=${key}&prettyPrint=false`, {
    ...opts,
    method: "POST",
    body: { context: { client: { clientName: "WEB", clientVersion } }, continuation },
  });
  if (!r.ok) throw new Error(`live chat poll returned ${r.status}`);
  const j = await r.json();
  const cc = j && j.continuationContents && j.continuationContents.liveChatContinuation;
  // No continuationContents means the chat is over (or the token expired); the caller must re-resolve.
  if (!cc) return null;
  const next = firstContinuation(cc.continuations);
  return { actions: cc.actions || [], continuation: next ? next.continuation : "", timeoutMs: next ? next.timeoutMs || 0 : 0 };
}

// ---------------------------------------------------------------------------
// Message normalization (unchanged from the site)
// ---------------------------------------------------------------------------

// ARGB integers, which is what YouTube ships for Super Chat tiers.
function argbToCss(n) {
  if (!Number.isFinite(n)) return "";
  const a = ((n >>> 24) & 255) / 255;
  return `rgba(${(n >>> 16) & 255}, ${(n >>> 8) & 255}, ${n & 255}, ${a.toFixed(3)})`;
}

/**
 * Message text arrives as "runs" — alternating text and emoji objects, where each emoji carries its own
 * image URL, so the parts are shipped ready-rendered. `text` is the plain-text form.
 */
function parseRuns(message) {
  const runs = (message && message.runs) || [];
  const parts = [];
  let text = "";
  for (const run of runs) {
    if (run.text) {
      parts.push({ t: "text", v: run.text });
      text += run.text;
      continue;
    }
    const e = run.emoji;
    if (!e) continue;
    const thumbs = (e.image && e.image.thumbnails) || [];
    const url = thumbs.length ? thumbs[thumbs.length - 1].url : "";
    const label = (e.shortcuts && e.shortcuts[0]) || e.emojiId || "";
    if (url) parts.push({ t: "emoji", u: url, a: label, custom: !!e.isCustomEmoji });
    text += label;
  }
  return { parts, text };
}

function parseBadges(list) {
  const out = [];
  for (const b of list || []) {
    const r = b && b.liveChatAuthorBadgeRenderer;
    if (!r) continue;
    const thumbs = (r.customThumbnail && r.customThumbnail.thumbnails) || [];
    out.push({
      label: r.tooltip || "",
      // Members get real artwork; moderator/owner/verified come as icon types we style ourselves.
      icon: (r.icon && r.icon.iconType) || "",
      url: thumbs.length ? thumbs[thumbs.length - 1].url : "",
    });
  }
  return out.slice(0, 6);
}

/** "@someone" -> "someone". A name that is only "@" is left alone. */
function stripHandle(name) {
  const s = String(name || "");
  return s.length > 1 && s[0] === "@" ? s.slice(1) : s;
}

/** One InnerTube action -> a chat row, or null for the many action types that are not messages. */
export function toYouTubeChatMsg(action) {
  const item = action && action.addChatItemAction && action.addChatItemAction.item;
  if (!item) return null;

  const text = item.liveChatTextMessageRenderer;
  const paid = item.liveChatPaidMessageRenderer;
  const sticker = item.liveChatPaidStickerRenderer;
  const member = item.liveChatMembershipItemRenderer;
  const r = text || paid || sticker || member;
  if (!r) return null;

  const ts = Number(r.timestampUsec);
  const authorThumbs = (r.authorPhoto && r.authorPhoto.thumbnails) || [];
  const { parts, text: plain } = parseRuns(r.message || r.headerSubtext);

  return {
    platform: "youtube",
    id: String(r.id || ""),
    ts: Number.isFinite(ts) ? Math.round(ts / 1000) : Date.now(),
    user: {
      // The channel id is the only identifier YouTube moderation can act on.
      id: String(r.authorExternalChannelId || ""),
      // YouTube gives the author as a HANDLE ("@someone"); the leading @ is stripped so a merged timeline
      // reads the same across platforms.
      name: stripHandle((r.authorName && r.authorName.simpleText) || ""),
      avatar: authorThumbs.length ? authorThumbs[authorThumbs.length - 1].url : "",
    },
    badges: parseBadges(r.authorBadges),
    parts,
    text: plain,
    // Super Chats and Super Stickers carry their own tier colour, passed through rather than re-derived.
    paid:
      paid || sticker
        ? {
            amount: ((paid || sticker).purchaseAmountText || {}).simpleText || "",
            bg: argbToCss((paid || sticker).bodyBackgroundColor),
            headerBg: argbToCss((paid || sticker).headerBackgroundColor),
            sticker: !!sticker,
          }
        : null,
    // New members and milestones render as notices rather than chat lines.
    notice: member ? "membership" : null,
  };
}
