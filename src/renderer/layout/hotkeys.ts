// Keyboard shortcuts: the action list with default bindings, binding parsing/formatting, and the merge of
// the user's overrides (settings.hotkeys) over the defaults. Pure (no React, no runtime imports), so
// `node --test` can load it directly.

export type HotkeyAction =
  | "newTab"
  | "closeSplit"
  | "closeTab"
  | "nextTab"
  | "prevTab"
  | "selectTab1"
  | "selectTab2"
  | "selectTab3"
  | "selectTab4"
  | "selectTab5"
  | "selectTab6"
  | "selectTab7"
  | "selectTab8"
  | "selectLastTab"
  | "openSettings";

export type HotkeyDef = { id: HotkeyAction; label: string; group: string; defaults: string[] };

export const HOTKEY_ACTIONS: HotkeyDef[] = [
  { id: "newTab", label: "New tab", group: "Tabs", defaults: ["Ctrl+T"] },
  { id: "closeTab", label: "Close tab", group: "Tabs", defaults: ["Ctrl+Shift+W"] },
  { id: "nextTab", label: "Next tab", group: "Tabs", defaults: ["Ctrl+Tab", "Ctrl+PageDown"] },
  { id: "prevTab", label: "Previous tab", group: "Tabs", defaults: ["Ctrl+Shift+Tab", "Ctrl+PageUp"] },
  ...[1, 2, 3, 4, 5, 6, 7, 8].map((n) => ({ id: `selectTab${n}` as HotkeyAction, label: `Select tab ${n}`, group: "Tabs", defaults: [`Ctrl+${n}`] })),
  { id: "selectLastTab", label: "Select last tab", group: "Tabs", defaults: ["Ctrl+9"] },
  { id: "closeSplit", label: "Close split", group: "Splits", defaults: ["Ctrl+W"] },
  { id: "openSettings", label: "Open / close settings", group: "Window", defaults: ["Ctrl+P"] },
];

const BY_ID = new Map(HOTKEY_ACTIONS.map((a) => [a.id, a]));

/** What settings.hotkeys holds: only the actions the user changed (an empty list = unbound). */
export type HotkeyOverrides = Record<string, string[]>;
export type HotkeyTable = Record<HotkeyAction, string[]>;

export type Binding = { ctrl: boolean; alt: boolean; shift: boolean; key: string };

const NAMED_KEYS = [
  "Tab",
  "Enter",
  "Space",
  "Backspace",
  "Delete",
  "Insert",
  "Home",
  "End",
  "PageUp",
  "PageDown",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
  "Escape",
];
const NAMED_LOWER = new Map(NAMED_KEYS.map((k) => [k.toLowerCase(), k]));
const KEY_ALIASES: Record<string, string> = { esc: "Escape", del: "Delete", ins: "Insert", pgup: "PageUp", pgdn: "PageDown", pagedn: "PageDown", up: "ArrowUp", down: "ArrowDown", left: "ArrowLeft", right: "ArrowRight", return: "Enter" };
const PUNCT = new Set(["-", "=", "[", "]", ";", "'", ",", ".", "/", "\\", "`"]);

function normKey(raw: string): string | null {
  const k = raw.trim();
  if (!k) return null;
  if (/^[a-z0-9]$/i.test(k)) return k.toUpperCase();
  if (PUNCT.has(k)) return k;
  const lower = k.toLowerCase();
  const fn = /^f([1-9]|1[0-9]|2[0-4])$/.exec(lower);
  if (fn) return `F${fn[1]}`;
  return NAMED_LOWER.get(lower) ?? KEY_ALIASES[lower] ?? null;
}

/** "ctrl+shift+w" -> {ctrl, shift, key: "W"}; null for anything that is not a single valid shortcut. */
export function parseBinding(s: string): Binding | null {
  if (typeof s !== "string") return null;
  const t = s.trim();
  if (!t) return null;
  // The key itself may be "+" only as "Plus"; split on "+" that separates parts.
  const parts = t.split("+").map((p) => p.trim());
  if (parts.some((p) => !p)) return null;
  const b: Binding = { ctrl: false, alt: false, shift: false, key: "" };
  for (let i = 0; i < parts.length; i++) {
    const p = parts[i].toLowerCase();
    const last = i === parts.length - 1;
    if (!last) {
      if (p === "ctrl" || p === "control" || p === "cmd" || p === "meta") {
        if (b.ctrl) return null;
        b.ctrl = true;
      } else if (p === "alt" || p === "option") {
        if (b.alt) return null;
        b.alt = true;
      } else if (p === "shift") {
        if (b.shift) return null;
        b.shift = true;
      } else return null;
    } else {
      const k = normKey(parts[i]);
      if (!k) return null;
      b.key = k;
    }
  }
  return b;
}

export function formatBinding(b: Binding): string {
  return [b.ctrl && "Ctrl", b.alt && "Alt", b.shift && "Shift", b.key].filter(Boolean).join("+");
}

/** Canonical form ("ctrl+shift+w" -> "Ctrl+Shift+W"), or null when it does not parse. */
export function normalizeBinding(s: string): string | null {
  const b = parseBinding(s);
  return b ? formatBinding(b) : null;
}

/** A binding we let the user assign: needs Ctrl or Alt, except the function keys (a bare letter would eat typing). */
export function isUsableBinding(s: string): boolean {
  const b = parseBinding(s);
  if (!b) return false;
  return b.ctrl || b.alt || /^F\d+$/.test(b.key);
}

const CODE_PUNCT: Record<string, string> = {
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Semicolon: ";",
  Quote: "'",
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backslash: "\\",
  Backquote: "`",
};

export type KeyLike = { key: string; code?: string; ctrlKey: boolean; altKey: boolean; shiftKey: boolean; metaKey?: boolean };

/** The binding a keydown event produces, or null for a lone modifier / an unknown key. Ctrl and Cmd are the same. */
export function eventToBinding(e: KeyLike): string | null {
  if (["Control", "Shift", "Alt", "Meta", "AltGraph", "CapsLock", "OS"].includes(e.key)) return null;
  const code = e.code ?? "";
  let key: string | null = null;
  // Physical letter/digit keys, so Shift+1 is "Shift+1" and not "!".
  const letter = /^Key([A-Z])$/.exec(code);
  const digit = /^(?:Digit|Numpad)([0-9])$/.exec(code);
  if (letter) key = letter[1];
  else if (digit) key = digit[1];
  else if (CODE_PUNCT[code]) key = CODE_PUNCT[code];
  else key = normKey(e.key === " " ? "Space" : e.key);
  if (!key) return null;
  return formatBinding({ ctrl: e.ctrlKey || !!e.metaKey, alt: e.altKey, shift: e.shiftKey, key });
}

/** Defaults with the user's overrides applied. Bad or unusable entries and unknown actions are dropped. */
export function resolveHotkeys(overrides: unknown): HotkeyTable {
  const o = overrides && typeof overrides === "object" && !Array.isArray(overrides) ? (overrides as Record<string, unknown>) : {};
  const out = {} as HotkeyTable;
  for (const a of HOTKEY_ACTIONS) {
    const v = Object.prototype.hasOwnProperty.call(o, a.id) ? o[a.id] : undefined;
    const list = Array.isArray(v) ? v : a.defaults;
    const seen = new Set<string>();
    out[a.id] = [];
    for (const s of list) {
      const n = typeof s === "string" ? normalizeBinding(s) : null;
      if (n && isUsableBinding(n) && !seen.has(n)) {
        seen.add(n);
        out[a.id].push(n);
      }
    }
  }
  return out;
}

/** binding -> action, for the key handler. When two actions share a binding the first in HOTKEY_ACTIONS wins. */
export function bindingMap(table: HotkeyTable): Map<string, HotkeyAction> {
  const m = new Map<string, HotkeyAction>();
  for (const a of HOTKEY_ACTIONS) for (const b of table[a.id] ?? []) if (!m.has(b)) m.set(b, a.id);
  return m;
}

/** Bindings used by more than one action: binding -> the actions that share it. */
export function findConflicts(table: HotkeyTable): Map<string, HotkeyAction[]> {
  const users = new Map<string, HotkeyAction[]>();
  for (const a of HOTKEY_ACTIONS) for (const b of table[a.id] ?? []) users.set(b, [...(users.get(b) ?? []), a.id]);
  for (const [b, list] of users) if (list.length < 2) users.delete(b);
  return users;
}

/** The overrides that store `table`: actions equal to their defaults are left out. */
export function toOverrides(table: HotkeyTable): HotkeyOverrides {
  const out: HotkeyOverrides = {};
  for (const a of HOTKEY_ACTIONS) {
    const cur = table[a.id] ?? [];
    const same = cur.length === a.defaults.length && cur.every((b, i) => b === a.defaults[i]);
    if (!same) out[a.id] = cur.slice();
  }
  return out;
}

export function actionLabel(id: HotkeyAction): string {
  return BY_ID.get(id)?.label ?? id;
}

// While the settings page records a new shortcut, the global key handlers (shortcuts, Esc-closes-dialog)
// stand aside so the pressed keys reach the recorder.
let recording = false;
export const hotkeyRecording = {
  get active() {
    return recording;
  },
  set(v: boolean) {
    recording = v;
  },
};
