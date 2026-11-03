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
  channelId?: string; // Twitch: the broadcaster id (7TV/BTTV/FFZ are keyed by it)
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

/**
 * A highlight rule (Settings > Highlights). Rules are tried in list order; the first match colours the row.
 * `phrase` matches whole words, `regex` is a JavaScript regular expression, `user` is a list of names
 * (login or display name) separated by commas or spaces. Keep in sync with src/main/highlightRules.js.
 */
export type HighlightRule = {
  id: string;
  enabled: boolean;
  kind: "phrase" | "regex" | "user";
  pattern: string;
  caseSensitive: boolean;
  /** #rrggbb */
  color: string;
  sound: boolean;
  /** A file in the app's sounds folder (from agu.sounds.pick); "" = the default sound. */
  soundFile: string;
  /** Flash the taskbar button when the window isn't focused. */
  flash: boolean;
};

/** A user whose messages are hidden, on one platform. `login` is lowercase, without "@". */
export type IgnoredUser = { platform: Platform; login: string };
/** A phrase (plain text or a regular expression) that hides a message or replaces the matched text. */
export type IgnoredPhrase = {
  pattern: string;
  regex: boolean;
  caseSensitive: boolean;
  action: "hide" | "replace";
  /** Literal text put in place of each match when `action` is "replace". */
  replacement: string;
};

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
  /** Shortcut overrides: action id -> bindings ("Ctrl+Shift+W"). Actions not listed use their defaults. */
  hotkeys: Record<string, string[]>;
  /** Inline mod-mode buttons, in display order. */
  modButtons: ModButton[];
  highlightRules: HighlightRule[];
  /** Alerts for the built-in "mentions your Twitch name" highlight. */
  mentionSound: boolean;
  mentionFlash: boolean;
  /** Default alert sound: a file in the app's sounds folder, "" = the built-in ping. */
  highlightSoundFile: string;
  /** 0-100 */
  highlightVolume: number;
  /** Play alert sounds while the window is focused too (the taskbar only flashes when it isn't). */
  highlightSoundFocused: boolean;
  ignoredUsers: IgnoredUser[];
  ignoredPhrases: IgnoredPhrase[];
};

export type ModButtonAction = "ban" | "timeout" | "delete" | "command";
export type ModButtonIcon = "ban" | "clock" | "trash" | "shield" | "warning" | "flag" | "eyeoff" | "chat" | "pin" | "check";
export type ModButton = {
  id: string;
  action: ModButtonAction;
  /** Drawn when `label` is empty. */
  icon: ModButtonIcon | "";
  /** Short text shown instead of the icon. */
  label: string;
  /** timeout only. */
  seconds: number;
  /** command only: a slash command or chat line with {user}, {msg-id}, ... placeholders. */
  command: string;
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
  app: {
    info(): Promise<{ version: string; packaged: boolean }>;
    openExternal(url: string): Promise<void>;
    /** Flash the taskbar button until the window is focused (does nothing while it is focused). */
    flash(): Promise<void>;
  };
  sounds: {
    /** Let the user choose an audio file; it is copied into the app's sounds folder. null = cancelled. */
    pick(): Promise<{ file: string } | null>;
    /** The bytes of a file in the sounds folder. */
    read(file: string): Promise<Uint8Array>;
  };
}
