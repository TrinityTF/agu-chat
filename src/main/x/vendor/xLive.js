// Vendored from modtools.agu.lol render/lib/xLive.js (render commit 7704aec). Changes: the browser User-Agent (X_UA) is replaced by the app's honest UA (../ua.js; verified that guest/activate, broadcasts/show, the profile page and live-chat all answer it); otherwise unchanged. Guest token only: no account, no bot session, no paid v2 API.
// Is this X account live, and on which broadcast — with no account at all.
//
// WHAT THIS CAN AND CANNOT DO, corrected after being wrong about it
// -----------------------------------------------------------------
// This was written believing the public profile carries the current
// broadcast. It does not, and the belief came from a single misread
// observation: a broadcast link WAS in the logged-out profile HTML, so
// "going live posts the link" looked confirmed. It was an old tweet that
// happened to contain one. Tested later against a genuinely RUNNING
// broadcast, the profile contained the previous stream's id and not the
// live one.
//
// So what actually holds, all of it measured against a live broadcast:
//
//   * Given an id, everything works anonymously — state, title, media key,
//     chat token, chat history. That whole chain is solid.
//   * Getting the id from a handle is AUTH-GATED. x.com/<handle>/live serves
//     a JavaScript shell to everyone, and once it runs, a logged-out visitor
//     is redirected to the login page without a single broadcast request
//     being made. UserByScreenName carries no live fields. Every v1.1
//     user->broadcast route answers 404, and every v2 endpoint answers
//     client-not-enrolled on a Free plan.
//
// WHAT THIS IS STILL FOR
// ----------------------
// Best effort, and genuinely useful when it fires: a streamer who tweets
// their broadcast link gets automatic detection for free, with no account and
// nothing to paste. When they do not, findLive returns not-live and the caller
// falls back to a pinned broadcast id (see xChat.js), which is the path that
// always works.
//
// It must never be the ONLY path, which is the mistake this comment is
// correcting.
//
// STATE IS THE ONLY TRUSTWORTHY SIGNAL
// ------------------------------------
// A broadcast id on a profile says only that it was linked once. Yesterday's
// stream is still linked today, and reading presence as liveness would report
// a channel live permanently. Nothing is live here until broadcasts/show says
// RUNNING.

export const WEB_BEARER =
  "AAAAAAAAAAAAAAAAAAAAANRILgAAAAAAnNwIzUejRCOuH5E6I8xnZz4puTs%3D1Zv7ttfk8LF81IUq16cHjhLTvJu4FA33AGWWjCpTnA";

import { getUserAgent } from "../ua.js";

// Guest tokens last hours, and activating one per lookup would be both slow and
// the kind of traffic pattern that gets an IP rate limited. Refreshed on
// expiry rather than on a timer, since only a rejection proves it is stale.
let cachedToken = "";
let cachedAt = 0;
const TOKEN_TTL_MS = 60 * 60 * 1000;

export async function guestToken(force = false) {
  if (!force && cachedToken && Date.now() - cachedAt < TOKEN_TTL_MS) return cachedToken;
  const r = await fetch("https://api.x.com/1.1/guest/activate.json", {
    method: "POST",
    headers: { authorization: `Bearer ${WEB_BEARER}`, "user-agent": getUserAgent() },
  });
  if (!r.ok) throw new Error(`could not get an X guest token (${r.status})`);
  const j = await r.json();
  if (!j || !j.guest_token) throw new Error("X returned no guest token");
  cachedToken = j.guest_token;
  cachedAt = Date.now();
  return cachedToken;
}

/** A handle, however it was typed — bare, @-prefixed, or a full profile URL. */
export function normalizeHandle(input) {
  const clean = String(input || "")
    .trim()
    // The protocol is optional: people paste "x.com/name" as readily as the
    // full URL, and both mean the same thing.
    .replace(/^(?:https?:\/\/)?(?:www\.)?(?:x|twitter)\.com\//i, "")
    .replace(/^@/, "")
    .split(/[/?#]/)[0];
  if (!/^[A-Za-z0-9_]{1,15}$/.test(clean)) return "";
  return clean;
}

/**
 * A broadcast id out of anything a person might paste, or "".
 * 13 characters ("1DGleVdEapOJL") or, since 2026-10-27, 14 ("2qVWZNBJRbVaJN");
 * the trailing guard keeps a 14-character id from being read as its first 13.
 */
export function parseBroadcastId(input) {
  const s = String(input || "").trim();
  const m = /(?:broadcasts?|i\/broadcasts)\/([A-Za-z0-9]{13,14})(?![A-Za-z0-9])/.exec(s) || /^([A-Za-z0-9]{13,14})$/.exec(s);
  return m ? m[1] : "";
}

/**
 * Ask X the state of each broadcast id.
 *
 * One id per request on purpose. The `ids` parameter looks like it takes a
 * comma-separated list and does not: given two, it answers with a single key
 * that is the literal comma-joined string and an empty body, which reads as
 * "both unknown" rather than as an error. Measured, and the reason this loops.
 */
export async function broadcastStates(ids) {
  const out = {};
  for (const id of ids.slice(0, 12)) {
    let token = await guestToken();
    let r = await fetch(`https://api.x.com/1.1/broadcasts/show.json?ids=${encodeURIComponent(id)}`, {
      headers: { authorization: `Bearer ${WEB_BEARER}`, "x-guest-token": token, "user-agent": getUserAgent() },
    });
    // A stale guest token shows up as a 401/403 rather than as anything that
    // says "token", so one forced refresh and one retry.
    if (r.status === 401 || r.status === 403) {
      token = await guestToken(true);
      r = await fetch(`https://api.x.com/1.1/broadcasts/show.json?ids=${encodeURIComponent(id)}`, {
        headers: { authorization: `Bearer ${WEB_BEARER}`, "x-guest-token": token, "user-agent": getUserAgent() },
      });
    }
    if (!r.ok) continue;
    const j = await r.json().catch(() => null);
    const b = j && j.broadcasts && j.broadcasts[id];
    if (!b || !Object.keys(b).length) continue;
    out[id] = {
      state: b.state || "?",
      title: String(b.status || "").slice(0, 140),
      startedAt: Number(b.created_at_ms) || 0,
      updatedAt: Number(b.updated_at_ms) || 0,
      displayName: b.user_display_name || "",
      // The Periscope record's `username` is whatever the handle was when the
      // account first streamed and is not kept in step — it read "_Esfand" for
      // an account that is now @EsfandTV, and @_Esfand today is somebody else
      // entirely. Carried for display only; never matched against a handle.
      legacyUsername: b.username || "",
      mediaKey: b.media_key || "",
    };
  }
  return out;
}

/** Every broadcast id linked from an account's public profile page. */
export async function broadcastIdsOnProfile(handle) {
  const clean = normalizeHandle(handle);
  if (!clean) throw new Error("that is not an X handle");

  const r = await fetch(`https://x.com/${clean}`, {
    headers: { "user-agent": getUserAgent(), accept: "text/html,application/xhtml+xml", "accept-language": "en-US,en;q=0.9" },
  });
  const html = await r.text();

  // Only ids reached through a broadcast URL. An earlier version also swept up
  // every 13-character quoted string in the page, which matched base64 chunks,
  // asset hashes and tweet ids — dozens of candidates, none of them real, each
  // costing a lookup.
  const ids = [];
  for (const m of html.matchAll(/(?:x|twitter)\.com\/i\/broadcasts\/([A-Za-z0-9]{13,14})(?![A-Za-z0-9])/g)) {
    if (!ids.includes(m[1])) ids.push(m[1]);
  }
  return { ids, status: r.status, bytes: html.length, handle: clean };
}

/**
 * Is this handle live right now?
 *
 * Returns { live, broadcastId, title, handle, candidates, states } and treats
 * "not live" as an ordinary answer rather than an error — it is the answer
 * most of the time.
 */
export async function findLive(handle) {
  const found = await broadcastIdsOnProfile(handle);
  const out = {
    live: false,
    broadcastId: "",
    title: "",
    handle: found.handle,
    candidates: found.ids,
    profileStatus: found.status,
    via: "public profile + guest token",
  };
  if (!found.ids.length) return out;

  const states = await broadcastStates(found.ids);
  out.states = Object.fromEntries(Object.entries(states).map(([id, s]) => [id, s.state]));

  // Newest first, so that a profile linking several streams settles on the
  // current one rather than whichever the page happened to mention first.
  const running = Object.entries(states)
    .filter(([, s]) => s.state === "RUNNING")
    .sort((a, b) => b[1].startedAt - a[1].startedAt)[0];

  if (running) {
    out.live = true;
    out.broadcastId = running[0];
    out.title = running[1].title;
    out.startedAt = running[1].startedAt;
  }
  return out;
}
