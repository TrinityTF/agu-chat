// Settings (plain JSON) and secrets (encrypted with Electron safeStorage) in userData.
// Secrets never leave the main process: the renderer only ever sees account state.
import { app, safeStorage } from "electron";
import fs from "node:fs";
import path from "node:path";
import { EventEmitter } from "node:events";

// Keep in sync with Settings in src/renderer/protocol.ts.
export const DEFAULT_SETTINGS = {
  twitchClientId: "",
  updateChannel: "beta", // "beta" = GitHub prereleases too, "stable" = full releases only
  timestamps: true,
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
  showPlatformMarks: true,
  hotkeys: {}, // action id -> ["Ctrl+T", ...]; only the actions the user changed
  // Inline mod-mode buttons, in order. Same defaults as DEFAULT_MOD_BUTTONS in src/renderer/chat/modButtons.ts.
  modButtons: [
    { id: "ban", action: "ban", icon: "ban", label: "", seconds: 0, command: "" },
    { id: "timeout", action: "timeout", icon: "clock", label: "", seconds: 600, command: "" },
    { id: "delete", action: "delete", icon: "trash", label: "", seconds: 0, command: "" },
  ],
};

// Keys whose default is an object or array: typeof can't tell them apart, so each gets a shape check that
// returns a clean copy (or undefined to reject the value). The renderer validates the meaning on read.
const isPlainObject = (v) => !!v && typeof v === "object" && !Array.isArray(v);
const str = (v, max) => (typeof v === "string" ? v.slice(0, max) : "");

export const STRUCTURED = {
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
};

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
    if (!settings) settings = { ...structuredClone(DEFAULT_SETTINGS), ...readJson("settings.json", {}) };
    return structuredClone(settings);
  },
  set(patch) {
    const next = { ...this.get() };
    for (const [k, v] of Object.entries(patch || {})) {
      if (!Object.hasOwn(DEFAULT_SETTINGS, k)) continue;
      if (Object.hasOwn(STRUCTURED, k)) {
        const clean = STRUCTURED[k](v);
        if (clean !== undefined) next[k] = clean;
        continue;
      }
      if (typeof v !== typeof DEFAULT_SETTINGS[k]) continue;
      next[k] = typeof v === "string" ? v.trim() : v;
    }
    settings = next;
    writeJson("settings.json", settings);
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
