// Vendored from modtools.agu.lol render/lib/ircRelay.js (render commit 7704aec), the module-level text/shape builders at the bottom of the file.
// Changes: exported followersModeText, slowModeText, toggleModeText, buildModerateEntry, buildHeldMsg, mapAutomodStatus; everything else is the verbatim code.

// ROOMSTATE mode-change text builders (CHAT v5 item 4) — module-level
// (stateless) since _handleRoomState needs no instance context beyond the
// diffed tag values themselves.
export function followersModeText(val) {
  const n = Number(val);
  if (!Number.isFinite(n) || n < 0) return "Followers-only chat disabled";
  if (n === 0) return "Followers-only chat enabled (any duration)";
  return "Followers-only chat enabled (" + n + " min)";
}

export function slowModeText(val) {
  const n = Number(val);
  if (!Number.isFinite(n) || n <= 0) return "Slow mode off";
  return "Slow mode set to " + n + "s";
}

export function toggleModeText(label, on) {
  return label + " " + (on ? "enabled" : "disabled");
}

// A Helix chat message ({text, fragments}) -> its text plus an IRC-style
// emotes tag ("id:start-end/..."), counted in code points like IRC's.
export function textFromFragments(message) {
  const frags = message && Array.isArray(message.fragments) ? message.fragments : null;
  if (!frags) return { text: (message && typeof message.text === "string" && message.text) || "", emotes: "" };
  let text = "";
  let pos = 0;
  const byId = new Map();
  for (const f of frags) {
    const t = (f && typeof f.text === "string" && f.text) || "";
    const len = Array.from(t).length;
    if (f && f.type === "emote" && f.emote && f.emote.id && len) {
      if (!byId.has(f.emote.id)) byId.set(f.emote.id, []);
      byId.get(f.emote.id).push(`${pos}-${pos + len - 1}`);
    }
    text += t;
    pos += len;
  }
  const emotes = [...byId].map(([id, spans]) => `${id}:${spans.join(",")}`).join("/");
  return { text, emotes };
}

// ---------- channel.moderate v2 -> named feed text (CHAT v5b) ----------
// Module-level (stateless) like the ROOMSTATE text builders above.

function targetName(sub) {
  if (!sub) return "someone";
  return sub.user_name || sub.user_login || "someone";
}

// Humanizes a whole-second duration the way the spec's examples show
// (90 -> "1m 30s", 600 -> "10m", 3600 -> "1h", 86400 -> "1d", 604800 -> "1w"):
// largest unit that fits, plus a remainder in the NEXT smaller unit only
// (never more than two units).
function humanizeDuration(totalSeconds) {
  const s = Math.max(0, Math.round(Number(totalSeconds) || 0));
  if (s <= 0) return "0s";
  const units = [
    { label: "w", secs: 604800 },
    { label: "d", secs: 86400 },
    { label: "h", secs: 3600 },
    { label: "m", secs: 60 },
    { label: "s", secs: 1 },
  ];
  for (let i = 0; i < units.length; i++) {
    const u = units[i];
    if (s < u.secs) continue;
    const count = Math.floor(s / u.secs);
    const rem = s - count * u.secs;
    const next = units[i + 1];
    if (next && rem >= next.secs) return `${count}${u.label} ${Math.floor(rem / next.secs)}${next.label}`;
    return `${count}${u.label}`;
  }
  return s + "s";
}

// timeout's duration comes from `expires_at` (RFC3339), not a raw seconds
// field — this is "now" relative to when the moderate event arrived.
function secondsUntil(expiresAtIso) {
  const t = Date.parse(expiresAtIso);
  if (!Number.isFinite(t)) return 0;
  return Math.max(0, Math.round((t - Date.now()) / 1000));
}

// shared_chat_* actions mirror their plain counterparts field-for-field,
// just keyed under the shared_chat_* name in the event — see the chat spec's
// "channel.moderate v2 event" section.
const SHARED_CHAT_ACTIONS = new Set(["shared_chat_ban", "shared_chat_timeout", "shared_chat_unban", "shared_chat_untimeout", "shared_chat_delete"]);

export function buildModerateEntry(action, actor, event) {
  const isShared = SHARED_CHAT_ACTIONS.has(action);
  const baseAction = isShared ? action.slice("shared_chat_".length) : action;
  const sub = event[action] || null; // sub-object key always matches the action name verbatim
  const base = buildBaseModerateText(baseAction, actor, event, sub);
  if (!base) return null;
  return isShared ? { kind: base.kind, text: base.text + " (shared chat)" } : base;
}

function buildBaseModerateText(action, actor, event, sub) {
  switch (action) {
    case "ban":
      return { kind: "ban", text: `${actor} banned ${targetName(sub)}` + (sub && sub.reason ? `: ${sub.reason}` : "") };
    case "timeout": {
      const dur = sub && sub.expires_at ? humanizeDuration(secondsUntil(sub.expires_at)) : "";
      return {
        kind: "timeout",
        text: `${actor} timed out ${targetName(sub)}` + (dur ? ` (${dur})` : "") + (sub && sub.reason ? `: ${sub.reason}` : ""),
      };
    }
    case "unban":
      return { kind: "ban", text: `${actor} unbanned ${targetName(sub)}` };
    case "untimeout":
      return { kind: "timeout", text: `${actor} removed the timeout on ${targetName(sub)}` };
    case "clear":
      return { kind: "clear", text: `${actor} cleared chat` };
    case "delete":
      return { kind: "delete", text: `${actor} deleted a message from ${targetName(sub)}` };
    case "warn": {
      const parts = [];
      if (sub && sub.reason) parts.push(sub.reason);
      if (sub && Array.isArray(sub.chat_rules_cited) && sub.chat_rules_cited.length) parts.push(sub.chat_rules_cited.join(", "));
      return { kind: "warn", text: `${actor} warned ${targetName(sub)}` + (parts.length ? `: ${parts.join("; ")}` : "") };
    }
    case "raid":
      return { kind: "mode", text: `${actor} started a raid to ${targetName(sub)}` };
    case "unraid":
      return { kind: "mode", text: `${actor} canceled the raid to ${targetName(sub)}` };
    case "slow":
      return { kind: "mode", text: `${actor} set slow mode` + (sub && sub.wait_time_seconds != null ? ` (${humanizeDuration(sub.wait_time_seconds)})` : "") };
    case "slowoff":
      return { kind: "mode", text: `${actor} turned off slow mode` };
    case "followers": {
      const mins = sub ? Number(sub.follow_duration_minutes) : 0;
      const durText = mins > 0 ? `(${humanizeDuration(mins * 60)})` : "(any follower)";
      return { kind: "mode", text: `${actor} set followers-only chat ${durText}` };
    }
    case "followersoff":
      return { kind: "mode", text: `${actor} turned off followers-only chat` };
    case "emoteonly":
      return { kind: "mode", text: `${actor} turned on emote-only chat` };
    case "emoteonlyoff":
      return { kind: "mode", text: `${actor} turned off emote-only chat` };
    case "uniquechat":
      return { kind: "mode", text: `${actor} turned on unique chat` };
    case "uniquechatoff":
      return { kind: "mode", text: `${actor} turned off unique chat` };
    case "subscribers":
      return { kind: "mode", text: `${actor} turned on subscribers-only chat` };
    case "subscribersoff":
      return { kind: "mode", text: `${actor} turned off subscribers-only chat` };
    case "mod":
      return { kind: "mode", text: `${actor} modded ${targetName(sub)}` };
    case "unmod":
      return { kind: "mode", text: `${actor} unmodded ${targetName(sub)}` };
    case "vip":
      return { kind: "mode", text: `${actor} added ${targetName(sub)} as a VIP` };
    case "unvip":
      return { kind: "mode", text: `${actor} removed ${targetName(sub)} as a VIP` };
    case "add_blocked_term":
    case "remove_blocked_term":
    case "add_permitted_term":
    case "remove_permitted_term": {
      const terms = event.automod_terms;
      const list = terms && Array.isArray(terms.terms) ? terms.terms : [];
      const verb = action.startsWith("add") ? "added" : "removed";
      const kindWord = action.includes("permitted") ? "permitted" : "blocked";
      const plural = list.length === 1 ? "" : "s";
      const quoted = list.map((t) => `"${t}"`).join(", ");
      const fromAutomod = terms && terms.from_automod ? " (from AutoMod)" : "";
      return { kind: "terms", text: `${actor} ${verb} ${kindWord} term${plural} ${quoted}${fromAutomod}` };
    }
    case "approve_unban_request":
      return { kind: "ban", text: `${actor} approved ${targetName(event.unban_request)}'s unban request` };
    case "deny_unban_request":
      return { kind: "ban", text: `${actor} denied ${targetName(event.unban_request)}'s unban request` };
    default:
      return { kind: "mode", text: `${actor} used ${action}` }; // future-proof, unknown action
  }
}

// ---------- AutoMod hold/update -> HeldMsg (CHAT v5b) ----------

export function mapAutomodStatus(status) {
  if (status === "Approved") return "approved";
  if (status === "Denied") return "denied";
  return "expired"; // "Expired" or any other/unrecognized value
}

// Shared by handleAutomodHold (status "held", resolvedBy null) and
// handleAutomodUpdate's synthesize-if-absent branch (a mod may resolve a
// message that was held before this session connected) — both automod.
// message.hold and .update events carry the same message/user/reason fields.
export function buildHeldMsg(event, status, resolvedBy) {
  const isBlockedTerm = event.reason === "blocked_term";
  return {
    id: event.message_id,
    ts: Date.parse(event.held_at) || Date.now(),
    user: { id: event.user_id || "", login: event.user_login || "", name: event.user_name || event.user_login || "" },
    text: (event.message && event.message.text) || "",
    reason: isBlockedTerm ? "blocked_term" : "automod",
    category: isBlockedTerm ? null : (event.automod && event.automod.category) || null,
    level: isBlockedTerm ? null : event.automod && typeof event.automod.level === "number" ? event.automod.level : null,
    status,
    resolvedBy,
  };
}
