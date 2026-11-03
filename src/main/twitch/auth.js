// Twitch account: OAuth device code flow with the user's own PUBLIC client id
// (no client secret, no redirect server), token storage, refresh and the hourly
// validate Twitch requires. Never falls back to another app's client id.
import { EventEmitter } from "node:events";
import { rt, log } from "./rt.js";
import { requestJson, HttpError } from "./http.js";

const ID = "https://id.twitch.tv/oauth2";
const HELIX = "https://api.twitch.tv/helix";
const SECRET_KEY = "twitch";
const VALIDATE_EVERY_MS = 60 * 60_000;
// Refresh this long before the token runs out, so a request never rides on a dying token.
const REFRESH_MARGIN_MS = 5 * 60_000;
export const EXPIRED_MESSAGE = "Twitch login expired, log in again";

// Everything the chat page asks for, in one grant (the user logs in once).
export const SCOPES = [
  "user:read:chat",
  "user:write:chat", // /api/chat/send
  "user:read:emotes", // the emote picker lists the emotes this user can actually send
  "user:read:moderated_channels", // which channels the user moderates: gates the named mod feed and mod buttons
  // Mod actions the chat page offers.
  "moderator:manage:banned_users", // ban / timeout / unban
  "moderator:manage:chat_messages", // delete, clear, pin
  "moderator:manage:warnings",
  "moderator:manage:chat_settings", // the Modes panel
  "moderator:manage:announcements",
  "moderator:manage:shoutouts",
  "moderator:manage:shield_mode",
  "moderator:manage:automod", // AutoMod Allow / Deny
  "moderator:manage:blocked_terms",
  // Read scopes EventSub's channel.moderate v2 and suspicious-user events demand as a set.
  "moderator:read:blocked_terms",
  "moderator:read:chat_settings",
  "moderator:read:unban_requests",
  "moderator:read:moderators",
  "moderator:read:vips",
  "moderator:read:suspicious_users",
  "moderator:read:followers", // "following since" on the user card
  "moderator:read:chatters", // the chatter list behind name suggestions
  "channel:moderate", // legacy chat-moderation scope, requested because the agu.lol chat page did
  "channel:manage:raids", // /raid: broadcaster only, Twitch does not let mods raid
  // Only used on the user's own channel, to label channel-point redeems and Bits Power-ups in chat.
  "channel:read:redemptions",
  "bits:read",
];

export const events = new EventEmitter(); // "change": login / logout / expiry / scope change (not routine refreshes)

let record = null; // {accessToken, refreshToken, expiresAt, scopes, clientId, user:{id,login,name}}
let pending = null; // {id, clientId, deviceCode, userCode, verificationUri, expiresAt, interval, timer}
let lastError = null;
let refreshing = null;
let refreshTimer = null;
let validateTimer = null;
let seq = 0;

function changed() {
  events.emit("change");
  try {
    rt.ctx?.accountsChanged?.();
  } catch (err) {
    log("accountsChanged failed", err);
  }
}

function persist() {
  try {
    if (record) rt.ctx.secrets.set(SECRET_KEY, record);
    else rt.ctx.secrets.delete(SECRET_KEY);
  } catch (err) {
    log("could not store the Twitch token", err);
  }
}

/** Called once at startup. Does not block on the network. */
export function init(ctx) {
  rt.ctx = ctx;
  let saved = null;
  try {
    saved = ctx.secrets.get(SECRET_KEY);
  } catch (err) {
    log("could not read the stored Twitch token", err);
  }
  if (saved?.accessToken && saved?.user?.id) {
    record = saved;
    scheduleRefresh();
    startValidateLoop();
    void validate();
  }
}

export function state() {
  const s = { connected: !!record };
  if (record) {
    s.user = { ...record.user };
    s.scopes = [...record.scopes];
  }
  if (pending) s.pending = { userCode: pending.userCode, verificationUri: pending.verificationUri, expiresAt: pending.expiresAt };
  if (lastError) s.error = lastError;
  return s;
}

/** Current token, synchronously: for other modules (ctx.platforms.twitch.getUserToken()). May be seconds from refresh. */
export function getUserToken() {
  if (!record) return null;
  return { token: record.accessToken, user: { ...record.user }, scopes: [...record.scopes], clientId: record.clientId };
}

export const current = () => record;

/** The record, refreshed first if it is about to run out. Null when logged out. */
export async function freshRecord() {
  if (record && record.expiresAt - Date.now() < 60_000) await refreshNow();
  return record;
}

export function hasScope(scope) {
  return !!record && record.scopes.includes(scope);
}

// ---------- login (device code flow) ----------

export async function login() {
  const clientId = String(rt.ctx.settings.get().twitchClientId || "").trim();
  if (!clientId) {
    throw new Error(
      "Paste your Twitch client ID in Settings first. Register a free app at dev.twitch.tv/console with client type \"Public\", then copy its client ID."
    );
  }
  cancel();
  lastError = null;
  let dev;
  try {
    dev = await requestJson(`${ID}/device`, { method: "POST", form: { client_id: clientId, scopes: SCOPES.join(" ") } });
  } catch (err) {
    if (err instanceof HttpError && err.status >= 400 && err.status < 500) {
      throw new Error(
        `Twitch did not accept that client ID (${err.message}). Check it in Settings, and that the app's client type is "Public".`
      );
    }
    throw new Error(err.message || "Could not start the Twitch login.");
  }
  if (!dev?.device_code || !dev?.user_code || !dev?.verification_uri) throw new Error("Twitch sent an unexpected reply to the login request.");
  const flow = {
    id: ++seq,
    clientId,
    deviceCode: dev.device_code,
    userCode: dev.user_code,
    verificationUri: dev.verification_uri,
    expiresAt: Date.now() + (Number(dev.expires_in) || 600) * 1000,
    interval: Math.max(1, Number(dev.interval) || 5),
    timer: null,
  };
  pending = flow;
  schedulePoll(flow);
  changed();
  return { userCode: flow.userCode, verificationUri: flow.verificationUri, expiresAt: flow.expiresAt };
}

function schedulePoll(flow) {
  flow.timer = setTimeout(() => void poll(flow), flow.interval * 1000);
  flow.timer.unref?.();
}

function failFlow(flow, message) {
  if (pending !== flow) return;
  clearTimeout(flow.timer);
  pending = null;
  lastError = message;
  changed();
}

async function poll(flow) {
  if (pending !== flow) return;
  if (Date.now() > flow.expiresAt) return failFlow(flow, "The Twitch login code expired. Start the login again.");
  let tok;
  try {
    tok = await requestJson(`${ID}/token`, {
      method: "POST",
      form: {
        client_id: flow.clientId,
        scopes: SCOPES.join(" "),
        device_code: flow.deviceCode,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      },
    });
  } catch (err) {
    const msg = String(err.message || "").toLowerCase();
    if (pending !== flow) return;
    if (err.status === 0 || err.status >= 500) return schedulePoll(flow); // transient: keep polling until the code expires
    if (msg.includes("authorization_pending")) return schedulePoll(flow);
    if (msg.includes("slow_down")) {
      flow.interval += 5;
      return schedulePoll(flow);
    }
    if (msg.includes("access_denied") || msg.includes("denied")) return failFlow(flow, "The Twitch login was denied.");
    if (msg.includes("expired") || msg.includes("invalid device code")) return failFlow(flow, "The Twitch login code expired. Start the login again.");
    return failFlow(flow, `Twitch login failed: ${err.message}`);
  }
  if (pending !== flow) return;
  try {
    await finishLogin(flow, tok);
  } catch (err) {
    failFlow(flow, `Twitch login failed: ${err.message}`);
  }
}

async function finishLogin(flow, tok) {
  const v = await requestJson(`${ID}/validate`, { headers: { Authorization: `OAuth ${tok.access_token}` } });
  let name = v.login;
  try {
    const u = await requestJson(`${HELIX}/users?id=${encodeURIComponent(v.user_id)}`, {
      headers: { Authorization: `Bearer ${tok.access_token}`, "Client-Id": flow.clientId },
    });
    name = u?.data?.[0]?.display_name || name;
  } catch {
    /* the login name is a fine display name */
  }
  if (pending !== flow) return;
  clearTimeout(flow.timer);
  pending = null;
  lastError = null;
  record = {
    accessToken: tok.access_token,
    refreshToken: tok.refresh_token,
    expiresAt: Date.now() + (Number(tok.expires_in) || v.expires_in || 3600) * 1000,
    scopes: Array.isArray(v.scopes) ? v.scopes : Array.isArray(tok.scope) ? tok.scope : [],
    clientId: flow.clientId,
    user: { id: String(v.user_id), login: v.login, name },
  };
  persist();
  scheduleRefresh();
  startValidateLoop();
  changed();
}

export function cancel() {
  if (!pending) return;
  clearTimeout(pending.timer);
  pending = null;
  changed();
}

export async function logout() {
  cancel();
  const old = record;
  record = null;
  lastError = null;
  stopTimers();
  persist();
  changed();
  if (old) {
    // Best effort: tell Twitch the token is dead so it cannot be used if the file leaks.
    try {
      await requestJson(`${ID}/revoke`, { method: "POST", form: { client_id: old.clientId, token: old.accessToken }, timeoutMs: 8000 });
    } catch {
      /* already gone, or offline */
    }
  }
}

// ---------- refresh / validate ----------

/** Resolves true when the token was refreshed, false when it could not be (the login is cleared if Twitch refused it). */
export function refreshNow() {
  if (refreshing) return refreshing;
  refreshing = (async () => {
    const rec = record;
    if (!rec?.refreshToken) return false;
    try {
      // Public client: client_id only, no secret.
      const tok = await requestJson(`${ID}/token`, {
        method: "POST",
        form: { client_id: rec.clientId, grant_type: "refresh_token", refresh_token: rec.refreshToken },
      });
      if (record !== rec) return false; // logged out / replaced meanwhile
      record = {
        ...rec,
        accessToken: tok.access_token,
        refreshToken: tok.refresh_token || rec.refreshToken,
        expiresAt: Date.now() + (Number(tok.expires_in) || 3600) * 1000,
        scopes: Array.isArray(tok.scope) && tok.scope.length ? tok.scope : rec.scopes,
      };
      persist();
      scheduleRefresh();
      return true;
    } catch (err) {
      if (err instanceof HttpError && (err.status === 400 || err.status === 401)) expire();
      else log("token refresh failed", err.message);
      return false;
    }
  })().finally(() => {
    refreshing = null;
  });
  return refreshing;
}

function expire() {
  if (!record) return;
  record = null;
  lastError = EXPIRED_MESSAGE;
  stopTimers();
  persist();
  changed();
}

function scheduleRefresh() {
  clearTimeout(refreshTimer);
  if (!record) return;
  const wait = Math.max(10_000, record.expiresAt - Date.now() - REFRESH_MARGIN_MS);
  refreshTimer = setTimeout(() => void refreshNow(), Math.min(wait, 2 ** 31 - 1));
  refreshTimer.unref?.();
}

function startValidateLoop() {
  clearInterval(validateTimer);
  validateTimer = setInterval(() => void validate(), VALIDATE_EVERY_MS);
  validateTimer.unref?.();
}

function stopTimers() {
  clearTimeout(refreshTimer);
  clearInterval(validateTimer);
  refreshTimer = null;
  validateTimer = null;
}

/** GET /oauth2/validate: required hourly by Twitch. A 401 triggers a refresh; a failed refresh logs the user out. */
export async function validate() {
  const rec = record;
  if (!rec) return;
  try {
    const v = await requestJson(`${ID}/validate`, { headers: { Authorization: `OAuth ${rec.accessToken}` } });
    if (record !== rec) return;
    const scopes = Array.isArray(v.scopes) ? v.scopes : rec.scopes;
    const same = scopes.length === rec.scopes.length && scopes.every((s) => rec.scopes.includes(s));
    if (!same) {
      record = { ...rec, scopes };
      persist();
      changed();
    }
  } catch (err) {
    if (err instanceof HttpError && err.status === 401) await refreshNow();
    else log("validate failed", err.message);
  }
}
