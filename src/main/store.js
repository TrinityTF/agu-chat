// Settings (plain JSON) and secrets (encrypted with Electron safeStorage) in userData.
// Secrets never leave the main process: the renderer only ever sees account state.
import { app, safeStorage } from "electron";
import fs from "node:fs";
import path from "node:path";
import { EventEmitter } from "node:events";
import { sanitizeHighlightRules } from "./highlightRules.js";
import { sanitizeIgnoredPhrases, sanitizeIgnoredUsers, sanitizeSetting } from "./settingsSanitize.js";

// Keep in sync with Settings in src/renderer/protocol.ts.
export const DEFAULT_SETTINGS = {
  twitchClientId: "",
  alwaysOnTop: false,
  updateChannel: "beta", // "beta" = GitHub prereleases too, "stable" = full releases only
  timestamps: false,
  timestampSeconds: false,
  alternateRows: true,
  fontSize: 13,
  emoteSize: "medium", // small | medium | large
  showBadges: true,
  readableColors: true,
  sevenTv: true,
  bttv: true,
  ffz: true,
  sevenTvCosmetics: true,
  deletedMessages: "strike", // strike | hide | show
  showModButtons: true,
  showModActions: true, // the mod log lines in chat (bans, timeouts, mode changes)
  showAutomod: true, // AutoMod's held messages, with Allow / Deny
  pauseChat: "scroll", // scroll | hover | alt: what holds the chat still, besides scrolling up
  showPlatformMarks: true,
  hotkeys: {}, // action id -> ["Ctrl+T", ...]; only the actions the user changed
  // Inline mod-mode buttons, in order. Same defaults as DEFAULT_MOD_BUTTONS in src/renderer/chat/modButtons.ts.
  modButtons: [
    { id: "ban", action: "ban", icon: "ban", label: "", seconds: 0, command: "" },
    { id: "timeout", action: "timeout", icon: "clock", label: "", seconds: 600, command: "" },
    { id: "warn", action: "warn", icon: "warning", label: "", seconds: 0, command: "" },
    { id: "delete", action: "delete", icon: "trash", label: "", seconds: 0, command: "" },
  ],
  highlightRules: [], // see src/main/highlightRules.js
  mentionSound: false,
  mentionFlash: true,
  highlightSoundFile: "", // a file in userData/sounds, "" = the built-in ping
  highlightVolume: 70, // 0-100
  highlightSoundFocused: true,
  ignoredUsers: [], // [{ platform, login }] (see settingsSanitize.js)
  ignoredPhrases: [], // [{ pattern, regex, caseSensitive, action: "hide" | "replace", replacement }]
};

const SETTINGS_VERSION = 3;

const isPlainObject = (v) => !!v && typeof v === "object" && !Array.isArray(v);
const str = (v, max) => (typeof v === "string" ? v.slice(0, max) : "");

// Keys whose value is not a plain string/number/boolean: each gets its own check. A sanitizer returns the
// cleaned value, or undefined to reject the patch for that key.
// The renderer validates the meaning of structured values on read; these only check their shape.
const SANITIZE = {
  hotkeys(v) {
    if (!isPlainObject(v)) return undefined;
    const out = {};
    for (const [k, list] of Object.entries(v).slice(0, 100)) {
      if (k.length > 64 || !Array.isArray(list)) continue;
      out[k] = list.filter((b) => typeof b === "string" && b.length <= 40).slice(0, 8);
    }
    return out;
  },
  modButtons(v) {
    if (!Array.isArray(v)) return undefined;
    return v
      .filter(isPlainObject)
      .slice(0, 12)
      .map((b) => ({
        id: str(b.id, 40),
        action: str(b.action, 16),
        icon: str(b.icon, 16),
        label: str(b.label, 16),
        seconds: typeof b.seconds === "number" && Number.isFinite(b.seconds) ? b.seconds : 0,
        command: str(b.command, 500),
      }));
  },
  highlightRules: sanitizeHighlightRules,
  ignoredUsers: (v) => sanitizeIgnoredUsers(v) ?? undefined,
  ignoredPhrases: (v) => sanitizeIgnoredPhrases(v) ?? undefined,
  highlightVolume: (v) => (typeof v === "number" && Number.isFinite(v) ? Math.max(0, Math.min(100, Math.round(v))) : undefined),
};

function clean(k, v) {
  if (Object.hasOwn(SANITIZE, k)) return SANITIZE[k](v);
  return sanitizeSetting(k, v, DEFAULT_SETTINGS); // scalars: must have the default's type
}

const dir = () => app.getPath("userData");
const file = (name) => path.join(dir(), name);

export function readJson(name, fallback) {
  try {
    return JSON.parse(fs.readFileSync(file(name), "utf8"));
  } catch {
    return fallback;
  }
}

export function writeJson(name, value) {
  fs.mkdirSync(dir(), { recursive: true });
  const target = file(name);
  const tmp = `${target}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(value, null, 2));
  fs.renameSync(tmp, target);
}

export const events = new EventEmitter();

let settings = null;

export const settingsStore = {
  get() {
    if (!settings) {
      settings = structuredClone(DEFAULT_SETTINGS);
      const saved = readJson("settings.json", {});
      // v2 turned timestamps off by default; older files saved every key, so their `true` was just the old default.
      if (saved && typeof saved === "object" && (saved.v ?? 1) < 2) delete saved.timestamps;
      // v3 added warn to the default mod buttons: a list still exactly the old three defaults gets the new set.
      if (saved && typeof saved === "object" && (saved.v ?? 1) < 3 && Array.isArray(saved.modButtons)) {
        const ids = saved.modButtons.map((b) => `${b?.id}:${b?.action}:${b?.seconds}:${b?.label || ""}:${b?.command || ""}`).join(",");
        if (ids === "ban:ban:0::,timeout:timeout:600::,delete:delete:0::") delete saved.modButtons;
      }
      for (const [k, v] of Object.entries(saved && typeof saved === "object" ? saved : {})) {
        if (!Object.hasOwn(DEFAULT_SETTINGS, k)) continue;
        const c = clean(k, v);
        if (c !== undefined) settings[k] = c;
      }
    }
    return structuredClone(settings); // callers may mutate arrays/objects; never hand out the live ones
  },
  set(patch) {
    const next = { ...this.get() };
    for (const [k, v] of Object.entries(patch || {})) {
      if (!Object.hasOwn(DEFAULT_SETTINGS, k)) continue;
      const c = clean(k, v);
      if (c !== undefined) next[k] = c;
    }
    settings = next;
    writeJson("settings.json", { ...settings, v: SETTINGS_VERSION });
    events.emit("settings", this.get());
    return this.get();
  },
};

// All secrets live in one encrypted blob. safeStorage uses DPAPI on Windows,
// so the file is useless on another machine or to another Windows user.
let secrets = null;

function loadSecrets() {
  if (secrets) return secrets;
  secrets = {};
  try {
    const buf = fs.readFileSync(file("secrets.bin"));
    if (safeStorage.isEncryptionAvailable()) secrets = JSON.parse(safeStorage.decryptString(buf));
  } catch {}
  return secrets;
}

function saveSecrets() {
  if (!safeStorage.isEncryptionAvailable()) throw new Error("Secure storage is not available on this system.");
  fs.mkdirSync(dir(), { recursive: true });
  fs.writeFileSync(file("secrets.bin"), safeStorage.encryptString(JSON.stringify(secrets)));
}

export const secretStore = {
  get(key) {
    const v = loadSecrets()[key];
    return v === undefined ? null : structuredClone(v);
  },
  set(key, value) {
    loadSecrets()[key] = value;
    saveSecrets();
  },
  delete(key) {
    delete loadSecrets()[key];
    saveSecrets();
  },
};

export const layoutStore = {
  get: () => readJson("layout.json", null),
  set: (layout) => writeJson("layout.json", layout),
};
