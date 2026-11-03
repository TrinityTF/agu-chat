// Helix with the logged-in user's token. A 401 refreshes the token once and retries.
import { requestJson, HttpError } from "./http.js";
import * as auth from "./auth.js";

const HELIX = "https://api.twitch.tv/helix";

export function loginRequired() {
  const e = new HttpError(401, "Log in to Twitch");
  e.local = true; // ours, not Twitch's: routes answer it as-is instead of "Twitch error: ..."
  return e;
}

function qs(query) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(query || {})) {
    if (v === undefined || v === null || v === "") continue;
    if (Array.isArray(v)) for (const x of v) p.append(k, String(x));
    else p.append(k, String(v));
  }
  const s = p.toString();
  return s ? `?${s}` : "";
}

/** helix("/chat/settings", {query:{broadcaster_id}}) -> parsed JSON, or null on 204. Throws HttpError. */
export async function helix(path, { method = "GET", query, body } = {}) {
  for (let attempt = 0; ; attempt++) {
    const rec = await auth.freshRecord();
    if (!rec) throw loginRequired();
    try {
      return await requestJson(HELIX + path + qs(query), {
        method,
        headers: { Authorization: `Bearer ${rec.accessToken}`, "Client-Id": rec.clientId },
        json: body,
      });
    } catch (err) {
      if (err instanceof HttpError && err.status === 401 && attempt === 0 && (await auth.refreshNow())) continue;
      throw err;
    }
  }
}

/** Follows pagination.cursor and returns every `data` row (capped so a huge list cannot run away). */
export async function helixAll(path, { query, maxPages = 20 } = {}) {
  const rows = [];
  let after;
  let last = null;
  for (let page = 0; page < maxPages; page++) {
    last = await helix(path, { query: { ...query, after } });
    rows.push(...(last?.data || []));
    after = last?.pagination?.cursor;
    if (!after) break;
  }
  return { rows, template: last?.template };
}

// login -> {id, login, name}, cached for an hour (logins rarely change hands).
const userCache = new Map();
const USER_TTL_MS = 60 * 60_000;

export async function userByLogin(login) {
  const hit = userCache.get(login);
  if (hit && Date.now() - hit.at < USER_TTL_MS) return hit.user;
  const page = await helix("/users", { query: { login } });
  const u = page?.data?.[0];
  if (!u) return null;
  const user = { id: String(u.id), login: u.login, name: u.display_name || u.login };
  userCache.set(login, { user, at: Date.now() });
  if (userCache.size > 500) userCache.delete(userCache.keys().next().value);
  return user;
}

export function clearUserCache() {
  userCache.clear();
}
