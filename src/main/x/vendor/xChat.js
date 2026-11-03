// Vendored from modtools.agu.lol render/lib/xChat.js (render commit 7704aec). Changes: dropped XRelay, the profile cache and name batching (they exist for the X bot session, which this app does not have); the browser User-Agent is replaced by the app's honest UA; toXChatMsg's placeholder name now carries the last digits of the user id so chatters can be told apart (X answers 404 to a guest for the name lookup); readLiveChat/tsToMs are otherwise unchanged.
// X (Twitter) broadcast chat — the fifth feed on the Chat page.
//
// READ-ONLY: nothing official posts to broadcast chat, and this column has no
// business posting as anyone.
//
// WHERE X CHAT ACTUALLY IS (measured 2026-09-19, the hard way)
// ------------------------------------------------------------
// X moved broadcast chat off Periscope's chat servers. x.com's live chat reads
// one streaming endpoint:
//
//   GET x.com/i/api/live-chat?broadcastId=<id>&sessionId=<random>[&cursor=<ts>]
//
// It answers with newline-delimited JSON — first a backfill of recent
// messages ({isBackfill: true}, ~100), then new ones as they are sent — and
// ends every so often; x.com reconnects a second later with the last `ts` as
// the cursor, or 30 seconds later after an error. An event is
// {ts, chatType, userId, message}: `ts` is epoch nanoseconds as a string,
// chatType 1 is a chat message, userId is the X account's rest id.
//
// A GUEST TOKEN IS ENOUGH to read it. Measured with no account at all: the
// full chat, including messages that never appeared anywhere on the
// Periscope side.
//
// The Periscope path this file used before (accessChatPublic / accessChat,
// the chatman socket and its history) is a leftover: x.com users' messages
// never reach it. A socket correctly joined there — logged in as the bot, from
// a home connection, with x.com's exact frames — heard nothing while a message
// was sent, and its history held ~6 of ~100 messages. That is why the column
// looked like a quiet chat. Do not go back to it.
//
// NAMES need a login. Events carry only a user id, and the lookup x.com uses
// (GraphQL liveAtomsUserQuery) answers 404 to a guest. So names come from the
// X bot session when one is stored (see lib/xBot.js lookupLiveUsers), batched
// and cached; without one, messages still arrive, under a placeholder name.

import crypto from "node:crypto";
import { guestToken, WEB_BEARER } from "./xLive.js";
import { getUserAgent } from "../ua.js";

// Both hosts x.com serves the endpoint on; the second is tried when the first fails outright.
const LIVE_CHAT_URLS = ["https://x.com/i/api/live-chat", "https://api.x.com/live-chat"];
const CHAT = 1;

/** X's nanosecond `ts` string -> epoch milliseconds. */
export function tsToMs(ts) {
  const s = String(ts || "");
  return s.length > 6 ? Number(s.slice(0, -6)) || 0 : 0;
}

/**
 * One live-chat event -> a chat message, or null for everything else.
 *
 * Only chatType 1 with text is a message; the stream also carries moderator
 * lists, can/cannot-chat notices and mutes, none of which are people talking.
 */
/** What a message shows for someone whose name is not known (a guest cannot look names up). */
export function placeholderName(userId) {
  return "X user " + String(userId).slice(-5);
}

export function toXChatMsg(ev, profile = null) {
  if (!ev || typeof ev !== "object" || ev.chatType !== CHAT) return null;
  const text = typeof ev.message === "string" ? ev.message : "";
  if (!text.trim() || !ev.userId || !ev.ts) return null;
  const p = profile || {};
  return {
    platform: "x",
    // `ts` is what x.com itself uses as the message id; the user id makes
    // two messages in the same nanosecond still differ.
    id: `${ev.ts}:${ev.userId}`,
    ts: tsToMs(ev.ts) || Date.now(),
    user: {
      id: String(ev.userId),
      name: String(p.name || p.login || placeholderName(ev.userId)),
      login: String(p.login || ""),
      avatar: String(p.avatar || ""),
      twitterId: String(ev.userId),
    },
    text,
  };
}

/**
 * Read one connection of X's live-chat stream, calling onEvent per line, until
 * X ends it. Throws (with .status when X answered) when it cannot be read.
 */
export async function readLiveChat(broadcastId, opts = {}) {
  const fetchImpl = opts.fetch || fetch;
  const getToken = opts.guestToken || guestToken;
  const onEvent = opts.onEvent || (() => {});
  const sessionId = crypto.randomInt(0, 2 ** 32);
  const q = `?broadcastId=${encodeURIComponent(broadcastId)}&sessionId=${sessionId}${
    opts.cursor ? `&cursor=${encodeURIComponent(opts.cursor)}` : ""
  }`;

  let res = null;
  let failure = null;
  for (const base of LIVE_CHAT_URLS) {
    const token = await getToken();
    try {
      res = await fetchImpl(base + q, {
        headers: {
          authorization: `Bearer ${WEB_BEARER}`,
          "x-guest-token": token,
          "user-agent": getUserAgent(),
          "x-twitter-active-user": "yes",
          "x-twitter-client-language": "en",
        },
        signal: opts.signal,
      });
    } catch (e) {
      if (opts.signal && opts.signal.aborted) throw e;
      failure = e;
      continue;
    }
    if (res.ok) break;
    failure = Object.assign(new Error(`live-chat ${res.status}`), { status: res.status });
    // A stale guest token shows up as 401/403; the next attempt gets a fresh one.
    if (res.status === 401 || res.status === 403) await getToken(true).catch(() => {});
    res = null;
  }
  if (!res) throw failure || new Error("live-chat unreachable");
  if (!res.body) throw new Error("live-chat answered without a body");

  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of res.body) {
    buf += decoder.decode(chunk, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) {
      const t = line.trim();
      if (!t) continue;
      let ev;
      try {
        ev = JSON.parse(t);
      } catch {
        continue;
      }
      onEvent(ev);
    }
  }
}
