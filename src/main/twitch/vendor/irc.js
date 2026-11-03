// Vendored from modtools.agu.lol render/lib/irc.js (render commit 7704aec). Changes: parseIrcLine also reads a
// last parameter sent without the ':' prefix (recent-messages sends one-word messages that way).
// copied from src/do.js — keep in sync
//
// IRC line parsing plus the PRIVMSG -> ChatMsg and USERNOTICE -> Notice
// builders, mirrored from the Durable Object's chat pipeline so this relay
// speaks the exact same wire protocol (see the "Wire protocol" section of
// the chat spec). The DO gates USERNOTICE through a USERNOTICE_LABELS map
// (only sub/resub/raid/announcement/... become dashboard alerts); this
// relay has no alert feed, so buildNotice() below forwards EVERY USERNOTICE
// that carries a system-msg tag instead — simpler and strictly more info.

// IRC /me messages arrive as the CTCP-wrapped text \x01ACTION <text>\x01 —
// strip the wrapper (the trailing \x01 is optional defensively) and flag it.
export const ACTION_RE = /^\x01?ACTION (.*?)\x01?$/;

// Parses one IRC line: `@tag=val;tag2=val2 :nick!user@host PRIVMSG #chan :text`
export function parseIrcLine(line) {
  const out = { tags: {}, command: "", channel: "", user: "", text: "" };
  let rest = line;
  if (rest.startsWith("@")) {
    const sp = rest.indexOf(" ");
    for (const kv of rest.slice(1, sp).split(";")) {
      const eq = kv.indexOf("=");
      const k = eq < 0 ? kv : kv.slice(0, eq);
      const v = eq < 0 ? "" : kv.slice(eq + 1).replace(/\\(.?)/g, (_, c) =>
        c === "s" ? " " : c === "n" ? "\n" : c === "r" ? "\r" : c === ":" ? ";" : c
      );
      out.tags[k] = v;
    }
    rest = rest.slice(sp + 1);
  }
  if (rest.startsWith(":")) {
    const sp = rest.indexOf(" ");
    out.user = rest.slice(1, sp).split("!")[0];
    rest = rest.slice(sp + 1);
  }
  const trailIdx = rest.indexOf(" :");
  let head = rest;
  if (trailIdx >= 0) {
    out.text = rest.slice(trailIdx + 2);
    head = rest.slice(0, trailIdx);
  }
  const parts = head.split(" ");
  out.command = parts[0];
  out.channel = (parts.find((p) => p.startsWith("#")) || "").slice(1);
  // IRC only needs the ':' when the last parameter has spaces: `PRIVMSG #chan word` is valid.
  if (trailIdx < 0 && parts.length >= 3) out.text = parts[parts.length - 1];
  return out;
}

// Builds a chat-page ChatMsg from a parsed PRIVMSG line (see the wire-protocol
// doc in the chat spec).
export function buildChatMsg(m, displayUser) {
  const tags = m.tags;
  const actionMatch = ACTION_RE.exec(m.text);
  return {
    platform: "twitch",
    id: tags.id || crypto.randomUUID(),
    ts: Number(tags["tmi-sent-ts"]) || Date.now(),
    user: { id: tags["user-id"] || "", login: m.user || "", name: displayUser || m.user || "", color: tags.color || "" },
    badges: tags.badges || "",
    text: actionMatch ? actionMatch[1] : m.text,
    emotes: tags.emotes || "",
    // Twitch's GIF keyboard (tier 2/3 subs, shipped 2026). The message TEXT is
    // only a fallback label — "[Happy Dance GIF by World of Warcraft]" — so a
    // client that ignores this tag renders that bracketed string instead of
    // the image, which is exactly what our chat page was doing.
    //
    // Format mirrors the `emotes` tag: comma-separated
    // `<start>-<end>|<gifID>|<gifURL>`, positions indexing the same raw text.
    // Twitch's docs are explicit that the URL must be rendered as given and
    // never modified, so it is passed straight through rather than parsed or
    // proxied here.
    //
    // PRIVMSG only. USERNOTICE's attached text is trimmed before it reaches
    // buildNotice, which already shifts every position in it — adding a
    // position-indexed tag there would be actively wrong.
    gifs: tags.gifs || "",
    action: !!actionMatch,
    firstMsg: tags["first-msg"] === "1",
    reply: tags["reply-parent-display-name"]
      ? { user: tags["reply-parent-display-name"], text: tags["reply-parent-msg-body"] || "" }
      : null,
    rewardId: tags["custom-reward-id"] || null,
    msgId: tags["msg-id"] || null,
    returning: tags["returning-chatter"] === "1",
    // A cheer: the Bits on this message. 0 for every ordinary message.
    bits: Number(tags.bits) || 0,
  };
}

// Turn an IRC PRIVMSG's `badges` tag (plus the mod/subscriber/vip flag tags)
// into the {set_id} shape render/lib/bot.js's chatterRoles() expects, so the
// same command engine works whether a message arrived over IRC or
// (historically) EventSub. Ported from src/do.js's ircBadges — keep in sync.
export function ircBadges(tags) {
  const out = [];
  const seen = new Set();
  for (const part of String((tags && tags.badges) || "").split(",")) {
    const id = part.split("/")[0];
    if (id && !seen.has(id)) {
      seen.add(id);
      out.push({ set_id: id });
    }
  }
  if (tags && tags.mod === "1" && !seen.has("moderator")) out.push({ set_id: "moderator" });
  if (tags && tags.subscriber === "1" && !seen.has("subscriber") && !seen.has("founder")) out.push({ set_id: "subscriber" });
  if (tags && tags.vip === "1" && !seen.has("vip")) out.push({ set_id: "vip" });
  return out;
}

// Builds a Notice from a parsed USERNOTICE line (sub/resub/raid/announcement/
// ...). Returns null when the line carries no system-msg (shouldn't happen
// for a real USERNOTICE, but keeps this defensive like the rest of the
// parser). See the header comment above for why this skips the DO's
// USERNOTICE_LABELS gate.
export function buildNotice(m) {
  const tags = m.tags;
  const sysMsg = (tags["system-msg"] || "").trim();
  if (!sysMsg) return null;
  return {
    id: tags.id || crypto.randomUUID(),
    ts: Date.now(),
    system: sysMsg,
    msgId: tags["msg-id"] || "",
    user: { id: tags["user-id"] || "", login: m.user || "", name: tags["display-name"] || m.user || "", color: tags.color || "" },
    text: (m.text || "").trim(),
    // CHAT v5 item 1 (notice-emotes fix): the attached user text carries its
    // own emote positions on USERNOTICE lines exactly like PRIVMSG's
    // `emotes` tag — previously dropped, so the frontend had no way to
    // render emotes inside e.g. a resub message ("...streak!peepoRun"
    // rendering as literal text). "" when the message has none.
    emotes: tags.emotes || "",
    // Shared chat wraps every notice as msg-id "sharedchatnotice" and puts
    // the real kind (sub, raid, …) here; the chat page sorts on whichever
    // is set so its show/hide toggles work on both.
    sourceMsgId: tags["source-msg-id"] || "",
    // A gift bomb is one "submysterygift" line followed by one "subgift" per
    // recipient, all carrying the same community gift id — the chat page
    // uses it to fold the per-recipient lines under the first one.
    giftId: tags["msg-param-community-gift-id"] || "",
    giftCount: Number(tags["msg-param-mass-gift-count"]) || 0,
  };
}
