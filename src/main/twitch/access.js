// Is the logged-in user the broadcaster or a moderator of a channel? Twitch has no
// "am I a mod here" call, so this lists the channels the user moderates
// (GET /moderation/channels, needs user:read:moderated_channels) and caches it.
import * as auth from "./auth.js";
import { helixAll } from "./helix.js";
import { log } from "./rt.js";

const TTL_MS = 10 * 60_000;
let cache = null; // {userId, at, ids:Set}
let inflight = null;

export function invalidate() {
  cache = null;
}

async function load(rec) {
  try {
    const { rows } = await helixAll("/moderation/channels", { query: { user_id: rec.user.id, first: 100 } });
    cache = { userId: rec.user.id, at: Date.now(), ids: new Set(rows.map((r) => String(r.broadcaster_id))) };
  } catch (err) {
    log("could not list moderated channels", err.message);
    // A stale list beats no list; only give up when there is nothing at all.
    if (cache && cache.userId === rec.user.id) cache.at = Date.now() - TTL_MS + 60_000; // retry in a minute
  }
}

/**
 * {loggedIn, me, isBroadcaster, isMod}. isMod is true / false, or null when it cannot be known
 * (the token predates the scope, or Twitch could not be reached and nothing is cached).
 */
export async function access(channelId) {
  const rec = auth.current();
  if (!rec) return { loggedIn: false, me: null, isBroadcaster: false, isMod: false };
  const out = { loggedIn: true, me: rec.user, isBroadcaster: String(channelId) === rec.user.id, isMod: null };
  if (out.isBroadcaster) return { ...out, isMod: true };
  if (!rec.scopes.includes("user:read:moderated_channels")) return out;
  if (!cache || cache.userId !== rec.user.id || Date.now() - cache.at > TTL_MS) {
    if (!inflight) inflight = load(rec).finally(() => (inflight = null));
    await inflight;
  }
  if (cache && cache.userId === rec.user.id) out.isMod = cache.ids.has(String(channelId));
  return out;
}
