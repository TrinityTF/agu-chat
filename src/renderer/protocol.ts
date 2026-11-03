// Shared types between main and renderer. See docs/ARCHITECTURE.md.

export type Platform = "twitch" | "kick" | "youtube" | "rumble" | "x";
export const PLATFORMS: Platform[] = ["twitch", "kick", "youtube", "rumble", "x"];
export type SourceRef = { platform: Platform; channel: string };

export type ConnFrame = {
  type: "conn";
  state: "connecting" | "open" | "offline" | "error" | "unavailable";
  detail?: string;
  live?: boolean;
  title?: string;
  canSend?: boolean;
  readOnlyReason?: string;
};

// Every other frame is the platform's own agu.lol relay frame ({type: "chat", msg}, ...).
export type Frame = ConnFrame | ({ type: string } & Record<string, any>);

export type ApiResult = { status: number; body: any };

export type AccountUser = { id: string; login: string; name: string };
export type DevicePrompt = { userCode: string; verificationUri: string; expiresAt: number };
export type AccountState = {
  connected: boolean;
  unsupported?: boolean;
  user?: AccountUser;
  scopes?: string[];
  pending?: DevicePrompt;
  error?: string;
};
export type AccountsState = Record<Platform, AccountState>;

// Keep in sync with DEFAULT_SETTINGS in src/main/store.js.
export type Settings = {
  twitchClientId: string;
  updateChannel: "beta" | "stable";
  timestamps: boolean;
  timestampSeconds: boolean;
  alternateRows: boolean;
  fontSize: number;
  emoteSize: "small" | "medium" | "large";
  showBadges: boolean;
  readableColors: boolean;
  sevenTv: boolean;
  bttv: boolean;
  ffz: boolean;
  sevenTvCosmetics: boolean;
  deletedMessages: "strike" | "hide" | "show";
  showModButtons: boolean;
  showPlatformMarks: boolean;
};

export type SplitNode =
  | { kind: "leaf"; id: string; sources: SourceRef[]; modMode: boolean }
  | { kind: "row" | "col"; id: string; children: SplitNode[]; sizes: number[] };
export type Tab = { id: string; title: string; root: SplitNode | null };
export type Layout = { tabs: Tab[]; active: string };

export type UpdaterState = {
  status: "idle" | "dev" | "checking" | "none" | "available" | "downloading" | "ready" | "error";
  channel: "beta" | "stable";
  version: string | null;
  notes: string[];
  progress: number;
  error: string | null;
};

export interface AguBridge {
  subscribe(source: SourceRef): Promise<string>;
  unsubscribe(subId: string): Promise<void>;
  onFrame(cb: (subId: string, frame: Frame) => void): () => void;
  api(source: SourceRef, path: string, init?: { method?: string; body?: unknown }): Promise<ApiResult>;
  accounts: {
    get(): Promise<AccountsState>;
    login(platform: Platform, args?: Record<string, unknown>): Promise<any>;
    cancel(platform: Platform): Promise<void>;
    logout(platform: Platform): Promise<void>;
    onChange(cb: (s: AccountsState) => void): () => void;
  };
  settings: {
    get(): Promise<Settings>;
    set(patch: Partial<Settings>): Promise<Settings>;
    onChange(cb: (s: Settings) => void): () => void;
  };
  layout: { get(): Promise<Layout | null>; set(layout: Layout): Promise<void> };
  updater: {
    get(): Promise<UpdaterState>;
    check(): Promise<void>;
    install(): Promise<void>;
    onChange(cb: (s: UpdaterState) => void): () => void;
  };
  app: { info(): Promise<{ version: string; packaged: boolean }>; openExternal(url: string): Promise<void> };
}
