// Vendored from modtools.agu.lol render/lib/rumble.js (render commit 7704aec). Changes: dropped the RumbleRelay class (ws fan-out, linger, per-viewer hello) and process-wide stats, because ../index.js reimplements the lifecycle for a single in-process subscriber; dropped the ws import and the source registry; the browser User-Agent is replaced by the app's honest UA (../ua.js; verified wn0 answers it); rumbleFetch (now exported, for the SSE reader), resolveRumbleVideo/Channel, fetchRumbleEmotes, toRumbleChatMsg, fetchCanModerate and the parsers are otherwise unchanged.
// Rumble co-chat — the second chat feed on the /chat page.
//
// WHY SSE, AND WHY THIS LIVES ON RENDER
// -------------------------------------
// Rumble has no chat WebSocket and no public chat API. Its own web client
// reads Server-Sent Events from
//   GET https://web<N>.rumble.com/chat/api/chat/<chatId>/stream
// which is unauthenticated and CORS-open (`Access-Control-Allow-Origin: *`).
// That is a long-lived outbound connection per watched stream, which is
// exactly the shape the Cloudflare free tier can't hold — so, like the Twitch
// IRC relay next door, it lives here and fans out to browsers over a
// WebSocket. See MIGRATION.md's frozen-fallback policy: co-chat providers are
// Render-only, and a client on the DO fallback simply sees "unavailable".
//
// The structure deliberately mirrors lib/ircRelay.js's ChannelRelay — one
// upstream connection per source shared by every viewer, linger before
// teardown, exponential reconnect — so the two feeds behave the same way
// under flaps and the /ws/rumble handler in server.js reads like /ws/chat's.
//
// WHAT A "SOURCE" IS
// ------------------
// Rumble chat is per-VIDEO, not per-channel, so a stream's chat id changes
// every broadcast. A relay is therefore keyed by the *configured source* (a
// channel, or one pinned video) rather than by chat id, and owns the job of
// re-resolving which video is live right now and swapping the SSE connection
// underneath its viewers. A channel source that goes offline keeps polling
// and reattaches by itself when the next stream starts; nobody has to touch
// the settings between streams.
//
// EMOTES
// ------
// Rumble emotes are `:shortcode:` tokens (":r+cooldoge:") resolved against a
// per-chat emote list. There is no emote route on the chat API — /chat/<id>/
// emotes 404s — and the list is inlined into page HTML rather than served as
// data. The video page is unusable for that (it redirects in a loop off the
// apex, and the apex is challenged), but the CHAT POPUP page carries the same
// list and web<N> serves it happily: see fetchRumbleEmotes below.
import { fetchFromRumble } from "./rumbleHosts.js";
import { getUserAgent } from "../ua.js";

// EVERY request in this file goes to one of Rumble's front-end hosts (which
// ones, in what order, and the fallback when one closes: lib/rumbleHosts.js),
// never rumble.com.
//
// This is load-bearing, not a preference. Cloudflare fronts the apex with a
// bot challenge that fingerprints the TLS client, and Node is on the wrong
// side of it: from undici (and from node:https — same TLS stack, same
// verdict) `rumble.com/c/<channel>`, `/embedJS/...` and `/chat/command` all
// return a 403 "Just a moment..." interactive challenge, no matter what
// headers are sent. Adding browser headers does not help; the fingerprint is
// what's being judged.
//
// Rumble's own front-ends serve the identical site and are NOT behind that
// challenge. So the origin swap is the whole reason this works from a server
// at all, and moving any request back to the apex will break it in production
// while continuing to work from a developer's curl.

const MAX_BACKLOG = 200;
const LINGER_MS = 120_000;
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 60_000;
// How often an idle (offline) source re-checks whether its channel has gone
// live. Rumble's channel page is a ~500KB HTML fetch, so this is deliberately
// slow — a viewer waiting on a stream that hasn't started can wait a minute.
const OFFLINE_POLL_MS = 60_000;
// A live source re-checks its own liveness far less often; the SSE stream
// ending is the primary "it's over" signal and this is just the backstop for
// a broadcast that stops without the stream closing.
const LIVE_RECHECK_MS = 5 * 60_000;
// Rumble sends a `: ` heartbeat comment on the stream even when chat is
// quiet, so a gap this long means the connection is dead regardless of how
// slow the chat happens to be.
const IDLE_TIMEOUT_MS = 5 * 60_000;
const FETCH_TIMEOUT_MS = 20_000;


// ---------------------------------------------------------------------------
// Source parsing + resolution
// ---------------------------------------------------------------------------

// Turns whatever a user pasted into a normalized source descriptor. Accepts a
// channel ("rumble.com/c/Foo", "rumble.com/user/Foo", "c/Foo", "Foo"), a video
// page URL ("rumble.com/v7eh2s2-some-title.html"), or a bare embed id
// ("v7capuk"). Returns null when nothing recognizable is in there.
//
// The two kinds behave differently downstream: a `channel` source follows
// whatever that channel is streaming right now, forever; a `video` source is
// pinned to one broadcast and goes permanently idle when it ends.
export function parseRumbleSource(input) {
  const raw = String(input || "").trim();
  if (!raw) return null;

  // Strip a scheme/host if present so the rest can treat this as a path.
  let path = raw.replace(/^https?:\/\//i, "").replace(/^(?:www\.)?rumble\.com/i, "");
  path = path.split("?")[0].split("#")[0].replace(/^\/+/, "").replace(/\/+$/, "");
  if (!path) return null;

  // A video page: "<slug>-<title-words>.html", where the slug is the embed id.
  const page = /^(v[0-9a-z]+)(?:-[^/]*)?\.html$/i.exec(path);
  if (page) return { kind: "video", id: page[1].toLowerCase() };

  // "embed/v7capuk" or a bare embed id.
  const embed = /^(?:embed\/)?(v[0-9a-z]{4,})$/i.exec(path);
  if (embed) return { kind: "video", id: embed[1].toLowerCase() };

  // A channel: "c/Name" or "user/Name". Rumble treats the two as separate
  // namespaces, so the prefix is part of the identity and is preserved.
  const chan = /^(c|user)\/([A-Za-z0-9_.-]+)/.exec(path);
  if (chan) return { kind: "channel", space: chan[1].toLowerCase(), id: chan[2] };

  // A bare name with no prefix — assume a channel, which is the common case.
  if (/^[A-Za-z0-9_.-]+$/.test(path)) return { kind: "channel", space: "c", id: path };

  return null;
}

// Stable map key for a parsed source, so two viewers who typed the same
// channel a different way share one upstream connection.
export function rumbleSourceKey(src) {
  return src.kind === "video" ? `v:${src.id}` : `${src.space}:${src.id.toLowerCase()}`;
}

export function rumbleSourceUrl(src) {
  return src.kind === "video" ? `https://rumble.com/${src.id}` : `https://rumble.com/${src.space}/${src.id}`;
}

// One-shot JSON/HTML fetches get a deadline; the SSE stream passes
// `timeoutMs: 0` because the whole point there is a connection that stays
// open, and it brings its own AbortController for teardown instead. Note the
// explicit `=== undefined` check — `init.timeoutMs || DEFAULT` would quietly
// turn that intentional 0 back into a 20-second cap that kills every stream.
export async function rumbleFetch(url, init = {}) {
  const timeoutMs = init.timeoutMs === undefined ? FETCH_TIMEOUT_MS : init.timeoutMs;
  const ctl = new AbortController();
  const timer = timeoutMs > 0 ? setTimeout(() => ctl.abort(), timeoutMs) : null;
  // A caller-supplied signal (the relay's) must still abort the request, so
  // chain it onto the one actually handed to fetch.
  if (init.signal) {
    if (init.signal.aborted) ctl.abort();
    else init.signal.addEventListener("abort", () => ctl.abort(), { once: true });
  }
  try {
    return await fetch(url, {
      ...init,
      signal: ctl.signal,
      headers: { "User-Agent": getUserAgent(), "Accept-Language": "en-US,en;q=0.9", ...(init.headers || {}) },
    });
  } finally {
    if (timer) clearTimeout(timer);
  }
}

// Resolves a pinned video to its chat id via Rumble's embed metadata route.
// This hands out the numeric chat id directly (`vid`) as plain JSON, which is
// why the resolver goes through embeds rather than scraping /v...html — video
// pages redirect in a loop off the apex and are challenged on it.
//
// (The chat id is also just base36("v7capuk" minus the "v") — verified — but
// embedJS is used anyway because it returns live status and the title in the
// same trip, and the slug in a video page URL is a DIFFERENT id from the
// embed id, so decoding the wrong one silently yields a valid-looking number
// for someone else's video.)
//
// `live` is a small enum on Rumble's side (0 offline, 2 live); anything else
// is treated as not-live rather than guessed at.
// What Rumble files a video under, for the Chat widget's auto-theme.
//
// embedJS has no category field of its own. The only place Rumble states one
// is the ad-targeting block it ships with every video — `pause_ads_params.
// categories`, a primary category plus a secondary slug, e.g. ["Gaming",
// "world-of-warcraft"]. It is undocumented and may move, so this is strictly
// best-effort: anything missing reads as "no category", never as an error.
// Slugs become words so the theme words ("World of Warcraft") can match them,
// and both entries are kept, joined, because either can be the useful one.
export function rumbleCategoryOf(body) {
  const cats = body && body.a && body.a.pause_ads_params && body.a.pause_ads_params.categories;
  if (!Array.isArray(cats)) return "";
  return cats
    .filter((c) => typeof c === "string" && c.trim())
    .slice(0, 3)
    .map((c) => c.trim().replace(/[-_]+/g, " "))
    .join(" · ");
}

export async function resolveRumbleVideo(embedId) {
  const path = `/embedJS/u3/?request=video&ver=2&v=${encodeURIComponent(embedId)}`;
  const r = await fetchFromRumble(path, { headers: { Accept: "application/json" } }, rumbleFetch);
  if (!r.ok) throw new Error(`rumble embedJS ${r.status}`);
  const body = await r.json();
  const chatId = body && body.vid;
  if (!chatId) throw new Error("rumble embedJS: no video id");
  return {
    chatId: String(chatId),
    live: body.live === 2,
    title: body.title || "",
    category: rumbleCategoryOf(body),
    author: (body.author && body.author.name) || "",
    authorUrl: (body.author && body.author.url) || "",
    // `l` is the video's path on rumble.com — the link a viewer follows to
    // see the stream itself.
    url: body.l ? `https://rumble.com${body.l}` : rumbleSourceUrl({ kind: "video", id: embedId }),
    watching: null,
  };
}

// Resolves a channel to whatever it is streaming right now.
//
// Rumble renders channel pages server-side and embeds the full listing as a
// single <script type="application/json">{"items":[...]}</script> block, each
// item carrying `id` (the chat id), `live`, `livestream_status` and
// `permalink_id`. That is a far more stable contract than the surrounding
// markup, which is htmx-driven and changes shape. Returns `{live:false}`
// rather than throwing when the channel simply isn't streaming — that is a
// normal state a viewer sits in between broadcasts, not an error.
export async function resolveRumbleChannel(space, name) {
  // Two URLs, deliberately: the one we FETCH must be the unchallenged web
  // front-end, while the one handed to the browser must be the apex, since
  // that is the link a human clicks and shares.
  const fetchPath = `/${space}/${encodeURIComponent(name)}`;
  const url = `https://rumble.com/${space}/${encodeURIComponent(name)}`;
  const r = await fetchFromRumble(fetchPath, { headers: { Accept: "text/html" } }, rumbleFetch);
  if (!r.ok) throw new Error(`rumble channel ${r.status}`);
  const html = await r.text();

  const offline = { live: false, chatId: null, title: "", author: name, authorUrl: url, url, watching: null };

  let items = null;
  for (const m of html.matchAll(/<script type="application\/json">([\s\S]*?)<\/script>/g)) {
    let parsed;
    try {
      parsed = JSON.parse(m[1]);
    } catch {
      continue; // some blocks are unrelated config, not the listing
    }
    if (parsed && Array.isArray(parsed.items)) {
      items = parsed.items;
      break;
    }
  }

  if (items) {
    // `live: true` and `livestream_status: 2` agree in practice; accepting
    // either means a change to one of them doesn't silently blind the
    // resolver. A channel can have more than one live item (a re-stream); the
    // most-watched one is the one a viewer means.
    const live = items
      .filter((it) => it && (it.live === true || it.livestream_status === 2) && it.id)
      .sort((a, b) => (b.watching_now || 0) - (a.watching_now || 0))[0];
    if (!live) return offline;
    return {
      live: true,
      chatId: String(live.id),
      title: live.title || "",
      author: name,
      authorUrl: url,
      url: live.url || (live.permalink_id ? `https://rumble.com/${live.permalink_id}` : url),
      watching: typeof live.watching_now === "number" ? live.watching_now : null,
    };
  }

  // No JSON payload. That is NOT an error and NOT an empty channel: /user/
  // pages never carry one (only /c/ pages do), so a perfectly valid personal
  // page lands here every time. Fall back to reading the rendered listing.
  //
  // The page must still look like a channel/user page — if the listing
  // container is missing too, we were handed something else entirely (a 404
  // body, a redirect page, a Rumble redesign) and that IS worth reporting.
  const listing = extractListingHtml(html);
  if (listing === null) throw new Error("Rumble returned a page this doesn't recognise");

  const permalinks = listingPermalinks(listing);
  if (!permalinks.length) return offline;

  // embedJS is authoritative about live status, so ask it rather than trying
  // to read a "LIVE" badge out of markup that changes. Capped and sequential:
  // this only runs on the poll for an offline source, and hammering Rumble
  // with parallel lookups on every tick is exactly what the cap avoids.
  for (const permalink of permalinks) {
    try {
      const info = await resolveRumbleVideo(permalink);
      if (info.live && info.chatId) return { ...info, author: info.author || name, authorUrl: url };
    } catch {
      // A permalink that won't resolve is not fatal — try the next.
    }
  }
  return offline;
}

// The channel/user listing section, or null when the page has no such section
// at all. Exported for testing because everything downstream depends on this
// slice being the CHANNEL'S OWN content: Rumble pages carry a sidebar rail
// advertising unrelated live channels, and scraping the whole document would
// happily attach someone's chat to a stranger's stream.
export function extractListingHtml(html) {
  const start = String(html || "").indexOf("channel-listing__container");
  if (start < 0) return null;
  const end = html.indexOf("</section>", start);
  return end < 0 ? html.slice(start) : html.slice(start, end);
}

// Distinct video permalinks (the "v7eh2s2" slug) from a listing slice, capped.
// The cap matters: this runs on every poll for an offline source, and each
// permalink costs an embedJS lookup — an unbounded list would turn one channel
// check into a burst of requests at Rumble.
export function listingPermalinks(listingHtml, limit = 6) {
  if (!listingHtml) return [];
  const found = [...listingHtml.matchAll(/href="\/(v[0-9a-z]+)[^"]*\.html/g)].map((m) => m[1]);
  return [...new Set(found)].slice(0, limit);
}

// The chat's emote list: {name: {file, subsOnly}}.
//
// Scraped from the chat popup page, which embeds it as an argument to the
// page's own RumbleChat(...) call. That is a JS object literal with unquoted
// keys, not JSON, so rather than trying to make it parseable this pulls the
// emote entries directly — they have a tight, predictable shape, and a change
// to the surrounding structure can then only cost us emotes rather than
// throwing.
//
// Cached hard: the page is ~500KB and an emote set changes about never, so
// this must not run per message or per reconnect.
const emoteCache = new Map(); // chatId -> { emotes, at }
const EMOTE_TTL_MS = 30 * 60_000;

export async function fetchRumbleEmotes(chatId, { fetchImpl = rumbleFetch } = {}) {
  if (!chatId) return {};
  const hit = emoteCache.get(String(chatId));
  if (hit && Date.now() - hit.at < EMOTE_TTL_MS) return hit.emotes;

  try {
    const r = await fetchFromRumble(`/chat/popup/${encodeURIComponent(chatId)}`, { headers: { Accept: "text/html" } }, fetchImpl);
    if (!r.ok) throw new Error(`popup ${r.status}`);
    const html = await r.text();
    const emotes = {};
    const re = /\{name:"([^"]+)",is_subs_only:(true|false),position:(\d+),file:"([^"]+)"\}/g;
    let m;
    while ((m = re.exec(html))) emotes[m[1]] = { file: m[4], subsOnly: m[2] === "true" };
    emoteCache.set(String(chatId), { emotes, at: Date.now() });
    return emotes;
  } catch {
    // No emotes is a degradation, not a failure — shortcodes stay as text.
    // Cache the empty result briefly so a broken page doesn't get refetched
    // on every reconnect.
    emoteCache.set(String(chatId), { emotes: {}, at: Date.now() });
    return {};
  }
}

export async function resolveRumbleSource(src) {
  if (src.kind === "video") return await resolveRumbleVideo(src.id);
  return await resolveRumbleChannel(src.space, src.id);
}

// ---------------------------------------------------------------------------
// Wire normalization
// ---------------------------------------------------------------------------

// Rumble sends users and channels in a side table keyed by id, and messages
// reference them by `user_id` / `channel_id`. A message posted with
// `channel_id` set was posted AS that channel (the broadcaster speaking as
// their brand), which Rumble renders with the channel's name and avatar in
// place of the person's — so `channel_id` wins here for display, with the
// underlying user id kept for moderation, which always targets the person.
//
// The output shape intentionally matches the Twitch ChatMsg in lib/irc.js
// field-for-field where the concepts line up (`platform`, `id`, `ts`, `user`,
// `text`, `action`, `reply`), so the merged view on the chat page can render
// one list without special-casing every field. Rumble-only concepts (avatars,
// rants, the posted-as channel) ride in additive fields the Twitch renderer
// simply never reads.
export function toRumbleChatMsg(raw, users, channels) {
  const user = users.get(String(raw.user_id)) || null;
  const channel = raw.channel_id != null ? channels.get(String(raw.channel_id)) || null : null;
  const ts = Date.parse(raw.time);

  // A rant is Rumble's paid highlighted message (its super-chat). The price
  // arrives in cents and the tier's color/duration come from the init
  // payload's config, which the page already has — only the raw amount is
  // needed here.
  const rant = raw.rant
    ? {
        cents: raw.rant.price_cents || 0,
        duration: raw.rant.duration || 0,
        expiresAt: raw.rant.expires_on ? Date.parse(raw.rant.expires_on) : 0,
      }
    : null;

  return {
    platform: "rumble",
    id: String(raw.id),
    ts: Number.isFinite(ts) ? ts : Date.now(),
    user: {
      id: String(raw.user_id || ""),
      login: (user && user.username) || "",
      name: (channel && channel.username) || (user && user.username) || "",
      color: (user && user.color) || "",
      avatar: (channel && channel["image.1"]) || (user && user["image.1"]) || "",
    },
    // Rumble badges are opaque string ids ("verified", "whale-gray",
    // "recurring_subscription", "moderator", "admin"), not Twitch's
    // set/version pairs. Kept as an array — the renderer maps the handful it
    // knows and ignores the rest, so a new Rumble badge can't break a row.
    badges: (user && Array.isArray(user.badges) ? user.badges : []).slice(0, 8),
    text: String(raw.text || ""),
    // No Twitch-style emote ranges exist on Rumble; see the emote note in the
    // file header. Present and empty so the shared renderer needs no branch.
    emotes: "",
    action: false,
    firstMsg: false,
    reply: null,
    rewardId: null,
    msgId: null,
    isFollower: !!(user && user.is_follower),
    // The channel a message was posted as, when it wasn't posted as a person.
    channel: channel ? { id: String(channel.id), name: channel.username || "", avatar: channel["image.1"] || "" } : null,
    rant,
    // Rumble's own notification messages (raids, gifted subs) arrive with
    // type !== "regular"; the page renders those as notices rather than chat.
    notice: raw.type && raw.type !== "regular" ? String(raw.type) : null,
  };
}

// ---------------------------------------------------------------------------

// Whether THIS user may moderate THIS chat.
//
// The shared relay reads the SSE stream anonymously, so the `can_moderate` in
// its status is always false — it describes a logged-out viewer, not any
// particular person. Answering it per user means one authenticated read of the
// same stream: connect with their session cookie, take the `init` frame, hang
// up. The result is cached briefly by the caller, because it changes only when
// someone is promoted or demoted.
//
// Returns false rather than throwing on any failure — an expired session or a
// Rumble blip should hide the mod buttons, not break the page.
export async function fetchCanModerate(session, chatId, { fetchImpl = rumbleFetch } = {}) {
  if (!session || !chatId) return false;
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 15_000);
  try {
    const res = await fetchFromRumble(
      `/chat/api/chat/${encodeURIComponent(chatId)}/stream`,
      {
        headers: { Accept: "text/event-stream", Cookie: `u_s=${session}` },
        signal: ctl.signal,
        timeoutMs: 0,
      },
      fetchImpl,
    );
    if (res.status !== 200 || !res.body) return false;

    const decoder = new TextDecoder();
    let buffer = "";
    for await (const chunk of res.body) {
      buffer += decoder.decode(chunk, { stream: true });
      let sep;
      while ((sep = buffer.indexOf("\n\n")) >= 0) {
        const frame = buffer.slice(0, sep);
        buffer = buffer.slice(sep + 2);
        for (const line of frame.split("\n")) {
          if (!line.startsWith("data:")) continue;
          try {
            const event = JSON.parse(line.slice(5).trim());
            if (event && event.type === "init") return !!(event.data && event.data.can_moderate);
          } catch {
            /* keep reading — a partial frame is not a verdict */
          }
        }
      }
      // The init frame is always first; anything past this much data without
      // one means the shape changed and waiting longer will not help.
      if (buffer.length > 2_000_000) return false;
    }
    return false;
  } catch {
    return false;
  } finally {
    clearTimeout(timer);
    try {
      ctl.abort();
    } catch {
      /* already done */
    }
  }
}
