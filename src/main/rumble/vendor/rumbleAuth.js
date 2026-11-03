// Vendored from modtools.agu.lol render/lib/rumbleAuth.js (render commit 7704aec). Changes: the browser User-Agent is replaced by the app's honest UA (../ua.js); the Origin/Referer headers on chat writes are unchanged. The password is hashed and dropped inside login(); only the u_s session is kept (by ../index.js, in ctx.secrets).
// Rumble account auth + the chat write path (send, moderate, delete).
//
// WHY THERE IS A PASSWORD LOGIN HERE AT ALL
// -----------------------------------------
// Rumble has no OAuth for third parties — there is no app registration, no
// consent screen, no scoped token to ask for. The only way to act as a user is
// to hold a Rumble session, and the only way to get one is to log in as them.
// So this module does what Rumble's own web client does: fetch the account's
// salts, hash the password client-side (the plaintext is never sent, and never
// stored here — see `login` below), and exchange that for a session token.
//
// The token is `u_s`, the same cookie a browser would hold. It is stored per
// APP USER, not per channel, so a broadcaster and each of their mods act as
// themselves and Rumble attributes bans and messages to the right person —
// which is the whole reason this isn't one shared bot account.
//
// USE A FRONT-END HOST (wn0, web7), NOT THE APEX
// ----------------------------------------------
// Same rule as lib/rumble.js, and for the same reason: Cloudflare challenges
// Node's TLS fingerprint on rumble.com. `service.php` is unprotected on the
// front-ends — verified: a POST to `user.get_salts` there returned real salts
// from Node, while the apex returns the interactive challenge. This is what
// makes a server-side login possible at all. (Since 2026-10-27 only wn0 still
// serves service.php; web7 serves just /chat/api/*.)
//
// SECRET HANDLING
// ---------------
// The password exists only for the duration of one `login()` call: it is
// hashed, used, and dropped — never written to storage, never logged, never
// echoed back. The session token IS persisted (it has to be, or every page
// load would need the password again) and is treated the way this codebase
// already treats seToken and the Spotify refresh token: stored in the user's
// own hub and never included in any response to a client. Callers must keep
// that property — see server.js's /api/rumble/account, which returns only
// `{connected, username}`.
import crypto from "node:crypto";
import { fetchFromRumble } from "./rumbleHosts.js";
import { getUserAgent } from "../ua.js";

// Which front-end host each request goes to, and the fallback when one
// closes, is lib/rumbleHosts.js.
const FETCH_TIMEOUT_MS = 20_000;
const SESSION_COOKIE = "u_s";

// ---------------------------------------------------------------------------
// Password hashing
//
// Reimplemented from Rumble's own login behaviour and verified against the
// live endpoint — deliberately NOT transcribed from any GPL reference
// implementation, since this repo carries no such licence.
//
// The scheme is salted, stretched MD5. It is weak by modern standards, but it
// is not ours to choose: the server compares against exactly this, so the only
// options are "match it" or "cannot log in".
// ---------------------------------------------------------------------------

const md5 = (message) => crypto.createHash("md5").update(message).digest("hex");

// md5(salt + password), then `iterations` more rounds of md5(previous +
// password). The password is folded in on every round, which is what makes
// this a stretch rather than a plain salted digest.
function stretch(password, salt, iterations = 128) {
  let current = md5(salt + password);
  for (let i = 0; i < iterations; i++) current = md5(current + password);
  return current;
}

// Rumble hands out three salts and expects three comma-joined values back:
// the first salt's stretched hash re-hashed with the second salt, the third
// salt's stretched hash, and the second salt echoed verbatim.
export function passwordHashes(password, salts) {
  if (!Array.isArray(salts) || salts.length < 3) throw new Error("rumble: expected three salts");
  const first = md5(stretch(password, salts[0], 128) + salts[1]);
  const second = stretch(password, salts[2], 128);
  return `${first},${second},${salts[1]}`;
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

async function rumbleFetch(url, init = {}) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), init.timeoutMs || FETCH_TIMEOUT_MS);
  try {
    return await fetch(url, {
      ...init,
      signal: ctl.signal,
      redirect: "manual",
      headers: { "User-Agent": getUserAgent(), "Accept-Language": "en-US,en;q=0.9", ...(init.headers || {}) },
    });
  } finally {
    clearTimeout(timer);
  }
}

// One service.php call. `session` is the raw u_s token, or null for the
// unauthenticated steps of login.
//
// Rumble answers 200 with an `errors` array as often as it uses a real status
// code, so both are folded into one thrown Error carrying Rumble's own
// message — that text is what the settings UI shows the user ("invalid
// username or password", "too many attempts"), and it is far more useful than
// anything this layer could invent.
async function servicePhp(name, { data = {}, params = {}, session = null } = {}) {
  const qs = new URLSearchParams({ name, ...params });
  const body = new URLSearchParams();
  for (const [k, v] of Object.entries(data)) {
    if (v !== null && v !== undefined) body.set(k, String(v));
  }

  const r = await fetchFromRumble(
    `/service.php?${qs}`,
    {
      method: "POST",
      headers: {
        "Content-Type": "application/x-www-form-urlencoded",
        Accept: "application/json",
        ...(session ? { Cookie: `${SESSION_COOKIE}=${session}` } : {}),
      },
      body: body.toString(),
    },
    rumbleFetch,
  );

  const text = await r.text();
  if (text.includes("Just a moment")) {
    // The one failure mode worth naming explicitly, because the fix is a code
    // change (wrong host) rather than anything the user did.
    throw new Error("Rumble served a bot challenge — this request must use a Rumble front-end host (wn0), not rumble.com");
  }

  let json = null;
  try {
    json = JSON.parse(text);
  } catch {
    throw new Error(`Rumble returned a non-JSON response to ${name} (HTTP ${r.status})`);
  }

  // Flagged the same way chatApi flags its refusals: moderation runs through
  // here, and without this a Rumble "no" would come back to the browser as a
  // 5xx, which the proxy replaces with an error page carrying no CORS headers.
  const err = json && Array.isArray(json.errors) && json.errors[0];
  if (err) {
    const e = new Error(err.message || `Rumble rejected ${name}`);
    e.rumbleRefused = true;
    e.rumbleStatus = r.status;
    if (json.user && json.user.logged_in === false) e.rumbleExpired = true;
    throw e;
  }
  if (!r.ok) {
    const e = new Error(`Rumble ${name} failed (HTTP ${r.status})`);
    e.rumbleRefused = r.status >= 400 && r.status < 500;
    e.rumbleStatus = r.status;
    throw e;
  }

  return { json, headers: r.headers };
}

function sessionFromSetCookie(headers) {
  // Node exposes repeated Set-Cookie via getSetCookie(); fall back to the
  // folded single header on older runtimes.
  const all = typeof headers.getSetCookie === "function" ? headers.getSetCookie() : [headers.get("set-cookie") || ""];
  for (const line of all) {
    for (const part of String(line).split(/;\s*/)) {
      if (part.startsWith(`${SESSION_COOKIE}=`)) {
        const value = part.slice(SESSION_COOKIE.length + 1).trim();
        if (value) return value;
      }
    }
  }
  return null;
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

export async function getSalts(username) {
  const { json } = await servicePhp("user.get_salts", { data: { username }, params: { response_type: "session" } });
  const salts = json && json.data && json.data.salts;
  if (!Array.isArray(salts) || salts.length < 3) throw new Error("Rumble did not return login salts");
  return salts;
}

// Logs in, returning either a session or a 2FA challenge to continue with.
//
// `password` is consumed here and nowhere else: it is turned into hashes
// immediately and never leaves this function. Callers must not persist it.
//
// Returns:
//   { ok: true, session }                      — logged in
//   { ok: false, twoFactor: { totpId, userKey } } — needs a code, call
//                                                   completeTwoFactor next
export async function login(username, password) {
  const salts = await getSalts(username);
  const hashes = passwordHashes(password, salts);

  // Rumble decides whether 2FA applies in a dedicated first step, and this
  // call is also what rejects a wrong password — so it runs before user.login
  // even for accounts without 2FA.
  const first = await servicePhp("user.2fa.first_step", {
    data: {
      legacy_password: 1,
      login: username,
      password: hashes,
      redirect_url: "https://rumble.com",
    },
  });

  const info = (first.json && first.json.data) || {};
  if (info.totp_id) {
    return { ok: false, twoFactor: { totpId: String(info.totp_id), userKey: String(info.user_key || "") } };
  }

  const { json } = await servicePhp("user.login", { data: { username, password_hashes: hashes } });
  const session = json && json.data && json.data.session;
  if (!session) throw new Error("Rumble accepted the login but returned no session");
  return { ok: true, session: String(session) };
}

// Second half of a 2FA login. Unlike user.login, this one delivers the session
// as a Set-Cookie header rather than in the JSON body.
export async function completeTwoFactor({ totpId, userKey }, code) {
  const { headers } = await servicePhp("user.2fa.verify_totp", {
    data: { code, totp_id: totpId, user_key: userKey, redirect_url: "https://rumble.com" },
  });
  const session = sessionFromSetCookie(headers);
  if (!session) throw new Error("Rumble accepted the code but returned no session");
  return { ok: true, session };
}

// Cheap read-only "is this session still good, and who is it".
//
// Every service.php response carries a `user` envelope with `logged_in` and
// the account id, so any authenticated endpoint answers this; the notification
// check is used because it reads nothing interesting and changes nothing.
// Returns null for an expired or revoked session rather than throwing — that
// is an ordinary state (Rumble sessions do lapse), not an error.
export async function whoami(session) {
  if (!session) return null;
  try {
    const { json } = await servicePhp("user.has_unread_notifications", { session });
    const user = json && json.user;
    if (!user || !user.logged_in || !user.id) return null;
    return { id: String(user.id), username: user.username ? String(user.username) : "" };
  } catch {
    return null;
  }
}

export async function logout(session) {
  if (!session) return;
  try {
    await servicePhp("user.logout", { session });
  } catch {
    // Best effort. A session we are discarding locally does not need Rumble's
    // agreement, and a failure here must not block the user disconnecting.
  }
}

// ---------------------------------------------------------------------------
// Chat write path
//
// These are NOT service.php — they are the chat app's own routes, the same
// ones rumble.com's player calls, and they take JSON.
// ---------------------------------------------------------------------------

async function chatApi(path, { method = "POST", body, session, referer } = {}) {
  const r = await fetchFromRumble(path, {
    method,
    headers: {
      Accept: "application/json",
      ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      ...(session ? { Cookie: `${SESSION_COOKIE}=${session}` } : {}),
      // Rumble's own client only ever calls these from a page on rumble.com,
      // so every real request carries an Origin, a Referer pointing at the
      // video, and the XHR marker. Sending none of them is the difference
      // between our request and a genuine one, and is the most likely reason
      // a valid session still gets "Access denied" on a write — reads are not
      // CSRF-protected, which is why the feed worked while sending did not.
      // Captured from the live page: their helper sets only a content-type,
      // and the browser supplies these two for it. Nothing else, no CSRF token.
      Origin: "https://rumble.com",
      Referer: referer || "https://rumble.com/",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  }, rumbleFetch);

  const text = await r.text();
  if (text.includes("Just a moment")) {
    throw new Error("Rumble served a bot challenge — this request must use a Rumble front-end host (wn0), not rumble.com");
  }

  let json = null;
  if (text) {
    try {
      json = JSON.parse(text);
    } catch {
      throw new Error(`Rumble returned a non-JSON response (HTTP ${r.status})`);
    }
  }

  // 403 with `logged_in: false` is specifically "your saved Rumble session
  // expired", which the UI must be able to tell apart from a refused action so
  // it can prompt for a re-connect instead of showing a generic failure.
  if (json && json.user && json.user.logged_in === false) {
    const e = new Error("Your Rumble session has expired — reconnect your account in Settings");
    e.rumbleExpired = true;
    throw e;
  }

  // A refusal Rumble deliberately returned is NOT a gateway failure. Flagging
  // it lets the caller answer 4xx: a 5xx here got replaced by the proxy's own
  // error page, which carries no CORS headers, so the browser reported a CORS
  // problem instead of showing the user what Rumble actually said.
  const err = json && Array.isArray(json.errors) && json.errors[0];
  if (err) {
    const e = new Error(err.message || "Rumble rejected the request");
    e.rumbleRefused = true;
    e.rumbleStatus = r.status;
    throw e;
  }
  if (!r.ok) {
    const e = new Error(`Rumble request failed (HTTP ${r.status})`);
    e.rumbleRefused = r.status >= 400 && r.status < 500;
    e.rumbleStatus = r.status;
    throw e;
  }
  return json;
}

// Sends a chat message, or runs a slash command.
//
// Rumble routes BOTH through /chat/command — a leading "/timeout user 300" is
// parsed server-side exactly as it is when typed into their own chat box. That
// is why moderation below is expressed as commands rather than as a separate
// API: there isn't one, and this is the path their client uses.
// Sends a chat message.
//
// WHY NOT /chat/command, WHICH IS WHAT RUMBLE'S OWN CLIENT USES
// -------------------------------------------------------------
// Their client calls eF(method, "/chat/command", …) with a BARE path, so it
// resolves against the page origin: https://rumble.com/chat/command. Its chat
// API calls are different — those are prefixed with the API base and go to
// https://web<N>.rumble.com/chat/api/… (captured from the live page: the
// delete call is `A + "/chat/" + chatId + "/message/" + id`).
//
// We cannot use the apex: Cloudflare challenges Node's TLS fingerprint there,
// which is the whole reason everything else in this codebase talks to web<N>.
// And /chat/command on web<N> exists but answers a valid session with "Access
// denied" — it belongs to the apex, not to this host.
//
// So sending goes through the chat API's own message route on web<N>, the REST
// counterpart of the delete route above. If Rumble refuses that, we fall back
// to /chat/command so a working path isn't lost to a wrong guess about the
// body — and the caller logs which one answered, so this stops being guesswork.
// Every write to the chat API is wrapped in a `data` envelope — Rumble
// answers a bare body with "data: Required but missing". `request_id` is a
// client-generated id it expects on a send; 32 random bytes, base64, padding
// stripped, capped at 43 characters is the shape it accepts.
function requestId() {
  return crypto.randomBytes(32).toString("base64").replace(/=+$/, "").slice(0, 43);
}

export async function sendMessage(session, chatId, message, { referer, channelId = null } = {}) {
  const json = await chatApi(`/chat/api/chat/${encodeURIComponent(chatId)}/message`, {
    session,
    referer,
    body: {
      data: {
        request_id: requestId(),
        message: { text: message },
        // Present and null on purpose: Rumble validates the envelope's shape,
        // not just its contents. `rant` would carry a paid amount; `channel_id`
        // posts as a channel instead of as the person.
        rant: null,
        channel_id: channelId ? Number(channelId) : null,
      },
    },
  });
  return { via: "chat-api", json };
}

// Deletes one message. This one IS a REST-shaped route on the chat API.
export async function deleteMessage(session, chatId, messageId) {
  return await chatApi(`/chat/api/chat/${encodeURIComponent(chatId)}/message/${encodeURIComponent(messageId)}`, {
    method: "DELETE",
    session,
  });
}

// Moderation, expressed as the slash commands Rumble's own chat accepts.
//
// `seconds` of 0/absent means an indefinite ban rather than a timed mute.
// Usernames are sent without the leading @, matching what the chat box does.
// Moderation, via service.php rather than /chat/command.
//
// /chat/command is an apex route (see sendMessage above) and answers a valid
// session "Access denied" on web<N>, so slash commands are not available to
// us at all. service.php is — it is the same endpoint the login uses — and it
// exposes muting directly, which is what /timeout and /ban do underneath.
//
// A "video" mute is a timeout on this stream; a "total" mute is the ban that
// applies everywhere. Rumble takes the target by NAME, not id.
export function timeoutUser(session, videoId, username, seconds) {
  return servicePhp("moderation.mute", {
    session,
    data: {
      user_to_mute: String(username).replace(/^@/, ""),
      entity_type: "user",
      video: Number(videoId),
      duration: Math.max(1, Math.floor(seconds)),
      type: "video",
    },
  });
}

export function banUser(session, videoId, username) {
  return servicePhp("moderation.mute", {
    session,
    data: {
      user_to_mute: String(username).replace(/^@/, ""),
      entity_type: "user",
      video: Number(videoId),
      // No duration: a total mute with no end is the permanent ban.
      type: "total",
    },
  });
}

// Undoing a mute needs the id of the mute RECORD, which Rumble hands out only
// when listing a channel's mutes — nothing we hold from a chat message gives
// it. Rather than send a request that cannot work, say so: this is one thing
// that genuinely has to happen on Rumble.
export function unbanUser() {
  const e = new Error("Unbanning has to be done on Rumble — it needs the mute record, which chat messages don't carry");
  e.rumbleRefused = true;
  return Promise.reject(e);
}
