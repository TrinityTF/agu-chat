// Agu Chat main process: the window, IPC, and the local relay.
import { app, BrowserWindow, ipcMain, shell, Menu } from "electron";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { settingsStore, secretStore, layoutStore, events as storeEvents } from "./store.js";
import { SourceManager, makeApiRouter } from "./relay.js";
import { modules } from "./platforms.js";
import * as updater from "./updater.js";
import { log } from "./log.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const DEV_URL = process.env.AGU_DEV_URL || "";

if (!app.requestSingleInstanceLock()) app.quit();

let win = null;

function broadcast(channel, ...args) {
  for (const w of BrowserWindow.getAllWindows()) if (!w.isDestroyed()) w.webContents.send(channel, ...args);
}

const ctx = {
  settings: settingsStore,
  secrets: secretStore,
  log,
  userAgent: `AguChat/${app.getVersion()} (+https://github.com/TrinityTF/agu-chat)`,
  accountsChanged: () => broadcast("accounts:changed", accountsState()),
  platforms: modules,
};

const relay = new SourceManager(modules, ctx, (subId, frame) => {
  const sub = relay.subs.get(subId);
  const target = sub && BrowserWindow.fromId(sub.owner);
  if (target && !target.isDestroyed()) target.webContents.send("relay:frame", subId, frame);
});
const api = makeApiRouter(modules, ctx);

function accountsState() {
  const out = {};
  for (const [name, mod] of Object.entries(modules)) {
    try {
      out[name] = mod.accounts ? mod.accounts.state() : { connected: false, unsupported: true };
    } catch (err) {
      out[name] = { connected: false, error: String(err?.message || err) };
    }
  }
  return out;
}

function accountModule(platform) {
  const mod = modules[platform];
  if (!mod?.accounts) throw new Error(`${platform} accounts are not supported.`);
  return mod.accounts;
}

function handle(channel, fn) {
  ipcMain.handle(channel, async (event, ...args) => {
    try {
      return { ok: true, value: await fn(event, ...args) };
    } catch (err) {
      log(`[ipc] ${channel}`, err);
      return { ok: false, error: String(err?.message || err) };
    }
  });
}

function wireIpc() {
  handle("relay:subscribe", (e, ref) => relay.subscribe(ref, BrowserWindow.fromWebContents(e.sender).id));
  handle("relay:unsubscribe", (_e, subId) => relay.unsubscribe(subId));
  handle("relay:api", (_e, source, p, init) => api(source, p, init));

  handle("accounts:get", () => accountsState());
  handle("accounts:login", (_e, platform, args) => accountModule(platform).login(args));
  handle("accounts:cancel", (_e, platform) => accountModule(platform).cancel?.());
  handle("accounts:logout", async (_e, platform) => {
    await accountModule(platform).logout();
    ctx.accountsChanged();
  });

  handle("settings:get", () => settingsStore.get());
  handle("settings:set", (_e, patch) => settingsStore.set(patch));
  storeEvents.on("settings", (s) => {
    broadcast("settings:changed", s);
    updater.setChannel(s.updateChannel);
  });

  handle("layout:get", () => layoutStore.get());
  handle("layout:set", (_e, layout) => layoutStore.set(layout));

  handle("updater:get", () => updater.getState());
  handle("updater:check", () => updater.check());
  handle("updater:install", () => updater.install());
  updater.events.on("change", () => broadcast("updater:changed", updater.getState()));

  handle("app:info", () => ({ version: app.getVersion(), packaged: app.isPackaged }));
  handle("app:openExternal", (_e, url) => {
    if (/^https?:\/\//i.test(String(url))) return shell.openExternal(String(url));
  });
}

function createWindow() {
  win = new BrowserWindow({
    width: 1280,
    height: 800,
    minWidth: 420,
    minHeight: 300,
    backgroundColor: "#0b0b10",
    title: "Agu Chat",
    icon: path.join(here, "../../build/icon.png"),
    autoHideMenuBar: true,
    webPreferences: {
      preload: path.join(here, "../preload/preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  });
  const id = win.id;
  win.on("closed", () => {
    relay.dropOwner(id);
    win = null;
  });
  // Links open in the user's browser, never inside the app.
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:\/\//i.test(url)) shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("will-navigate", (e, url) => {
    if (DEV_URL && url.startsWith(DEV_URL)) return;
    e.preventDefault();
    if (/^https?:\/\//i.test(url)) shell.openExternal(url);
  });
  if (DEV_URL) win.loadURL(DEV_URL);
  else win.loadFile(path.join(here, "../../out/renderer/index.html"));
}

app.on("second-instance", () => {
  if (win) {
    if (win.isMinimized()) win.restore();
    win.focus();
  }
});

app.whenReady().then(async () => {
  Menu.setApplicationMenu(null);
  for (const mod of Object.values(modules)) {
    try {
      await mod.init?.(ctx);
    } catch (err) {
      log(`[init] ${mod.platform}`, err);
    }
  }
  wireIpc();
  createWindow();
  updater.init(settingsStore.get().updateChannel);
});

app.on("window-all-closed", () => {
  relay.stopAll();
  app.quit();
});
