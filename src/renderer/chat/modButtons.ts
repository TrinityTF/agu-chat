// Mod-mode row buttons: the user's list (settings.modButtons), its defaults, validation, durations and the
// {placeholder} expansion of command buttons. Pure (no React, no runtime imports), so `node --test` can
// load it directly.
import type { ModButton, ModButtonAction, ModButtonIcon } from "../protocol";

export const MOD_BUTTON_ICONS: { id: ModButtonIcon; label: string }[] = [
  { id: "ban", label: "Ban" },
  { id: "clock", label: "Clock" },
  { id: "trash", label: "Trash" },
  { id: "shield", label: "Shield" },
  { id: "warning", label: "Warning" },
  { id: "flag", label: "Flag" },
  { id: "eyeoff", label: "Hidden" },
  { id: "chat", label: "Chat" },
  { id: "pin", label: "Pin" },
  { id: "check", label: "Check" },
];
const ICON_IDS = new Set<string>(MOD_BUTTON_ICONS.map((i) => i.id));
const ACTIONS = new Set<ModButtonAction>(["ban", "timeout", "delete", "command"]);

export const MAX_MOD_BUTTONS = 12;
export const MAX_LABEL = 8;
export const MAX_COMMAND = 500;
/** Twitch's timeout cap: 2 weeks. */
export const MAX_TIMEOUT = 1_209_600;

/** Today's (and the default) set: ban, timeout 10 minutes, delete. */
export const DEFAULT_MOD_BUTTONS: ModButton[] = [
  { id: "ban", action: "ban", icon: "ban", label: "", seconds: 0, command: "" },
  { id: "timeout", action: "timeout", icon: "clock", label: "", seconds: 600, command: "" },
  { id: "delete", action: "delete", icon: "trash", label: "", seconds: 0, command: "" },
];

/** Plain seconds, or a number with an s/m/h/d/w suffix, clamped to 1s..2 weeks. Null if it does not parse. */
export function parseDuration(s: string): number | null {
  const m = /^(\d+)\s*(s|m|h|d|w)?$/i.exec(String(s).trim());
  if (!m) return null;
  const n = Number(m[1]);
  if (!Number.isFinite(n)) return null;
  const mult: Record<string, number> = { s: 1, m: 60, h: 3600, d: 86400, w: 604800 };
  const unit = (m[2] || "s").toLowerCase();
  return Math.max(1, Math.min(MAX_TIMEOUT, Math.round(n * (mult[unit] ?? 1))));
}

/** 600 -> "10m", 90 -> "90s", 86400 -> "1d": the largest unit that divides it evenly. */
export function formatDuration(seconds: number): string {
  const s = Math.max(1, Math.round(seconds));
  for (const [unit, n] of [
    ["w", 604800],
    ["d", 86400],
    ["h", 3600],
    ["m", 60],
  ] as const) {
    if (s % n === 0) return `${s / n}${unit}`;
  }
  return `${s}s`;
}

let seq = 0;
export function newModButtonId(): string {
  seq += 1;
  return `b${Date.now().toString(36)}${seq.toString(36)}`;
}

/** One stored entry -> a valid button, or null if it can't be one. Fills missing fields with sane values. */
export function normalizeModButton(v: unknown): ModButton | null {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const o = v as Record<string, unknown>;
  const action = o.action as ModButtonAction;
  if (!ACTIONS.has(action)) return null;
  const icon = typeof o.icon === "string" && ICON_IDS.has(o.icon) ? (o.icon as ModButtonIcon) : "";
  const rawLabel = typeof o.label === "string" ? o.label.slice(0, MAX_LABEL) : "";
  const label = rawLabel.trim() ? rawLabel : "";
  const rawSecs = typeof o.seconds === "number" && Number.isFinite(o.seconds) ? Math.round(o.seconds) : 0;
  const seconds = action === "timeout" ? Math.max(1, Math.min(MAX_TIMEOUT, rawSecs || 600)) : 0;
  const command = action === "command" && typeof o.command === "string" ? o.command.trim().slice(0, MAX_COMMAND) : "";
  if (action === "command" && !command) return null;
  const id = typeof o.id === "string" && o.id ? o.id.slice(0, 40) : newModButtonId();
  return { id, action, icon: icon || (label ? "" : defaultIcon(action)), label, seconds, command };
}

export function defaultIcon(action: ModButtonAction): ModButtonIcon {
  return action === "ban" ? "ban" : action === "timeout" ? "clock" : action === "delete" ? "trash" : "chat";
}

/** settings.modButtons -> the buttons to draw, in order. Not an array (missing / corrupt) -> the defaults. */
export function normalizeModButtons(v: unknown): ModButton[] {
  if (!Array.isArray(v)) return DEFAULT_MOD_BUTTONS;
  const out: ModButton[] = [];
  const ids = new Set<string>();
  for (const x of v) {
    const b = normalizeModButton(x);
    if (!b) continue;
    if (ids.has(b.id)) b.id = newModButtonId();
    ids.add(b.id);
    out.push(b);
    if (out.length >= MAX_MOD_BUTTONS) break;
  }
  return out;
}

/** The tooltip: what the button does. */
export function describeModButton(b: ModButton): string {
  switch (b.action) {
    case "ban":
      return "Ban";
    case "timeout":
      return `Timeout ${formatDuration(b.seconds)}`;
    case "delete":
      return "Delete message";
    case "command":
      return b.command;
  }
}

export type TemplateVars = {
  /** Login (what commands take). */
  user: string;
  /** Display name. */
  name?: string;
  userId?: string;
  msgId?: string;
  channel?: string;
  /** The message text. */
  msg?: string;
};

/**
 * Fills {user} {name} {user-id} {msg-id} {channel} {msg} in a command template. Unknown placeholders stay as
 * typed, and values are inserted once (a value containing "{user}" is not expanded again).
 */
export function expandTemplate(template: string, vars: TemplateVars): string {
  const map: Record<string, string | undefined> = {
    user: vars.user,
    name: vars.name ?? vars.user,
    "user-id": vars.userId,
    "msg-id": vars.msgId,
    channel: vars.channel,
    msg: vars.msg,
  };
  return template.replace(/\{([a-z-]+)\}/gi, (whole, key: string) => {
    const v = map[key.toLowerCase()];
    return v === undefined ? whole : v.replace(/[\r\n]+/g, " ");
  });
}

export const TEMPLATE_HELP = "{user} login, {name} display name, {user-id}, {msg-id}, {channel}, {msg} message text";
