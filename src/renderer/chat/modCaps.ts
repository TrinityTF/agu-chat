// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): ModCaps / fetchModCaps /
// fetchChatRules / modReach, the timeout ladder, plus the Rumble and Kick caps hooks' fetches
// (RumbleChat.tsx useRumbleCaps, KickChat.tsx useKickCaps). Changes: every call goes through
// relayApi(source, ...); the caps answer is not cached for the page's life (the account can log in later,
// callers refetch when the account changes) - main caches what is worth caching.
import { relayApi } from "../bridge";
import type { SourceRef } from "../protocol";

export type ModCaps = {
  pin: boolean;
  ban: boolean;
  warn: boolean;
  delete: boolean;
  send: boolean;
  modes: boolean;
  announce: boolean;
  shoutout: boolean;
  shield: boolean;
  terms: boolean;
  raid: boolean;
  // moderator:manage:automod - gates the AutoMod Allow/Deny buttons.
  automod: boolean;
  /** From /api/mod/caps of the local relay: the account is logged in / is this channel's broadcaster / a mod. */
  loggedIn?: boolean;
  isBroadcaster?: boolean;
  isMod?: boolean;
};

export const ALL_FALSE_CAPS: ModCaps = {
  pin: false,
  ban: false,
  warn: false,
  delete: false,
  send: false,
  modes: false,
  announce: false,
  shoutout: false,
  shield: false,
  terms: false,
  raid: false,
  automod: false,
};

export async function fetchModCaps(source: SourceRef): Promise<ModCaps> {
  try {
    const r = await relayApi(source, "/api/mod/caps");
    if (!r.ok) return { ...ALL_FALSE_CAPS };
    const data = await r.json();
    return {
      pin: !!data?.pin,
      ban: !!data?.ban,
      warn: !!data?.warn,
      delete: !!data?.delete,
      send: !!data?.send,
      modes: !!data?.modes,
      announce: !!data?.announce,
      shoutout: !!data?.shoutout,
      shield: !!data?.shield,
      terms: !!data?.terms,
      raid: !!data?.raid,
      automod: !!data?.automod,
      loggedIn: data?.loggedIn,
      isBroadcaster: data?.isBroadcaster,
      isMod: data?.isMod,
    };
  } catch {
    return { ...ALL_FALSE_CAPS };
  }
}

/** Whether the acting user can moderate this Rumble / Kick chat (/api/rumble/caps, /api/kick/caps). */
export type CoCaps = { connected: boolean; canModerate: boolean; canDelete: boolean };
export const NO_CO_CAPS: CoCaps = { connected: false, canModerate: false, canDelete: false };

export async function fetchCoCaps(source: SourceRef): Promise<CoCaps> {
  const path = source.platform === "rumble" ? "/api/rumble/caps" : source.platform === "kick" ? "/api/kick/caps" : "";
  if (!path) return NO_CO_CAPS;
  try {
    const r = await relayApi(source, path);
    if (!r.ok) return NO_CO_CAPS;
    const d = await r.json();
    return {
      connected: !!d?.connected,
      canModerate: !!d?.canModerate,
      // Rumble deletes with the same right as bans; Kick reports canDelete separately.
      canDelete: source.platform === "kick" ? !!d?.canDelete : !!d?.canModerate,
    };
  } catch {
    return NO_CO_CAPS;
  }
}

// Channel chat rules for the Warn panel, cached per source.
const rulesCache = new Map<string, Promise<string[]>>();

export function fetchChatRules(source: SourceRef): Promise<string[]> {
  const key = `${source.platform}:${source.channel}`;
  const cached = rulesCache.get(key);
  if (cached) return cached;
  const p = (async (): Promise<string[]> => {
    try {
      const r = await relayApi(source, "/api/chat/rules");
      if (!r.ok) {
        rulesCache.delete(key);
        return [];
      }
      const data = await r.json();
      return Array.isArray(data?.rules) ? data.rules : [];
    } catch {
      rulesCache.delete(key);
      return [];
    }
  })();
  rulesCache.set(key, p);
  return p;
}

export const TIMEOUT_LADDER: { label: string; sec: number }[] = [
  { label: "1m", sec: 60 },
  { label: "5m", sec: 300 },
  { label: "10m", sec: 600 },
  { label: "30m", sec: 1800 },
  { label: "1h", sec: 3600 },
  { label: "12h", sec: 43200 },
  { label: "24h", sec: 86400 },
  { label: "1w", sec: 604800 },
  { label: "2w", sec: 1209600 },
];

// Who's looking, for modReach: their user id, and whether they're this chat's broadcaster or one of its
// lead moderators.
export type ModViewer = { id: string; broadcaster: boolean; lead: boolean };
export type ModReach = { ban: boolean; timeout: boolean; warn: boolean; delete: boolean };

export function badgeNames(tag: string): Set<string> {
  return new Set(
    (tag || "")
      .split(",")
      .map((b) => b.split("/")[0])
      .filter(Boolean),
  );
}

// What the viewer may do to a chatter, by Twitch's rules: nobody acts on the broadcaster; a mod can't act
// on another mod, but the broadcaster can, and a lead moderator can on ordinary mods. Nobody bans
// themselves, but they can time out or warn themselves. The viewer's mod rights (ModCaps) are checked
// separately; this only rules out targets.
export function modReach(viewer: ModViewer, target: { id: string; badges: string }): ModReach {
  const b = badgeNames(target.badges);
  const self = !!viewer.id && viewer.id === target.id;
  if (b.has("broadcaster")) return { ban: false, timeout: false, warn: false, delete: self };
  if (self) return { ban: false, timeout: true, warn: true, delete: true };
  const lead = b.has("lead_moderator");
  const ok = !(lead || b.has("moderator")) || viewer.broadcaster || (viewer.lead && !lead);
  return { ban: ok, timeout: ok, warn: ok, delete: ok };
}

export const REACH_ALL: ModReach = { ban: true, timeout: true, warn: true, delete: true };
