// Agu Chat main process: the window, IPC, and the local relay.
import { app, BrowserWindow, ipcMain, shell, Menu, screen } from "electron";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { settingsStore, secretStore, layoutStore, events as storeEvents } from "./store.js";
import { SourceManager, makeApiRouter } from "./relay.js";
import { modules } from "./platforms.js";
import * as updater from "./updater.js";
import { pickSound, readSound } from "./sounds.js";
import { log } from "./log.js";
import { loadWindowState, placeBounds, trackWindowState } from "./windowState.js";

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
    applyAlwaysOnTop(s.alwaysOnTop);
    applyImageAnimation(s.animatedEmotes);
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
  // Highlight alerts: flash the taskbar button, but only when the user isn't already looking at the window.
  handle("app:flash", (e) => {
    const w = BrowserWindow.fromWebContents(e.sender);
    if (w && !w.isDestroyed() && !w.isFocused()) w.flashFrame(true);
  });

  // The window has no OS frame: the tab bar draws these buttons.
  const own = (e) => BrowserWindow.fromWebContents(e.sender);
  handle("win:minimize", (e) => own(e)?.minimize());
  handle("win:toggleMaximize", (e) => {
    const w = own(e);
    if (w) w.isMaximized() ? w.unmaximize() : w.maximize();
  });
  handle("win:close", (e) => own(e)?.close());
  handle("win:isMaximized", (e) => !!own(e)?.isMaximized());
  handle("win:fitPopout", (_e, name, height) => fitPopout(name, height));

  handle("sounds:pick", (e) => pickSound(BrowserWindow.fromWebContents(e.sender)));
  handle("sounds:read", (_e, file) => readSound(String(file)));
}

function applyAlwaysOnTop(on) {
  if (!win || win.isDestroyed()) return;
  // "floating" keeps it above normal windows without fighting full-screen games for the very top.
  win.setAlwaysOnTop(!!on, "floating");
  for (const child of win.getChildWindows()) child.setAlwaysOnTop(!!on, "floating");
}

// Animated emotes are nearly all of the app's GPU use, so Settings can turn them off. Chromium applies the
// policy only to images loaded after it is set, so a change reloads the chat view (and closes the user cards)
// to apply it to every emote; at startup it is set before anything loads.
let animationPolicy = null;
function applyImageAnimation(on) {
  const policy = on === false ? "noAnimation" : "animate";
  if (policy === animationPolicy) return;
  const first = animationPolicy === null;
  animationPolicy = policy;
  if (!win || win.isDestroyed()) return;
  win.webContents.setImageAnimationPolicy(policy);
  if (first) return;
  for (const p of popouts.values()) if (!p.isDestroyed()) p.close();
  win.webContents.reload();
}

// Pop-out cards (user cards): the renderer opens about:blank windows named "agu-popout:<id>" and draws into them
// itself, so they share its state. Frameless, owned by the main window, free to move anywhere on screen.
const popouts = new Map(); // frameName -> BrowserWindow

// A pop-out's height follows its content until the user resizes it by hand; from then on it keeps their size
// and the card fills it. Done here with content sizes, because a renderer-side resizeTo(outerWidth, ..) grew
// the window a few pixels on every call until it filled the screen.
const userSized = new WeakSet();
function fitPopout(name, height) {
  const w = popouts.get(String(name));
  if (!w || w.isDestroyed() || userSized.has(w)) return;
  const [cw, ch] = w.getContentSize();
  const max = screen.getDisplayMatching(w.getBounds()).workAreaSize.height - 40;
  const h = Math.max(120, Math.min(Math.round(Number(height) || 0), max));
  if (h !== ch) w.setContentSize(cw, h);
}

function popoutOptions(features) {
  // window.open's "left=..,top=..,width=..,height=.." is applied here so it can't be lost in Electron's merge.
  const f = Object.fromEntries(
    String(features || "")
      .split(",")
      .map((kv) => kv.split("=").map((s) => s.trim()))
      .filter(([k, v]) => k && /^-?\d+$/.test(v || ""))
      .map(([k, v]) => [k, Number(v)]),
  );
  return {
    ...(f.left !== undefined && f.top !== undefined ? { x: f.left, y: f.top } : {}),
    width: Math.max(240, f.width || 300),
    height: Math.max(120, f.height || 200),
    frame: false,
    resizable: true,
    minimizable: false,
    maximizable: false,
    fullscreenable: false,
    skipTaskbar: true,
    parent: win ?? undefined,
    alwaysOnTop: !!win?.isAlwaysOnTop(),
    backgroundColor: "#16161d",
    minWidth: 240,
    minHeight: 120,
    autoHideMenuBar: true,
  };
}

function createWindow() {
  const saved = loadWindowState();
  const work = [screen.getPrimaryDisplay(), ...screen.getAllDisplays().filter((d) => d.id !== screen.getPrimaryDisplay().id)].map((d) => d.workArea);
  const at = placeBounds(saved, work);
  win = new BrowserWindow({
    width: 1280,
    height: 800,
    ...(at || {}),
    minWidth: 280,
    minHeight: 300,
    frame: false,
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
  if (saved?.maximized) win.maximize();
  trackWindowState(win);
  const id = win.id;
  win.on("focus", () => win?.flashFrame(false));
  win.on("maximize", () => win?.webContents.send("win:maximized", true));
  win.on("unmaximize", () => win?.webContents.send("win:maximized", false));
  win.once("ready-to-show", () => applyAlwaysOnTop(settingsStore.get().alwaysOnTop));
  win.on("closed", () => {
    relay.dropOwner(id);
    win = null;
  });
  // Links open in the user's browser, never inside the app.
  win.webContents.setWindowOpenHandler(({ url, frameName, features }) => {
    if (url === "about:blank" && frameName.startsWith("agu-popout:")) return { action: "allow", overrideBrowserWindowOptions: popoutOptions(features) };
    if (/^https?:\/\//i.test(url)) shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("will-navigate", (e, url) => {
    if (DEV_URL && url.startsWith(DEV_URL)) return;
    e.preventDefault();
    if (/^https?:\/\//i.test(url)) shell.openExternal(url);
  });
  // Pop-outs never navigate either: a link clicked in a user card goes to the browser too.
  win.webContents.on("did-create-window", (child, details) => {
    const name = details?.frameName || "";
    if (name.startsWith("agu-popout:")) {
      popouts.set(name, child);
      child.webContents.setImageAnimationPolicy(animationPolicy || "animate");
      // will-resize fires only for a resize by hand, never for setContentSize.
      child.on("will-resize", () => {
        if (userSized.has(child)) return;
        userSized.add(child);
        win?.webContents.send("win:popoutUserSized", name);
      });
      child.on("closed", () => popouts.get(name) === child && popouts.delete(name));
    }
    child.webContents.setWindowOpenHandler(({ url }) => {
      if (/^https?:\/\//i.test(url)) shell.openExternal(url);
      return { action: "deny" };
    });
    child.webContents.on("will-navigate", (e, url) => {
      e.preventDefault();
      if (/^https?:\/\//i.test(url)) shell.openExternal(url);
    });
  });
  animationPolicy = null;
  applyImageAnimation(settingsStore.get().animatedEmotes);
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
