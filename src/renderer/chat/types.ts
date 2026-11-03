// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2) wire types, plus the
// RumbleChat / KickChat / YouTubeChat / XChat message types. Changes: collected in one file, the
// second-channel ("alt") fields dropped, a unified `Row` added for the merged message list.
import type { Platform } from "../protocol";

export type ChatUser = { id: string; login: string; name: string; color: string };

// `bits` is set (and `cost` null) for a Bits Power-up, which reaches chat through the same
// custom-reward-id tag as a channel-point redeem.
export type RewardInfo = { title: string; cost: number | null; bits?: number };

export type PinInfo = { by: { id: string; login: string; name: string }; startsAt: string | null; endsAt: string | null };

export type ChatMsg = {
  platform: "twitch";
  id: string;
  ts: number;
  user: ChatUser;
  badges: string;
  text: string;
  emotes: string;
  gifs?: string;
  action: boolean;
  firstMsg: boolean;
  reply: { user: string; text: string } | null;
  rewardId: string | null;
  msgId: string | null;
  reward?: RewardInfo;
  bits?: number;
  deleted?: boolean;
  returning?: boolean;
  pin?: PinInfo;
};

export type Notice = {
  id: string;
  ts: number;
  system: string;
  msgId: string;
  sourceMsgId?: string;
  giftId?: string;
  giftCount?: number;
  reward?: RewardInfo;
  user: ChatUser;
  text: string;
  emotes?: string;
};

export type ModFeedEntry = { ts: number; feedKind: "mode" | "timeout" | "ban" | "clear" | "delete"; text: string };

export type HeldMsg = {
  id: string;
  ts: number;
  user: { id: string; login: string; name: string };
  text: string;
  reason: "automod" | "blocked_term";
  category: string | null;
  level: number | null;
  status: "held" | "approved" | "denied" | "expired";
  resolvedBy: string | null;
};

export type SuspiciousUser = {
  id: string;
  login: string;
  name: string;
  status: "active_monitoring" | "restricted";
  types: string[];
  evaluation: string | null;
  sharedBanChannels: number;
};

export type EsubStatus = { active: boolean; reason: null | "connecting" | "relogin" | "error" };

// ---- co-chats ---------------------------------------------------------------------------------

export type RumbleUser = { id: string; login: string; name: string; color: string; avatar: string };
export type RumbleMsg = {
  platform: "rumble";
  id: string;
  ts: number;
  user: RumbleUser;
  badges: string[];
  text: string;
  emotes: string;
  action: boolean;
  firstMsg: boolean;
  reply: null;
  isFollower: boolean;
  channel: { id: string; name: string; avatar: string } | null;
  rant: { cents: number; duration: number; expiresAt: number } | null;
  notice: string | null;
  deleted?: boolean;
};
export type RumbleBadgeDefs = Record<string, { icons?: Record<string, string>; label?: Record<string, string> }>;
export type RumbleEmotes = Record<string, { file: string; subsOnly?: boolean }>;

export type KickUser = { id: string; login: string; name: string; color: string; avatar: string; verified: boolean; anonymous: boolean };
export type KickBadge = { type: string; text: string; count: number; url?: string };
export type KickMsg = {
  platform: "kick";
  id: string;
  ts: number;
  user: KickUser;
  badges: KickBadge[];
  text: string;
  reply: { id: string; text: string; name: string } | null;
  action: boolean;
  notice: string | null;
  deleted?: boolean;
};
export type KickEmotes = Record<string, { id: string; group: string; subsOnly?: boolean }>;

export type YouTubePart = { t: "text"; v: string } | { t: "emoji"; u: string; a: string; custom?: boolean };
export type YouTubeBadge = { label: string; icon: string; url: string };
export type YouTubeMsg = {
  platform: "youtube";
  id: string;
  ts: number;
  user: { id: string; name: string; avatar: string };
  badges: YouTubeBadge[];
  parts: YouTubePart[];
  text: string;
  paid: { amount: string; bg: string; headerBg: string; sticker: boolean } | null;
  notice: string | null;
};

export type XMsg = {
  platform: "x";
  id: string;
  ts: number;
  user: { id: string; name: string; login: string; avatar: string; twitterId: string };
  text: string;
};

// ---- the merged list --------------------------------------------------------------------------

export type RowBody =
  | { k: "msg"; m: ChatMsg }
  | { k: "notice"; n: Notice }
  | { k: "modfeed"; f: ModFeedEntry }
  | { k: "automod"; h: HeldMsg }
  | { k: "rumble"; m: RumbleMsg }
  | { k: "kick"; m: KickMsg }
  | { k: "youtube"; m: YouTubeMsg }
  | { k: "x"; m: XMsg };

/** One line of the message list. `sk` is the source key `${platform}:${channel}` as given in props. */
export type Row = {
  key: string;
  ts: number;
  sk: string;
  platform: Platform;
  /** arrival counter, stable under trimming: alternating row shading keys off it */
  ord?: number;
  /** came in with a history / snapshot batch (chatlog), not live: never alerts or marks a tab */
  backlog?: boolean;
} & RowBody;

/** The person behind a row, for the user card and the "recent messages" list. */
export type RowUser = { id: string; login: string; name: string; avatar?: string; color?: string };
