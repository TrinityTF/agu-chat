// Auto-updates from GitHub Releases (TrinityTF/agu-chat), with two channels:
//   beta   = GitHub prereleases (0.0.1-beta.N) AND full releases, whichever is newer
//   stable = full releases only
// Updates download in the background and install when the app quits; state "ready" lets the UI offer
// "Restart to update" (install()). Rules and the electron-updater behaviour behind them: docs/RELEASING.md.
import { app } from "electron";
import { EventEmitter } from "node:events";
import { createRequire } from "node:module";
import { log } from "./log.js";

const require = createRequire(import.meta.url);

const FIRST_CHECK_MS = 5_000; // shortly after start
const CHECK_EVERY_MS = 4 * 60 * 60_000; // then every 4 hours
const RETRY_AFTER_ERROR_MS = 15 * 60_000; // a failed background check is retried sooner than 4h

export const events = new EventEmitter();
const state = {
  status: "idle", // idle | dev | checking | none | available | downloading | ready | error
  channel: "beta", // "beta" | "stable"
  version: null, // the update being downloaded / ready
  notes: [],
  progress: 0, // 0..1 while downloading
  error: null,
};

let autoUpdater = null;
let CancellationToken = null;
let inFlightCheck = null; // promise of the running checkForUpdates()
let recheckQueued = false; // a channel change arrived while a check was running
let download = null; // { promise, token } of the running download
let retryTimer = null;
let lastPercent = -1;

const isPrerelease = (version) => /^\d+\.\d+\.\d+-/.test(String(version || ""));
const normalizeChannel = (channel) => (channel === "stable" ? "stable" : "beta");
/** Would an update to `version` be allowed on the current channel? (stable never takes a prerelease) */
const allowedOnChannel = (version) => state.channel === "beta" || !isPrerelease(version);

function set(patch) {
  Object.assign(state, patch);
  events.emit("change");
}

/** Release notes arrive as HTML (GitHub's release feed) or a list of versions; turn them into plain lines. */
function notesToLines(notes) {
  const html = Array.isArray(notes) ? notes.map((n) => n.note || "").join("\n") : String(notes || "");
  return html
    .replace(/<\/(p|li|h\d)>|<br\s*\/?>/gi, "\n")
    .replace(/<[^>]+>/g, "")
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .split("\n")
    .map((line) => line.replace(/^\s*[-*•]\s*/, "").trim())
    .filter(Boolean)
    .slice(0, 12);
}

function friendlyError(err) {
  const message = String(err?.message || err);
  if (err?.code === "ERR_UPDATER_CHANNEL_FILE_NOT_FOUND") return "The newest release has no update files yet. Try again in a few minutes.";
  if (/ENOTFOUND|ETIMEDOUT|ECONNRESET|ECONNREFUSED|EAI_AGAIN|net::/i.test(message)) return "Could not reach GitHub. Check your internet connection.";
  if (/rate limit|403/i.test(message)) return "GitHub is limiting requests right now. Try again later.";
  return message.split("\n")[0].slice(0, 200);
}

/** "No release on this channel yet" is not a failure: the repo may simply have nothing published for it. */
const isNothingPublished = (err) => {
  // electron-updater wraps these (verified against GitHubProvider.js): an empty releases feed is "No published
  // versions on GitHub"; stable with no full release is "Unable to find latest version ... HttpError: 404".
  const message = String(err?.message || "");
  return /No published versions/i.test(message) || (/Unable to find latest version/i.test(message) && /\b404\b/.test(message));
};

const isCancel = (err) => err?.name === "CancellationError" || /cancel/i.test(String(err?.message || ""));

function scheduleRetry() {
  clearTimeout(retryTimer);
  retryTimer = setTimeout(check, RETRY_AFTER_ERROR_MS);
  retryTimer.unref?.();
}

/** Throw away an update that is not allowed on the current channel (e.g. a beta after switching to stable). */
function discardPending() {
  autoUpdater.autoInstallOnAppQuit = false; // a downloaded installer must not run on quit; re-enabled on the next valid update
  download?.token.cancel();
  set({ status: "none", version: null, notes: [], progress: 0, error: null });
}

function startDownload() {
  const token = new CancellationToken();
  lastPercent = -1;
  const promise = autoUpdater
    .downloadUpdate(token)
    .catch(() => {}) // failures reach the 'error' event; cancellation is deliberate
    .finally(() => {
      if (download?.token === token) download = null;
    });
  download = { promise, token };
}

export function init(channel) {
  state.channel = normalizeChannel(channel);
  if (!app.isPackaged) return set({ status: "dev" });

  const eu = require("electron-updater");
  autoUpdater = eu.autoUpdater;
  CancellationToken = eu.CancellationToken;
  autoUpdater.autoDownload = false; // we start the download ourselves so a channel switch can veto it
  autoUpdater.autoInstallOnAppQuit = true; // install the downloaded update when the app quits
  autoUpdater.allowDowngrade = false; // never move to an older version (see docs/RELEASING.md)
  autoUpdater.allowPrerelease = state.channel === "beta"; // beta: also consider prereleases
  autoUpdater.logger = {
    info: (m) => log("[updater]", String(m)),
    warn: (m) => log("[updater]", String(m)),
    error: (m) => log("[updater]", String(m)),
    debug: () => {},
  };

  autoUpdater.on("checking-for-update", () => set({ status: "checking", error: null }));
  autoUpdater.on("update-not-available", () => set({ status: "none", version: null, notes: [], progress: 0 }));
  autoUpdater.on("update-available", (info) => {
    if (!allowedOnChannel(info.version)) {
      // Channel changed while this check was running: ignore it, the queued re-check decides.
      return set({ status: "none", version: null, notes: [], progress: 0 });
    }
    autoUpdater.autoInstallOnAppQuit = true;
    set({ status: "downloading", version: info.version, notes: notesToLines(info.releaseNotes), progress: 0, error: null });
    startDownload();
  });
  autoUpdater.on("download-progress", (p) => {
    const percent = Math.floor(p.percent || 0);
    if (percent === lastPercent) return;
    lastPercent = percent;
    set({ status: "downloading", progress: percent / 100 });
  });
  autoUpdater.on("update-downloaded", (info) => {
    if (!allowedOnChannel(info.version)) return discardPending();
    autoUpdater.autoInstallOnAppQuit = true;
    set({ status: "ready", version: info.version, progress: 1, error: null });
  });
  autoUpdater.on("error", (err) => {
    log("[updater] error", err instanceof Error ? err : String(err));
    if (isCancel(err)) return;
    if (isNothingPublished(err)) return set({ status: "none", version: null, notes: [], progress: 0, error: null });
    // A failed install keeps "ready" (the update is still downloaded); anything else becomes an error.
    set({ status: state.status === "ready" ? "ready" : "error", error: friendlyError(err) });
    if (state.status === "error") scheduleRetry();
  });

  setTimeout(check, FIRST_CHECK_MS).unref?.();
  setInterval(check, CHECK_EVERY_MS).unref?.();
}

/** Switch channel (called on every settings change, so unchanged values are a no-op). Re-checks at once. */
export function setChannel(channel) {
  const next = normalizeChannel(channel);
  if (next === state.channel) return;
  state.channel = next;
  if (!autoUpdater) return set({}); // dev: just remember it

  autoUpdater.allowPrerelease = next === "beta";
  autoUpdater.allowDowngrade = false;
  // A pending prerelease must not be installed once the user asks for stable only.
  if (["downloading", "ready"].includes(state.status) && !allowedOnChannel(state.version)) {
    const running = download?.promise;
    discardPending();
    return running ? void running.then(check) : check();
  }
  set({});
  if (inFlightCheck) recheckQueued = true; // its result belongs to the old channel; run again afterwards
  else check();
}

/** Looks for a newer release on the current channel; a found update downloads by itself. */
export function check() {
  if (!autoUpdater || ["downloading", "ready"].includes(state.status)) return;
  if (inFlightCheck) return;
  inFlightCheck = autoUpdater
    .checkForUpdates()
    .catch(() => {}) // reported through the 'error' event
    .finally(() => {
      inFlightCheck = null;
      if (recheckQueued) {
        recheckQueued = false;
        check();
      }
    });
}

/** Restarts the app into the downloaded update ("Restart to update"). */
export async function install() {
  if (!autoUpdater || state.status !== "ready") throw new Error("No update is ready to install.");
  autoUpdater.quitAndInstall(true, true); // silent install, then relaunch
}

export const getState = () => ({ ...state });
