// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: split into its own
// module (pure); the per-user "Show in chat" event toggles are the site's defaults (the app has no
// setting for them in this beta); `eventVisible` takes a Row-shaped notice.
export type ChatEventKind =
  | "redeems"
  | "cheers"
  | "subs"
  | "gifts"
  | "raids"
  | "streaks"
  | "announcements"
  | "bitsBadges"
  | "charity"
  | "combos"
  | "sharedRewards";
export type ChatEventSettings = Record<ChatEventKind, boolean> & {
  // Fold a gift bomb's per-recipient lines under its "is gifting N subs" line.
  groupGifts: boolean;
};

export const DEFAULT_EVENTS: ChatEventSettings = {
  redeems: true,
  cheers: true,
  subs: true,
  gifts: true,
  raids: true,
  streaks: true,
  announcements: false,
  bitsBadges: false,
  charity: false,
  combos: false,
  sharedRewards: false,
  groupGifts: true,
};

// Twitch USERNOTICE msg-ids by the item that governs them. Anything not
// listed never shows.
export const NOTICE_KIND: Record<string, ChatEventKind> = {
  redeem: "redeems",
  powerup: "cheers",
  sub: "subs",
  resub: "subs",
  extendsub: "subs",
  primepaidupgrade: "subs",
  giftpaidupgrade: "subs",
  anongiftpaidupgrade: "subs",
  standardpayforward: "subs",
  communitypayforward: "subs",
  subgift: "gifts",
  anonsubgift: "gifts",
  submysterygift: "gifts",
  anonsubmysterygift: "gifts",
  raid: "raids",
  unraid: "raids",
  announcement: "announcements",
  viewermilestone: "streaks",
  bitsbadgetier: "bitsBadges",
  charitydonation: "charity",
  onetapgiftredeemed: "combos",
  rewardgift: "sharedRewards",
};

export function noticeKind(n: { msgId?: string | null; sourceMsgId?: string }): ChatEventKind | null {
  return NOTICE_KIND[n.sourceMsgId || n.msgId || ""] || null;
}

export const kindOn = (ev: ChatEventSettings, k: ChatEventKind | null) => !!k && ev[k];

// Twitch's own redeems, which arrive as a msg-id on an ordinary message rather than a custom-reward-id.
// Highlight My Message costs channel points; the other two are Bits Power-ups. Twitch doesn't say what
// these cost, so they show no price.
export const BUILTIN_REDEEMS: Record<string, { title: string; bits: boolean }> = {
  "highlighted-message": { title: "Highlight My Message", bits: false },
  "gigantified-emote-message": { title: "Gigantify an Emote", bits: true },
  "animated-message": { title: "Message Effects", bits: true },
};

// Whether a Twitch notice should be on screen under the event toggles (port of eventVisible). `bombs` is
// the set of gift-bomb ids already showing their summary line: their per-recipient lines fold under it.
export function noticeVisible(n: { msgId?: string | null; sourceMsgId?: string; giftId?: string }, bombs: Set<string>, ev: ChatEventSettings = DEFAULT_EVENTS): boolean {
  if (!kindOn(ev, noticeKind(n))) return false;
  const id = n.sourceMsgId || n.msgId;
  if (ev.groupGifts && n.giftId && (id === "subgift" || id === "anonsubgift") && bombs.has(n.giftId)) return false;
  return true;
}
