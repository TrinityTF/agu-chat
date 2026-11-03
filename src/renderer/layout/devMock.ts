// Dev-only stand-in for window.agu so the layout can be exercised in a plain browser
// (http://localhost:5199). Installed only when import.meta.env.DEV and window.agu is missing;
// inside Electron the real preload bridge is there and this does nothing.
import type {
  AccountsState,
  AguBridge,
  ConnFrame,
  Frame,
  Layout,
  Platform,
  Settings,
  SourceRef,
  UpdaterState,
} from "../protocol";
import { DEFAULT_MOD_BUTTONS } from "../chat/modButtons";

const DEFAULTS: Settings = {
  twitchClientId: "",
  alwaysOnTop: false,
  updateChannel: "beta",
  timestamps: false,
  timestampSeconds: false,
  alternateRows: true,
  fontSize: 13,
  emoteSize: "medium",
  showBadges: true,
  readableColors: true,
  sevenTv: true,
  bttv: true,
  ffz: true,
  sevenTvCosmetics: true,
  deletedMessages: "strike",
  showModButtons: true,
  showPlatformMarks: true,
  hotkeys: {},
  modButtons: DEFAULT_MOD_BUTTONS,
  highlightRules: [],
  mentionSound: false,
  mentionFlash: true,
  highlightSoundFile: "",
  highlightVolume: 70,
  highlightSoundFocused: true,
  ignoredUsers: [],
  ignoredPhrases: [],
};

function createMock(): AguBridge {
  let settings: Settings = { ...DEFAULTS };
  // Kept in sessionStorage too, so a page reload in the browser exercises the "load saved layout" path.
  let layout: Layout | null = null;
  try {
    layout = JSON.parse(sessionStorage.getItem("agu-mock-layout") || "null");
  } catch {
    /* ignore */
  }
  const frameCbs = new Set<(subId: string, frame: Frame) => void>();
  const settingsCbs = new Set<(s: Settings) => void>();
  const accountCbs = new Set<(s: AccountsState) => void>();
  const updaterCbs = new Set<(s: UpdaterState) => void>();
  const log: Record<string, unknown[]> = { layoutSaves: [], opened: [], installs: [], subs: [], unsubs: [], flashes: [] };
  let nextSub = 1;

  const emitFrame = (subId: string, frame: Frame) => frameCbs.forEach((cb) => cb(subId, frame));

  // ---- accounts ----
  let accounts: AccountsState = {
    twitch: { connected: false },
    rumble: { connected: false },
    kick: { connected: false, unsupported: true },
    youtube: { connected: false, unsupported: true },
    x: { connected: false, unsupported: true },
  };
  const setAccount = (p: Platform, st: AccountsState[Platform]) => {
    accounts = { ...accounts, [p]: st };
    accountCbs.forEach((cb) => cb(accounts));
  };
  let twitchTimer: ReturnType<typeof setTimeout> | null = null;

  // ---- updater ----
  let updater: UpdaterState = { status: "dev", channel: "beta", version: null, notes: [], progress: 0, error: null };
  const setUpdater = (p: Partial<UpdaterState>) => {
    updater = { ...updater, ...p };
    updaterCbs.forEach((cb) => cb(updater));
  };

  const bridge: AguBridge = {
    async subscribe(source: SourceRef) {
      const id = `sub${nextSub++}`;
      log.subs.push(source);
      const conn = (c: Partial<ConnFrame> & { state: ConnFrame["state"] }) => emitFrame(id, { type: "conn", ...c } as ConnFrame);
      setTimeout(() => conn({ state: "connecting" }), 20);
      setTimeout(() => {
        const ch = source.channel.toLowerCase();
        if (ch.includes("err")) conn({ state: "error", detail: "Mock: connection refused" });
        else if (ch.includes("off")) conn({ state: "offline", detail: "Not live" });
        else if (ch.includes("na")) conn({ state: "unavailable", detail: "Chat is disabled for this live stream." });
        else conn({ state: "open", live: true, title: `Mock stream on ${source.platform}: ${source.channel}`, canSend: false });
        emitFrame(id, { type: "chatlog", messages: [] });
      }, 600);
      return id;
    },
    async unsubscribe(subId) {
      log.unsubs.push(subId);
    },
    onFrame(cb) {
      frameCbs.add(cb);
      return () => frameCbs.delete(cb);
    },
    async api() {
      return { status: 404, body: { error: "not found" } };
    },

    accounts: {
      async get() {
        return accounts;
      },
      async login(platform, args) {
        if (platform === "twitch") {
          const pending = { userCode: "ABCDEFGH", verificationUri: "https://www.twitch.tv/activate?device-code=ABCDEFGH", expiresAt: Date.now() + 15 * 60_000 };
          setAccount("twitch", { connected: false, pending });
          twitchTimer = setTimeout(
            () =>
              setAccount("twitch", {
                connected: true,
                user: { id: "1", login: "mock_user", name: "Mock_User" },
                scopes: ["chat:read", "chat:edit", "moderator:manage:banned_users"],
              }),
            8000,
          );
          return pending;
        }
        if (platform === "rumble") {
          const a = (args ?? {}) as { username?: string; password?: string; code?: string };
          if (a.code !== undefined) {
            if (a.code !== "123456") throw new Error("Wrong code (mock accepts 123456).");
            setAccount("rumble", { connected: true, user: { id: "r1", login: "mockrumble", name: "MockRumble" } });
            return { ok: true };
          }
          if (a.username === "2fa") return { twoFactor: true };
          if (a.password === "bad") throw new Error("Invalid username or password.");
          setAccount("rumble", { connected: true, user: { id: "r1", login: a.username ?? "", name: a.username ?? "" } });
          return { ok: true };
        }
        throw new Error("Not supported in the mock.");
      },
      async cancel(platform) {
        if (twitchTimer) clearTimeout(twitchTimer);
        setAccount(platform, { connected: false });
      },
      async logout(platform) {
        setAccount(platform, { connected: false });
      },
      onChange(cb) {
        accountCbs.add(cb);
        return () => accountCbs.delete(cb);
      },
    },

    settings: {
      async get() {
        return settings;
      },
      async set(patch) {
        const next = { ...settings };
        for (const [k, v] of Object.entries(patch)) {
          if (!(k in DEFAULTS) || typeof v !== typeof (DEFAULTS as any)[k]) continue;
          (next as any)[k] = typeof v === "string" ? v.trim() : v;
        }
        settings = next;
        settingsCbs.forEach((cb) => cb(settings));
        return settings;
      },
      onChange(cb) {
        settingsCbs.add(cb);
        return () => settingsCbs.delete(cb);
      },
    },

    layout: {
      async get() {
        return layout;
      },
      async set(l) {
        layout = l;
        try {
          sessionStorage.setItem("agu-mock-layout", JSON.stringify(l));
        } catch {
          /* ignore */
        }
        log.layoutSaves.push(JSON.parse(JSON.stringify(l)));
      },
    },

    updater: {
      async get() {
        return updater;
      },
      async check() {
        setUpdater({ status: "checking", error: null });
        setTimeout(() => setUpdater({ status: "available", version: "0.0.1-beta.2", notes: ["Mock: layout work", "Mock: new settings dialog"] }), 800);
        setTimeout(() => setUpdater({ status: "downloading", progress: 10 }), 1400);
        setTimeout(() => setUpdater({ status: "downloading", progress: 60 }), 2000);
        setTimeout(() => setUpdater({ status: "ready", progress: 100 }), 2600);
      },
      async install() {
        log.installs.push(Date.now());
      },
      onChange(cb) {
        updaterCbs.add(cb);
        return () => updaterCbs.delete(cb);
      },
    },

    app: {
      async info() {
        return { version: "0.0.1-beta.1", packaged: false };
      },
      async openExternal(url) {
        log.opened.push(url);
      },
      async flash() {
        log.flashes.push(Date.now());
      },
    },

    win: {
      async minimize() {},
      async toggleMaximize() {},
      async close() {},
      async isMaximized() {
        return false;
      },
      onMaximized() {
        return () => {};
      },
    },

    sounds: {
      async pick() {
        return { file: "mock-sound.mp3" };
      },
      async read() {
        throw new Error("No sound files in the mock.");
      },
    },
  };

  (window as any).__aguMock = { log, emit: emitFrame, get layout() { return layout; }, get settings() { return settings; } };
  return bridge;
}

if (import.meta.env.DEV && typeof window !== "undefined" && !(window as any).agu) {
  (window as any).agu = createMock();
  console.info("[devMock] window.agu is missing, using the in-memory mock bridge");
}
