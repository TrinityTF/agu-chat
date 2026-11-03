// The Twitch half of the api router: the paths and bodies of render/server.js's chat-page
// endpoints, answered with the logged-in user's own token and the source's channel.
// Handlers get ({source, method, query, body, ctx}) and return {status, body}.
//
// Errors follow the site: 403 "relogin" when the token lacks the scope, 502 "Twitch error: ..."
// when Twitch refuses; the error text sits in {error}. Logged out is 401 "Log in to Twitch".
// Badges, emotes and rules never fail for a logged-out user: they answer empty so chat still renders.
import { HttpError, requestJson } from "./http.js";
import { helix, helixAll, loginRequired, userByLogin } from "./helix.js";
import * as auth from "./auth.js";
import { access as getAccess } from "./access.js";
import { normalizeChannel } from "./channel.js";
import { sources } from "./source.js";
import { suggestChatters } from "./vendor/chatters.js";

const ANNOUNCE_COLORS = new Set(["blue", "green", "orange", "purple", "primary"]);
const CHAT_SETTINGS_FIELDS = ["emote_mode", "follower_mode", "follower_mode_duration", "slow_mode", "slow_mode_wait_time", "subscriber_mode", "unique_chat_mode"];

const SETTINGS_CACHE_MS = 30_000;
const BADGES_CACHE_MS = 60 * 60_000;
const EMOTES_CACHE_MS = 30 * 60_000;

// Which scope /api/mod/action needs per action.
const MOD_ACTION_SCOPES = {
  ban: "moderator:manage:banned_users",
  timeout: "moderator:manage:banned_users",
  unban: "moderator:manage:banned_users",
  warn: "moderator:manage:warnings",
  delete: "moderator:manage:chat_messages",
  pin: "moderator:manage:chat_messages",
  unpin: "moderator:manage:chat_messages",
  pinduration: "moderator:manage:chat_messages",
  clear: "moderator:manage:chat_messages",
};

const settingsCache = new Map(); // channelId -> {data, at}
const badgesCache = new Map(); // login -> {data, at}
const emotesCache = new Map(); // `${userId}:${channelId}` -> {data, at}
let globalBadges = null; // {data, at}

// ---------- plumbing ----------

function local(status, message) {
  const e = new HttpError(status, message);
  e.local = true;
  return e;
}

function toResult(err) {
  if (err instanceof HttpError) {
    if (err.local) return { status: err.status, body: { error: err.message } };
    if (err.status === 401) return { status: 401, body: { error: "Log in to Twitch" } }; // refused even after a refresh
    return { status: 502, body: { error: `Twitch error: ${err.message}` } };
  }
  if (err?.userMessage) return { status: 400, body: { error: err.userMessage } };
  throw err; // a bug: the router logs it and answers 500
}

const route = (fn) => async (req) => {
  try {
    return await fn(req);
  } catch (err) {
    return toResult(err);
  }
};

const ok = (body = { ok: true }) => ({ status: 200, body });
const bodyOf = (b) => (b && typeof b === "object" ? b : {});

function loginOf(source) {
  try {
    return normalizeChannel(source?.channel);
  } catch (err) {
    err.userMessage = err.message;
    throw err;
  }
}

/** Logged in (401 otherwise), holding `scope` (403 "relogin" otherwise), and the source's channel id resolved. */
async function need(source, scope) {
  const login = loginOf(source);
  const rec = auth.current();
  if (!rec) throw loginRequired();
  if (scope && !rec.scopes.includes(scope)) throw local(403, "relogin");
  const channelId = await channelIdOf(login);
  return { login, rec, me: rec.user, channelId };
}

async function channelIdOf(login) {
  const live = sources.get(login)?.channelId;
  if (live) return live;
  const u = await userByLogin(login);
  if (!u) throw local(404, "channel not found");
  return u.id;
}

async function resolveUser(login) {
  const u = await userByLogin(String(login || "").trim().toLowerCase());
  if (!u) throw local(404, "user not found");
  return u;
}

function broadcastTo(login, frame) {
  sources.get(login)?.emit(frame);
}

function pickChatSettings(raw) {
  const d = raw || {};
  const out = {};
  for (const k of CHAT_SETTINGS_FIELDS) out[k] = k in d ? d[k] : null;
  return out;
}

function mapEmote(e, template) {
  if (!e?.id) return null;
  const images = e.images || {};
  let url = images.url_2x || images.url_1x || images.url_4x || "";
  if (!url && template) {
    const pick = (list, prefer, dflt) => (Array.isArray(list) && list.includes(prefer) ? prefer : (Array.isArray(list) && list[0]) || dflt);
    const format = pick(e.format, "animated", "static");
    const theme = pick(e.theme_mode, "dark", "dark");
    const scale = Array.isArray(e.scale) && e.scale.length ? (e.scale.includes("2.0") ? "2.0" : e.scale[e.scale.length - 1]) : "2.0";
    url = String(template).replace("{{id}}", e.id).replace("{{format}}", format).replace("{{theme_mode}}", theme).replace("{{scale}}", scale);
  }
  return { id: e.id, name: e.name || "", url };
}

// ---------- routes ----------

async function send({ source, body }) {
  const { me, channelId } = await need(source, "user:write:chat");
  const b = bodyOf(body);
  const text = String(b.text || "").slice(0, 500);
  if (!text) throw local(400, "empty message");
  const replyTo = b.replyTo ? String(b.replyTo).slice(0, 100) : undefined;
  const req = { broadcaster_id: String(channelId), sender_id: me.id, message: text };
  if (replyTo) req.reply_parent_message_id = replyTo;
  const r = await helix("/chat/messages", { method: "POST", body: req });
  const d = r?.data?.[0];
  if (d && d.is_sent === false) throw new HttpError(502, "drop: " + (d.drop_reason ? d.drop_reason.message : "unknown"));
  return ok();
}

async function modAction({ source, body }) {
  const b = bodyOf(body);
  const action = String(b.action || "");
  const scope = MOD_ACTION_SCOPES[action];
  if (!scope) throw local(400, "unknown action");
  const { login, me, channelId } = await need(source, scope);
  const q = { broadcaster_id: channelId, moderator_id: me.id };

  // ban/timeout/unban/warn accept userLogin as an alternative to userId; an explicit userId wins.
  let userId = b.userId;
  if (["ban", "timeout", "unban", "warn"].includes(action) && !userId) {
    const l = String(b.userLogin || "").trim().toLowerCase();
    if (!l) throw local(400, "missing userId or userLogin");
    userId = (await resolveUser(l)).id;
  }

  if (action === "ban" || action === "timeout") {
    const data = { user_id: String(userId) };
    if (action === "timeout") {
      const raw = Number(b.duration);
      data.duration = Math.max(1, Math.min(1_209_600, Number.isFinite(raw) ? Math.round(raw) : 1)); // Twitch's two-week cap
    }
    if (b.reason) data.reason = String(b.reason);
    await helix("/moderation/bans", { method: "POST", query: q, body: { data } });
  } else if (action === "unban") {
    await helix("/moderation/bans", { method: "DELETE", query: { ...q, user_id: userId } });
  } else if (action === "warn") {
    const reason = String(b.reason || "").slice(0, 500) || "Warned by moderator";
    await helix("/moderation/warnings", { method: "POST", query: q, body: { data: { user_id: String(userId), reason } } });
  } else if (action === "delete") {
    await helix("/moderation/chat", { method: "DELETE", query: { ...q, message_id: b.messageId } });
  } else if (action === "clear") {
    await helix("/moderation/chat", { method: "DELETE", query: q }); // no message_id wipes every deletable message
  } else if (action === "pin" || action === "pinduration" || action === "unpin") {
    // Twitch takes 30s to 30min; no duration = until unpinned or the stream ends.
    const d = b.durationSeconds;
    const duration = d == null || d === "" || !Number.isFinite(Number(d)) ? undefined : Math.max(30, Math.min(1800, Math.round(Number(d))));
    const pinQ = { ...q, message_id: b.messageId || undefined };
    const src = sources.get(login);
    if (action === "pin") {
      await helix("/chat/pins", { method: "PUT", query: { ...pinQ, duration_seconds: duration } });
      // Show it now, then let Twitch's own answer (who pinned it, when it ends) replace the guess.
      src?.pinLocal(b.messageId, { id: me.id, login: me.login, name: me.name }, b.durationSeconds);
      void src?.syncPin();
    } else if (action === "pinduration") {
      await helix("/chat/pins", { method: "PATCH", query: { ...pinQ, duration_seconds: duration } });
      await src?.syncPin();
    } else {
      await helix("/chat/pins", { method: "DELETE", query: pinQ });
      src?.unpin();
    }
  }
  return ok();
}

// Capability probe: which mod UI to offer. The site answered from scopes alone; here the user can open
// ANY channel, so mod capabilities also need to be the broadcaster or a moderator there.
async function caps({ source }) {
  const login = loginOf(source);
  const rec = auth.current();
  const has = (s) => !!rec && rec.scopes.includes(s);
  let canMod = true;
  let isBroadcaster = false;
  let isMod = rec ? null : false;
  if (rec) {
    try {
      const acc = await getAccess(await channelIdOf(login));
      isBroadcaster = acc.isBroadcaster;
      isMod = acc.isMod;
      canMod = acc.isMod !== false; // unknown (null) keeps the buttons: Twitch refuses what is not allowed
    } catch {
      /* channel unresolvable: scopes alone decide */
    }
  }
  const m = (s) => canMod && has(s);
  return ok({
    pin: m("moderator:manage:chat_messages"),
    ban: m("moderator:manage:banned_users"),
    warn: m("moderator:manage:warnings"),
    delete: m("moderator:manage:chat_messages"),
    send: has("user:write:chat"),
    modes: m("moderator:manage:chat_settings"),
    announce: m("moderator:manage:announcements"),
    shoutout: m("moderator:manage:shoutouts"),
    shield: m("moderator:manage:shield_mode"),
    terms: m("moderator:manage:blocked_terms"),
    raid: isBroadcaster && has("channel:manage:raids"),
    automod: m("moderator:manage:automod"),
    // Not in the site's answer: lets the page say why buttons are missing.
    loggedIn: !!rec,
    isBroadcaster,
    isMod,
  });
}

async function modUser({ source, query }) {
  const { channelId, rec } = await need(source);
  const id = query.get("id");
  if (!id) throw local(400, "missing id");
  const page = await helix("/users", { query: { id } });
  const u = page?.data?.[0];
  const user = u ? { id: u.id, login: u.login, displayName: u.display_name, avatar: u.profile_image_url || "", createdAt: u.created_at } : null;

  // followedAt / ban only answer for a broadcaster or moderator, and need their scopes. A refusal is
  // reported as followError / banError so the card can tell "not following" apart from "could not check".
  const acc = await getAccess(channelId);
  const allowed = acc.isBroadcaster || acc.isMod !== false;
  let followedAt = null;
  let followError;
  if (!allowed) followError = "Only moderators can see follow status";
  else if (!rec.scopes.includes("moderator:read:followers")) followError = "missing moderator:read:followers scope";
  else {
    try {
      const f = await helix("/channels/followers", { query: { broadcaster_id: channelId, user_id: id } });
      followedAt = f?.data?.[0]?.followed_at || null;
    } catch (err) {
      followError = err.message;
    }
  }
  let ban = null;
  let banError;
  if (!allowed) banError = "Only moderators can see ban status";
  else if (!rec.scopes.includes("moderator:manage:banned_users")) banError = "missing moderator:manage:banned_users scope";
  else {
    try {
      const b = await helix("/moderation/banned", { query: { broadcaster_id: channelId, user_id: id } });
      const row = b?.data?.[0];
      ban = row ? { permanent: !row.expires_at, expiresAt: row.expires_at || null, reason: row.reason || "" } : null;
    } catch (err) {
      ban = undefined;
      banError = err.message;
    }
  }
  return ok({ user, followedAt, followError, ban, banError });
}

async function automod({ source, body }) {
  const b = bodyOf(body);
  const msgId = String(b.msgId || "");
  if (!msgId) throw local(400, "missing msgId");
  const action = String(b.action || "");
  if (action !== "allow" && action !== "deny") throw local(400, "unknown action");
  const { me } = await need(source, "moderator:manage:automod");
  try {
    // user_id must be the token owner's (the acting moderator); the resolving automod.message.update event reaches every client.
    await helix("/moderation/automod/message", { method: "POST", body: { user_id: me.id, msg_id: msgId, action: action === "allow" ? "ALLOW" : "DENY" } });
  } catch (err) {
    if (err instanceof HttpError && !err.local) {
      if (err.status === 404) throw local(404, "message no longer held");
      if (err.status === 403) throw local(403, "not a moderator");
    }
    throw err;
  }
  return ok();
}

async function getSettings({ source }) {
  const { channelId } = await need(source);
  const hit = settingsCache.get(channelId);
  if (hit && Date.now() - hit.at < SETTINGS_CACHE_MS) return ok(hit.data);
  const page = await helix("/chat/settings", { query: { broadcaster_id: channelId } });
  const data = pickChatSettings(page?.data?.[0]);
  settingsCache.set(channelId, { data, at: Date.now() });
  return ok(data);
}

async function postSettings({ source, body }) {
  const { login, me, channelId } = await need(source, "moderator:manage:chat_settings");
  const b = bodyOf(body);
  const patch = {};
  for (const k of CHAT_SETTINGS_FIELDS) if (k in b) patch[k] = b[k];
  const page = await helix("/chat/settings", { method: "PATCH", query: { broadcaster_id: channelId, moderator_id: me.id }, body: patch });
  const settings = pickChatSettings(page?.data?.[0]);
  settingsCache.set(channelId, { data: settings, at: Date.now() });
  broadcastTo(login, { type: "modes", settings }); // the Modes panel of every open view of this channel follows
  return ok(settings);
}

async function shield({ source, method, body }) {
  const { me, channelId } = await need(source, "moderator:manage:shield_mode");
  const q = { broadcaster_id: channelId, moderator_id: me.id };
  if (method === "GET") {
    const page = await helix("/moderation/shield_mode", { query: q });
    return ok(page?.data?.[0] || { is_active: false });
  }
  const on = !!bodyOf(body).is_active;
  const page = await helix("/moderation/shield_mode", { method: "PUT", query: q, body: { is_active: on } });
  return ok(page?.data?.[0] || { is_active: on });
}

async function announce({ source, body }) {
  const { me, channelId } = await need(source, "moderator:manage:announcements");
  const b = bodyOf(body);
  const text = String(b.text || "").slice(0, 500);
  if (!text) throw local(400, "empty message");
  const color = ANNOUNCE_COLORS.has(String(b.color || "")) ? b.color : "primary"; // an odd colour is cosmetic, never a 400
  await helix("/chat/announcements", { method: "POST", query: { broadcaster_id: channelId, moderator_id: me.id }, body: { message: text, color } });
  return ok();
}

async function shoutout({ source, body }) {
  const { me, channelId } = await need(source, "moderator:manage:shoutouts");
  const login = String(bodyOf(body).login || "").trim().toLowerCase();
  if (!login) throw local(400, "missing login");
  const target = await resolveUser(login);
  await helix("/chat/shoutouts", { method: "POST", query: { from_broadcaster_id: channelId, to_broadcaster_id: target.id, moderator_id: me.id } });
  return ok();
}

// Twitch lets only the BROADCASTER raid, so this needs the user to be logged in as the channel's owner.
async function raid({ source, method, body }) {
  const { login, me, channelId } = await need(source, "channel:manage:raids");
  if (me.id !== String(channelId)) throw local(403, `Only ${login} can raid from their channel. Log in as ${login} to use raids.`);
  if (method === "POST") {
    const target = String(bodyOf(body).login || "").trim().toLowerCase();
    if (!target) throw local(400, "missing login");
    const to = await resolveUser(target);
    const page = await helix("/raids", { method: "POST", query: { from_broadcaster_id: channelId, to_broadcaster_id: to.id } });
    return ok(page?.data?.[0] || { ok: true });
  }
  await helix("/raids", { method: "DELETE", query: { broadcaster_id: channelId } });
  return ok();
}

async function terms({ source, method, query, body }) {
  const { me, channelId } = await need(source, "moderator:manage:blocked_terms");
  const q = { broadcaster_id: channelId, moderator_id: me.id };
  if (method === "GET") {
    const page = await helix("/moderation/blocked_terms", { query: { ...q, first: 100 } });
    return ok({ terms: (page?.data || []).map((t) => ({ id: t.id, text: t.text })) });
  }
  if (method === "POST") {
    const text = String(bodyOf(body).text || "").trim().slice(0, 500);
    if (!text) throw local(400, "empty term");
    const page = await helix("/moderation/blocked_terms", { method: "POST", query: q, body: { text } });
    const t = page?.data?.[0];
    return ok({ term: t ? { id: t.id, text: t.text } : null });
  }
  const id = query.get("id");
  if (!id) throw local(400, "missing id");
  await helix("/moderation/blocked_terms", { method: "DELETE", query: { ...q, id } });
  return ok();
}

// The site read these from Twitch's web GraphQL with twitch.tv's own client id; this app never borrows
// another app's client id, and Helix has no endpoint for them, so the warn panel just has no rules to cite.
async function rules() {
  return ok({ rules: [] });
}

// Helix answers badges only with a token. Logged out (or when Helix refuses), the public ivr.fi mirror of the
// same Helix data fills in, so mod/sub/VIP badges show for anonymous readers too.
const IVR = "https://api.ivr.fi/v2/twitch/badges";

async function helixBadges(login) {
  const channelId = await channelIdOf(login).catch(() => null);
  if (!globalBadges || Date.now() - globalBadges.at > BADGES_CACHE_MS) {
    globalBadges = { data: (await helix("/chat/badges/global"))?.data || [], at: Date.now() };
  }
  const channel = channelId ? (await helix("/chat/badges", { query: { broadcaster_id: channelId } }))?.data || [] : [];
  return { global: globalBadges.data, channel };
}

async function ivrBadges(login) {
  const [global, channel] = await Promise.all([
    requestJson(`${IVR}/global`),
    requestJson(`${IVR}/channel?login=${encodeURIComponent(login)}`).catch(() => []),
  ]);
  return { global: Array.isArray(global) ? global : [], channel: Array.isArray(channel) ? channel : [] };
}

async function badges({ source }) {
  const login = loginOf(source);
  const hit = badgesCache.get(login);
  if (hit && Date.now() - hit.at < BADGES_CACHE_MS) return ok(hit.data);
  let data = null;
  if (auth.current()) data = await helixBadges(login).catch(() => null);
  if (!data?.global.length) data = await ivrBadges(login).catch(() => null);
  if (!data?.global.length) return ok({ global: [], channel: [] }); // not cached: the next pane retries
  badgesCache.set(login, { data, at: Date.now() });
  return ok(data);
}

// The emote picker: what THIS user can send here. {channel, global} as the site's picker expects, plus
// `other` for emotes from channels the user is subscribed to.
async function twitchEmotes({ source }) {
  const login = loginOf(source);
  const rec = auth.current();
  if (!rec) return ok({ channel: [], global: [] });
  const channelId = await channelIdOf(login).catch(() => null);
  const key = `${rec.user.id}:${channelId || ""}`;
  const hit = emotesCache.get(key);
  if (hit && Date.now() - hit.at < EMOTES_CACHE_MS) return ok(hit.data);

  let data = { channel: [], global: [] };
  try {
    if (rec.scopes.includes("user:read:emotes")) {
      const { rows, template } = await helixAll("/chat/emotes/user", { query: { user_id: rec.user.id, broadcaster_id: channelId || undefined }, maxPages: 40 });
      const seen = new Set();
      const channel = [];
      const global = [];
      const other = [];
      for (const e of rows) {
        const m = mapEmote(e, template);
        if (!m || seen.has(m.id)) continue;
        seen.add(m.id);
        if (channelId && String(e.owner_id) === String(channelId)) channel.push(m);
        else if (e.owner_id === "twitch" || e.emote_type === "globals" || e.emote_type === "smilies") global.push(m);
        else other.push(m);
      }
      data = { channel, global, other };
    } else {
      // A token from before user:read:emotes: the public lists still work with any token.
      const [g, c] = await Promise.allSettled([
        helix("/chat/emotes/global"),
        channelId ? helix("/chat/emotes", { query: { broadcaster_id: channelId } }) : Promise.resolve(null),
      ]);
      const map = (r) => (r.status === "fulfilled" && r.value ? (r.value.data || []).map((e) => mapEmote(e, r.value.template)).filter(Boolean) : []);
      data = { channel: map(c), global: map(g) };
    }
  } catch (err) {
    if (err instanceof HttpError && err.local) throw err;
    return ok(data); // best effort: an empty picker section beats an error
  }
  emotesCache.set(key, { data, at: Date.now() });
  return ok(data);
}

// Name suggestions (for the hidden-chatters list), from what this chat said recently.
async function chatters({ source, query }) {
  const login = loginOf(source);
  const q = (query.get("q") || "").slice(0, 60);
  const messages = (sources.get(login)?.backlog || []).filter((e) => !e.notice);
  return ok({ results: suggestChatters(q, [{ platform: "twitch", messages }], 8) });
}

export function clearCaches() {
  settingsCache.clear();
  badgesCache.clear();
  emotesCache.clear();
  globalBadges = null;
}

export const routes = {
  "POST /api/chat/send": route(send),
  "POST /api/mod/action": route(modAction),
  "GET /api/mod/caps": route(caps),
  "GET /api/mod/user": route(modUser),
  "POST /api/mod/automod": route(automod),
  "GET /api/chat/settings": route(getSettings),
  "POST /api/chat/settings": route(postSettings),
  "GET /api/chat/shield": route(shield),
  "PUT /api/chat/shield": route(shield),
  "POST /api/chat/announce": route(announce),
  "POST /api/chat/shoutout": route(shoutout),
  "POST /api/chat/raid": route(raid),
  "DELETE /api/chat/raid": route(raid),
  "GET /api/chat/terms": route(terms),
  "POST /api/chat/terms": route(terms),
  "DELETE /api/chat/terms": route(terms),
  "GET /api/chat/rules": route(rules),
  "GET /api/chat/badges": route(badges),
  "GET /api/chat/twitch-emotes": route(twitchEmotes),
  "GET /api/chatters": route(chatters),
};
