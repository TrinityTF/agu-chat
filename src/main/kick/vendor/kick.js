// Adapted from modtools.agu.lol render/lib/kick.js and render/lib/kickApi.js (render commit 7704aec).
// Changes: the site builds rows from Kick WEBHOOK payloads (chat.message.sent) and reads the official API with
// an app token; a desktop app has no public URL for webhooks, so toKickChatMsg now maps the payload Kick's own
// web client receives over its Pusher websocket (App\Events\ChatMessageEvent, and the same shape from the
// history endpoint) onto the identical row shape; fetchKickEmotes / subBadgeFor are unchanged except that the
// UA is passed in (the honest product UA: Kick's WAF 403s browser UAs from a Node TLS stack) and subscriber
// badges come from the channel lookup we already make instead of a second request. New: getKickChannel,
// getRealtimeDescriptor, fetchHistory. Everything is public, unauthenticated, read-only.
//
// THE USER-AGENT RULE: never a browser string here. Measured on the site's VM: no UA -> 200, an honest product
// UA -> 200, a Chrome UA -> 403 "Request blocked by security policy".

const SLUG_RE = /^[A-Za-z0-9_-]{1,25}$/;

/**
 * kick.com/<slug>, or a bare slug. Kick's URLs hyphenate what a username spells with underscores
 * ("some_user" is kick.com/some-user), so underscores are normalised to hyphens for the lookup.
 */
export function parseKickSource(input) {
  const raw = String(input || "").trim();
  if (!raw) return null;

  let slug = raw;
  if (/^https?:\/\//i.test(raw) || /^(?:www\.)?kick\.com/i.test(raw)) {
    let u;
    try {
      u = new URL(/^https?:\/\//i.test(raw) ? raw : `https://${raw}`);
    } catch {
      return null;
    }
    if (!/(^|\.)kick\.com$/i.test(u.hostname)) return null;
    // kick.com/<slug> and nothing deeper: a link to a clip or video points at a channel too.
    slug = (u.pathname.split("/").filter(Boolean)[0] || "").trim();
  }
  slug = slug.replace(/^@/, "");
  return SLUG_RE.test(slug) ? slug.toLowerCase().replace(/_/g, "-") : null;
}

// ---------------------------------------------------------------------------
// Fetch helpers
// ---------------------------------------------------------------------------

async function kickFetch(url, { userAgent, signal, method = "GET", body, timeoutMs = 12_000 } = {}) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), timeoutMs);
  if (signal) {
    if (signal.aborted) ctl.abort();
    else signal.addEventListener("abort", () => ctl.abort(), { once: true });
  }
  try {
    return await fetch(url, {
      method,
      headers: { Accept: "application/json", "User-Agent": userAgent, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
      body: body !== undefined ? JSON.stringify(body) : undefined,
      signal: ctl.signal,
    });
  } finally {
    clearTimeout(timer);
  }
}

/**
 * Resolve a slug to what the socket and header need. null when the channel does not exist.
 * `userId` is the broadcaster's user id (what the site calls userId); `channelId` is the one `channel.<id>`
 * events and the history endpoint use; `chatroomId` is the one `chatrooms.<id>.v2` uses. They differ.
 */
export async function getKickChannel(slug, opts) {
  const r = await kickFetch(`https://kick.com/api/v2/channels/${encodeURIComponent(slug)}`, opts);
  if (r.status === 404) return null;
  if (!r.ok) throw new Error(`Kick answered ${r.status}`);
  const ch = await r.json();
  if (!ch || !ch.id) return null;
  const ls = ch.livestream || null;
  const cat = ls && Array.isArray(ls.categories) && ls.categories[0];
  return {
    channelId: ch.id,
    userId: String(ch.user_id ?? ""),
    chatroomId: ch.chatroom && ch.chatroom.id,
    slug: ch.slug || slug,
    live: !!(ls && ls.is_live !== false),
    title: (ls && ls.session_title) || "",
    category: (cat && cat.name) || "",
    viewers: Number((ls && ls.viewer_count) || 0),
    subBadges: subBadgesFrom(ch),
  };
}

/**
 * Kick's own web client asks this endpoint which realtime provider and credentials to use for a channel's
 * chat (the Pusher app key is NOT hard-coded in its JS any more). Public, no auth. Returns {appKey, cluster}.
 */
export async function getRealtimeDescriptor(channelId, opts, clientId) {
  const r = await kickFetch(`https://web.kick.com/api/v1/realtime/channels/${encodeURIComponent(channelId)}/chat/connection`, {
    ...opts,
    method: "POST",
    body: { client: { id: clientId, type: "web" }, capabilities: { accepted_providers: [{ provider: "pusher" }] } },
  });
  if (!r.ok) throw new Error(`Kick realtime descriptor answered ${r.status}`);
  const j = await r.json();
  const data = j && j.data;
  for (const c of (data && data.connections) || []) {
    const cred = c && c.credentials;
    if (c.provider === "pusher" && cred && cred.app_key && cred.cluster) return { appKey: String(cred.app_key), cluster: String(cred.cluster) };
  }
  throw new Error("Kick offered no realtime provider this client speaks");
}

/** Recent chat for the backlog, oldest first. Public; best-effort (the caller tolerates failure). */
export async function fetchHistory(channelId, opts) {
  const r = await kickFetch(`https://kick.com/api/v2/channels/${encodeURIComponent(channelId)}/messages`, opts);
  if (!r.ok) throw new Error(`Kick history answered ${r.status}`);
  const j = await r.json();
  const list = (j && j.data && j.data.messages) || [];
  return list.slice().sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at));
}

// ---------------------------------------------------------------------------
// Row shape
// ---------------------------------------------------------------------------

/**
 * A Kick chat message (Pusher App\Events\ChatMessageEvent, or one entry of the history endpoint) -> the row
 * the chat page renders, field for field what the site's toKickChatMsg produces from a webhook.
 *
 * The emote markup (`[emote:1234:name]`) is left INSIDE `text` on purpose: it is self-delimiting, so the
 * renderer splits on it directly and it cannot drift out of sync with an offset table.
 */
export function toKickChatMsg(ev) {
  const sender = (ev && ev.sender) || {};
  const identity = sender.identity || {};
  const badges = Array.isArray(identity.badges) ? identity.badges : [];
  const ts = Date.parse(ev && ev.created_at);
  // The history endpoint ships `metadata` as a JSON string; the socket ships an object.
  let meta = ev && ev.metadata;
  if (typeof meta === "string") {
    try {
      meta = JSON.parse(meta);
    } catch {
      meta = null;
    }
  }
  const orig = meta && meta.original_message;
  const origSender = meta && meta.original_sender;

  return {
    platform: "kick",
    id: String((ev && (ev.id || ev.message_id)) || ""),
    ts: Number.isFinite(ts) ? ts : Date.now(),
    user: {
      // The numeric id is what every moderation call needs, and the one identifier Kick's API can act on.
      id: String(sender.id ?? sender.user_id ?? ""),
      login: sender.slug || sender.channel_slug || "",
      name: sender.username || sender.slug || sender.channel_slug || "",
      color: identity.color || identity.username_color || "",
      // The socket payload carries no avatar or verified flag; the row keeps the fields so the renderer needs no branch.
      avatar: sender.profile_picture || "",
      verified: !!sender.is_verified,
      anonymous: !!sender.is_anonymous,
    },
    // Kept as Kick sends them — {type, text, count} — so a badge Kick adds next month shows as its own name.
    badges: badges.slice(0, 8).map((b) => ({ type: String(b.type || ""), text: String(b.text || ""), count: Number(b.count) || 0 })),
    text: String((ev && ev.content) || ""),
    reply:
      ev && ev.type === "reply" && orig
        ? { id: String(orig.id || ""), text: String(orig.content || ""), name: String((origSender && origSender.username) || "") }
        : null,
    action: false,
    notice: ev && ev.type === "celebration" ? "celebration" : null,
  };
}

// ---------------------------------------------------------------------------
// Emotes for the picker (unchanged from kickApi.js apart from the UA)
// ---------------------------------------------------------------------------

const EMOTE_TTL_MS = 30 * 60_000;
const emoteCache = new Map(); // slug -> { at, emotes }

export async function fetchKickEmotes(slug, { userAgent, signal, force = false } = {}) {
  const key = String(slug || "").toLowerCase();
  if (!key) return {};
  const hit = emoteCache.get(key);
  if (!force && hit && Date.now() - hit.at < EMOTE_TTL_MS) return hit.emotes;

  try {
    const r = await kickFetch(`https://kick.com/emotes/${encodeURIComponent(key)}`, { userAgent, signal });
    if (!r.ok) throw new Error(String(r.status));
    const groups = await r.json();
    const emotes = {};
    for (const g of Array.isArray(groups) ? groups : []) {
      // The channel's own group has a numeric id and a slug; Kick's shared sets arrive as "Global" and "Emoji".
      const group = g && g.slug ? "channel" : String((g && g.id) || "global").toLowerCase();
      for (const e of (g && g.emotes) || []) {
        if (!e || !e.name) continue;
        emotes[e.name] = { id: String(e.id), group, subsOnly: !!e.subscribers_only };
      }
    }
    emoteCache.set(key, { at: Date.now(), emotes });
    return emotes;
  } catch {
    // Serve a stale list rather than emptying a working picker over one blocked request. A blocked list costs
    // the picker and not the chat: an emote inside a message renders from files.kick.com by id alone.
    if (hit) return hit.emotes;
    emoteCache.set(key, { at: Date.now(), emotes: {} });
    return {};
  }
}

// A channel's own subscriber badge artwork, [{months, url}] — the only badges Kick serves as images.
function subBadgesFrom(ch) {
  const badges = [];
  for (const b of (ch && ch.subscriber_badges) || []) {
    const url = (b && b.badge_image && b.badge_image.src) || "";
    if (!url) continue;
    badges.push({ months: Number(b.months) || 0, url: String(url) });
  }
  badges.sort((a, b) => a.months - b.months);
  return badges;
}

/** The badge a subscriber of `months` months wears: the highest tier they have reached. */
export function subBadgeFor(badges, months) {
  if (!badges || !badges.length) return "";
  const m = Number(months) || 0;
  let best = badges[0];
  for (const b of badges) if (b.months <= m && b.months >= best.months) best = b;
  return best.url;
}
