// What a row needs from its pane, bundled so memoized rows get a handful of stable props instead of a
// dozen: `Handlers` is created once per pane (it reads current state through refs), `TwitchEnv` once per
// Twitch source and only changes when that source's badges / emotes / caps change.
import type { ModButton, Platform, Settings, SourceRef } from "../../protocol";
import type { ModCaps, ModViewer, CoCaps } from "../modCaps";
import type { BadgeMap } from "../twitch/badges";
import type { SevenTvDefs } from "../twitch/cosmetics";
import type { EmoteSets } from "../twitch/emotes";
import type { KnownChatter } from "../twitch/parts";
import type { ChatMsg, KickMsg, RowUser, RumbleBadgeDefs, RumbleEmotes, RumbleMsg } from "../types";
import type { KickModAction } from "../modActions";

export type ReplyTarget = { sk: string; platform: Platform; id: string; user: string; text: string };

export type Handlers = {
  openCard(target: HTMLElement, sk: string, user: RowUser): void;
  copy(text: string): void;
  reply(t: ReplyTarget): void;
  /** Twitch hover/inline actions. */
  pin(sk: string, id: string): void;
  deleteMsg(sk: string, id: string): void;
  quickTimeout(sk: string, msg: ChatMsg, seconds: number): void;
  /** A mod button's command template, expanded for this message and run as a slash command or sent as chat. */
  twitchCommand(sk: string, msg: ChatMsg, template: string): void;
  modPopover(target: HTMLElement, kind: "ban" | "timeout" | "warn" | "delete", sk: string, msg: ChatMsg): void;
  automod(sk: string, id: string, action: "allow" | "deny"): Promise<void>;
  /** Rumble / Kick moderation (only offered when the account can moderate). */
  rumbleMod(sk: string, action: "timeout" | "ban" | "delete", msg: RumbleMsg, seconds?: number): void;
  kickMod(sk: string, action: KickModAction, msg: KickMsg, seconds?: number): void;
};

export type TwitchEnv = {
  source: SourceRef;
  badgeMap: BadgeMap;
  emotes: EmoteSets | null;
  sevenDefs: SevenTvDefs | null;
  caps: ModCaps;
  known: Map<string, KnownChatter>;
  viewer: ModViewer;
  meLoginLower: string;
};

export type CoEnv = {
  source: SourceRef;
  caps: CoCaps;
  /** The conn frame says this account can post here (Kick replies need it). */
  canSend: boolean;
  rumbleBadges: RumbleBadgeDefs;
  rumbleEmotes: RumbleEmotes;
};

export type RowCtx = {
  rs: Settings;
  h: Handlers;
  /** More than one source in this split (or settings.showPlatformMarks): each row says where it came from. */
  showPlatform: boolean;
  /** Chatterino "mod mode": inline mod buttons at the start of every row. */
  modButtons: boolean;
  /** Which buttons, in order (settings.modButtons, validated). */
  modList: ModButton[];
};
