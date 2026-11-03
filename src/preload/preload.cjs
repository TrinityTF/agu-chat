// The window.agu bridge. CommonJS because sandboxed preloads cannot be ES modules.
// Every invoke answers {ok, value} or {ok:false, error}; errors are re-thrown here
// so renderer code can use plain try/catch.
const { contextBridge, ipcRenderer } = require("electron");

async function call(channel, ...args) {
  const res = await ipcRenderer.invoke(channel, ...args);
  if (!res || !res.ok) throw new Error((res && res.error) || `${channel} failed`);
  return res.value;
}

function listen(channel, cb) {
  const fn = (_event, ...args) => cb(...args);
  ipcRenderer.on(channel, fn);
  return () => ipcRenderer.removeListener(channel, fn);
}

contextBridge.exposeInMainWorld("agu", {
  subscribe: (source) => call("relay:subscribe", source),
  unsubscribe: (subId) => call("relay:unsubscribe", subId),
  onFrame: (cb) => listen("relay:frame", cb),
  api: (source, path, init) => call("relay:api", source, path, init),

  accounts: {
    get: () => call("accounts:get"),
    login: (platform, args) => call("accounts:login", platform, args),
    cancel: (platform) => call("accounts:cancel", platform),
    logout: (platform) => call("accounts:logout", platform),
    onChange: (cb) => listen("accounts:changed", cb),
  },
  settings: {
    get: () => call("settings:get"),
    set: (patch) => call("settings:set", patch),
    onChange: (cb) => listen("settings:changed", cb),
  },
  layout: {
    get: () => call("layout:get"),
    set: (layout) => call("layout:set", layout),
  },
  updater: {
    get: () => call("updater:get"),
    check: () => call("updater:check"),
    install: () => call("updater:install"),
    onChange: (cb) => listen("updater:changed", cb),
  },
  app: {
    info: () => call("app:info"),
    openExternal: (url) => call("app:openExternal", url),
    flash: () => call("app:flash"),
  },
  win: {
    minimize: () => call("win:minimize"),
    toggleMaximize: () => call("win:toggleMaximize"),
    close: () => call("win:close"),
    isMaximized: () => call("win:isMaximized"),
    onMaximized: (cb) => listen("win:maximized", cb),
    fitPopout: (name, height) => call("win:fitPopout", name, height),
  },
  sounds: {
    pick: () => call("sounds:pick"),
    read: (file) => call("sounds:read", file),
  },
});
