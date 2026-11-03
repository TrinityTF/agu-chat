// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): SLASH_COMMANDS and
// runSlashCommand (Twitch removed IRC/API text commands for third-party apps in Feb 2023 - only "/me" still
// sends as literal text; every other command maps to a real Helix-backed relay route). Changes: calls go
// through relayApi(source, ...), `/delete` and `/pin` / `/unpin` added, the Modes-panel callback is gone.
import { relayApi } from "../../bridge";
import type { SourceRef } from "../../protocol";
import type { ModCaps } from "../modCaps";
import type { Toast } from "../toast";

export type SlashCommand = { name: string; usage: string; capKey?: keyof ModCaps };

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "ban", usage: "<user> [reason]", capKey: "ban" },
  { name: "timeout", usage: "<user> <duration> [reason]", capKey: "ban" },
  { name: "unban", usage: "<user>", capKey: "ban" },
  { name: "untimeout", usage: "<user>", capKey: "ban" },
  { name: "warn", usage: "<user> <reason>", capKey: "warn" },
  { name: "clear", usage: "clear the whole chat", capKey: "delete" },
  { name: "slow", usage: "<seconds>", capKey: "modes" },
  { name: "slowoff", usage: "turn off slow mode", capKey: "modes" },
  { name: "followers", usage: "[10m|30m|1h|1d|1w|1mo]", capKey: "modes" },
  { name: "followersoff", usage: "turn off followers-only", capKey: "modes" },
  { name: "emoteonly", usage: "turn on emote-only", capKey: "modes" },
  { name: "emoteonlyoff", usage: "turn off emote-only", capKey: "modes" },
  { name: "subscribers", usage: "turn on subscribers-only", capKey: "modes" },
  { name: "subscribersoff", usage: "turn off subscribers-only", capKey: "modes" },
  { name: "uniquechat", usage: "turn on unique (r9k)", capKey: "modes" },
  { name: "uniquechatoff", usage: "turn off unique (r9k)", capKey: "modes" },
  { name: "announce", usage: "[blue|green|orange|purple] <text>", capKey: "announce" },
  { name: "shoutout", usage: "<user>", capKey: "shoutout" },
  { name: "shield", usage: "on|off", capKey: "shield" },
  { name: "raid", usage: "<user>", capKey: "raid" },
  { name: "unraid", usage: "cancel the pending raid", capKey: "raid" },
  { name: "delete", usage: "<messageId>", capKey: "delete" },
  { name: "pin", usage: "[messageId] - pins the last message by default", capKey: "pin" },
  { name: "unpin", usage: "unpin the current pin", capKey: "pin" },
  { name: "me", usage: "<action text> - passthrough, still supported", capKey: "send" },
];

// Plain seconds, or a number + s/m/h/d/w suffix, clamped to Twitch's ban/
// timeout cap of 2 weeks. Returns null for anything unparseable.
export function parseDuration(s: string): number | null {
  const m = /^(\d+)(s|m|h|d|w)?$/i.exec(s.trim());
  if (!m) return null;
  const n = Number(m[1]);
  if (!Number.isFinite(n)) return null;
  const mult: Record<string, number> = { s: 1, m: 60, h: 3600, d: 86400, w: 604800 };
  const unit = (m[2] || "s").toLowerCase();
  return Math.max(1, Math.min(1_209_600, Math.round(n * (mult[unit] ?? 1))));
}

// Followers-only duration, in MINUTES (Helix's follower_mode_duration unit)
// — "mo" (month) must be tried before the single-letter "m" (minute)
// alternative, which the regex group order below guarantees. A bare number
// with no suffix is already minutes. Empty string = "Any" (0 minutes).
export function parseFollowerMinutes(s: string): number | null {
  const t = s.trim().toLowerCase();
  if (!t) return 0;
  const m = /^(\d+)(mo|m|h|d|w)?$/.exec(t);
  if (!m) return null;
  const n = Number(m[1]);
  if (!Number.isFinite(n)) return null;
  const mult: Record<string, number> = { m: 1, h: 60, d: 1440, w: 10080, mo: 43200 };
  return Math.round(n * (mult[m[2] || "m"] ?? 1));
}

const ANNOUNCE_COLOR_WORDS = new Set(["blue", "green", "orange", "purple"]);

// "@name" -> "name", trimmed + lowercased (Twitch logins are already
// case-insensitive; the backend's userLogin resolver lowercases too).
export function parseUserArg(s: string): string {
  return (s.startsWith("@") ? s.slice(1) : s).trim().toLowerCase();
}

export function splitArgs(s: string): string[] {
  return s.split(/\s+/).filter(Boolean);
}

async function postJson(source: SourceRef, path: string, body?: Record<string, unknown>, method = "POST") {
  const r = await relayApi(source, path, { method, body: body !== undefined ? JSON.stringify(body) : undefined });
  if (!r.ok) {
    const t = await r.text();
    let msg = t;
    try {
      const j = JSON.parse(t);
      if (j && j.error) msg = String(j.error);
    } catch {
      /* plain text */
    }
    throw new Error(r.status === 403 && msg === "relogin" ? "Log in again to grant this permission" : msg || "action failed");
  }
  return r;
}

export type SlashContext = {
  source: SourceRef;
  toast: Toast;
  /** The newest message in this chat (for a bare /pin). */
  lastMessageId: () => string | null;
  /** The message currently pinned in this chat (for /unpin). */
  pinnedId: () => string | null;
};

// Dispatches every slash command EXCEPT /clear and /raid, which need a confirm step handled by the caller
// (the send box) before this runs, and /me, which falls straight through to the normal send path. Returns
// true on success (caller clears the input); throws Error (shown as a warn toast) on a bad/incomplete
// command; returns false + toasts "not supported" for anything unrecognized.
export async function runSlashCommand(raw: string, cx: SlashContext): Promise<boolean> {
  const { source, toast } = cx;
  const m = /^\/(\S+)\s*([\s\S]*)$/.exec(raw.trim());
  if (!m) return false;
  const cmd = m[1].toLowerCase();
  const rest = m[2].trim();

  const modAction = async (body: Record<string, unknown>, ok: string) => {
    await postJson(source, "/api/mod/action", body);
    toast(ok);
  };
  const patchModes = async (body: Record<string, unknown>, ok: string) => {
    await postJson(source, "/api/chat/settings", body);
    toast(ok);
  };

  switch (cmd) {
    case "ban": {
      const parts = splitArgs(rest);
      const user = parseUserArg(parts[0] || "");
      if (!user) throw new Error("Usage: /ban <user> [reason]");
      await modAction({ action: "ban", userLogin: user, reason: parts.slice(1).join(" ") || undefined }, `banned ${user}`);
      return true;
    }
    case "timeout": {
      const parts = splitArgs(rest);
      const user = parseUserArg(parts[0] || "");
      const dur = parts[1] ? parseDuration(parts[1]) : null;
      if (!user || dur == null) throw new Error("Usage: /timeout <user> <duration> [reason]");
      await modAction(
        { action: "timeout", userLogin: user, duration: dur, reason: parts.slice(2).join(" ") || undefined },
        `timed out ${user} (${parts[1]})`,
      );
      return true;
    }
    case "unban":
    case "untimeout": {
      const user = parseUserArg(splitArgs(rest)[0] || "");
      if (!user) throw new Error(`Usage: /${cmd} <user>`);
      await modAction({ action: "unban", userLogin: user }, `${cmd === "unban" ? "unbanned" : "untimed out"} ${user}`);
      return true;
    }
    case "warn": {
      const parts = splitArgs(rest);
      const user = parseUserArg(parts[0] || "");
      const reason = parts.slice(1).join(" ").trim();
      if (!user || !reason) throw new Error("Usage: /warn <user> <reason>");
      await modAction({ action: "warn", userLogin: user, reason }, `warned ${user}`);
      return true;
    }
    case "slow": {
      const secs = parseDuration(rest || "30");
      if (secs == null) throw new Error("Usage: /slow <seconds>");
      const clamped = Math.max(1, Math.min(1800, secs));
      await patchModes({ slow_mode: true, slow_mode_wait_time: clamped }, `slow mode ${clamped}s`);
      return true;
    }
    case "slowoff":
      await patchModes({ slow_mode: false }, "slow mode off");
      return true;
    case "followers": {
      const mins = parseFollowerMinutes(rest);
      if (mins == null) throw new Error("Usage: /followers [10m|30m|1h|1d|1w|1mo]");
      await patchModes(
        { follower_mode: true, follower_mode_duration: mins },
        mins ? `followers-only (${rest})` : "followers-only (any)",
      );
      return true;
    }
    case "followersoff":
      await patchModes({ follower_mode: false }, "followers-only off");
      return true;
    case "emoteonly":
      await patchModes({ emote_mode: true }, "emote-only on");
      return true;
    case "emoteonlyoff":
      await patchModes({ emote_mode: false }, "emote-only off");
      return true;
    case "subscribers":
      await patchModes({ subscriber_mode: true }, "subscribers-only on");
      return true;
    case "subscribersoff":
      await patchModes({ subscriber_mode: false }, "subscribers-only off");
      return true;
    case "uniquechat":
      await patchModes({ unique_chat_mode: true }, "unique chat on");
      return true;
    case "uniquechatoff":
      await patchModes({ unique_chat_mode: false }, "unique chat off");
      return true;
    case "announce": {
      const parts = splitArgs(rest);
      let color: string | undefined;
      let text = rest;
      if (parts[0] && ANNOUNCE_COLOR_WORDS.has(parts[0].toLowerCase())) {
        color = parts[0].toLowerCase();
        text = parts.slice(1).join(" ");
      }
      text = text.trim();
      if (!text) throw new Error("Usage: /announce [blue|green|orange|purple] <text>");
      await postJson(source, "/api/chat/announce", { text: text.slice(0, 500), color });
      toast("announced");
      return true;
    }
    case "shoutout": {
      const user = parseUserArg(splitArgs(rest)[0] || "");
      if (!user) throw new Error("Usage: /shoutout <user>");
      await postJson(source, "/api/chat/shoutout", { login: user });
      toast(`shoutout @${user}`);
      return true;
    }
    case "shield": {
      const v = rest.toLowerCase();
      if (v !== "on" && v !== "off") throw new Error("Usage: /shield on|off");
      await postJson(source, "/api/chat/shield", { is_active: v === "on" }, "PUT");
      toast(`shield mode ${v}`);
      return true;
    }
    case "unraid":
      await postJson(source, "/api/chat/raid", undefined, "DELETE");
      toast("raid cancelled");
      return true;
    case "delete": {
      const id = splitArgs(rest)[0] || "";
      if (!id) throw new Error("Usage: /delete <messageId>");
      await modAction({ action: "delete", messageId: id }, "deleted");
      return true;
    }
    case "pin": {
      const id = splitArgs(rest)[0] || cx.lastMessageId() || "";
      if (!id) throw new Error("Nothing to pin yet");
      await modAction({ action: "pin", messageId: id, durationSeconds: 1200 }, "Pinned");
      return true;
    }
    case "unpin": {
      const id = cx.pinnedId();
      if (!id) throw new Error("Nothing is pinned");
      await modAction({ action: "unpin", messageId: id }, "unpinned");
      return true;
    }
    default:
      toast(`Twitch removed chat text commands for third-party apps in 2023 - only /me still sends. "/${cmd}" isn't supported.`, "warn");
      return false;
  }
}

/** /clear and /raid, run after the confirm step. */
export async function confirmedCommand(
  source: SourceRef,
  c: { kind: "clear" } | { kind: "raid"; login: string },
  toast: Toast,
): Promise<void> {
  if (c.kind === "clear") {
    await postJson(source, "/api/mod/action", { action: "clear" });
    toast("chat cleared");
  } else {
    await postJson(source, "/api/chat/raid", { login: c.login });
    toast(`raiding @${c.login}`);
  }
}
