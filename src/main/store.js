// Settings (plain JSON) and secrets (encrypted with Electron safeStorage) in userData.
// Secrets never leave the main process: the renderer only ever sees account state.
import { app, safeStorage } from "electron";
import fs from "node:fs";
import path from "node:path";
import { EventEmitter } from "node:events";
import { loadSettings, sanitizeSetting } from "./settingsSanitize.js";

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
  ignoredUsers: [], // [{ platform, login }] (see settingsSanitize.js)
  ignoredPhrases: [], // [{ pattern, regex, caseSensitive, action: "hide" | "replace", replacement }]
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
    if (!settings) settings = loadSettings(readJson("settings.json", {}), DEFAULT_SETTINGS);
    return structuredClone(settings);
  },
  set(patch) {
    const next = { ...this.get() };
    for (const [k, v] of Object.entries(patch || {})) {
      const value = sanitizeSetting(k, v, DEFAULT_SETTINGS);
      if (value !== undefined) next[k] = value;
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
