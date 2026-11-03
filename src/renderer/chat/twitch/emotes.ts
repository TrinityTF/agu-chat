// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: split into its own
// module, types/functions exported, the Twitch picker-emote fetch takes the source and goes through
// relayApi(source, ...). 7TV / BTTV / FFZ stay direct browser fetches (CORS-open public APIs).
import { relayApi } from "../../bridge";
import type { SourceRef } from "../../protocol";

export type SizeOpt = "small" | "medium" | "large";

export type EmoteProvider = "7tv" | "bttv" | "ffz";

export type ProviderEmote = {
  provider: EmoteProvider;
  id: string;
  name: string;
  zeroWidth: boolean;
  animated: boolean;
  // Absolute CDN URLs keyed by density; providers don't all offer the same
  // set (BTTV has no 4x, FFZ has no 3x) — pickers fall back sensibly.
  urls: Partial<Record<"1" | "2" | "3" | "4", string>>;
};

export type ProviderScopedMap = { global: Map<string, ProviderEmote>; channel: Map<string, ProviderEmote> };

export type EmoteSets = {
  channel: Map<string, ProviderEmote>; // merged, highest-priority-per-name, channel scope only
  global: Map<string, ProviderEmote>; // merged, global scope only
  all: Map<string, ProviderEmote>; // final lookup: channel overrides global
};

const DENSITY_FOR_SIZE: Record<SizeOpt, "1" | "2" | "3"> = { small: "1", medium: "2", large: "3" };

export function providerEmoteUrl(e: ProviderEmote, size: SizeOpt): string {
  const want = DENSITY_FOR_SIZE[size];
  return e.urls[want] || e.urls["2"] || e.urls["1"] || e.urls["3"] || e.urls["4"] || "";
}

export function providerTooltipUrl(e: ProviderEmote): string {
  return e.urls["4"] || e.urls["3"] || e.urls["2"] || e.urls["1"] || "";
}

export const PROVIDER_LABEL: Record<EmoteProvider, string> = { "7tv": "7TV", bttv: "BetterTTV", ffz: "FrankerFaceZ" };

// ---------------------------------------------------------------------------
// localStorage cache for provider emote-set/cosmetics JSON responses — key
// `emotes:<provider>:<scope>:<channelId|global>`, value {v:1, at, data}.
// Serves cached data immediately while fresh; once stale it refetches and
// overwrites (no stale-while-refetch). Failed/non-ok fetches are never
// cached so they retry next time. Guards JSON.parse and quota errors —
// any storage hiccup just falls through to a network fetch.
// ---------------------------------------------------------------------------

const EMOTE_CHANNEL_TTL_MS = 3600_000;
const EMOTE_GLOBAL_TTL_MS = 6 * 3600_000;

export function readLsCache<T>(key: string, ttlMs: number): T | null {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return null;
    const parsed = JSON.parse(raw);
    if (!parsed || parsed.v !== 1 || typeof parsed.at !== "number") return null;
    if (Date.now() - parsed.at >= ttlMs) return null;
    return parsed.data as T;
  } catch {
    return null;
  }
}

export function writeLsCache(key: string, data: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify({ v: 1, at: Date.now(), data }));
  } catch {
    /* ignore quota errors */
  }
}

async function fetchJsonCached(key: string, ttlMs: number, url: string): Promise<any | null> {
  const cached = readLsCache<any>(key, ttlMs);
  if (cached !== null) return cached;
  try {
    const r = await fetch(url);
    if (!r.ok) return null;
    const data = await r.json();
    writeLsCache(key, data);
    return data;
  } catch {
    return null;
  }
}

// -- 7TV --------------------------------------------------------------------

function toSevenTvMap(emotes: any[]): Map<string, ProviderEmote> {
  const m = new Map<string, ProviderEmote>();
  for (const item of emotes || []) {
    const flags = item.flags || 0;
    const dataFlags = item.data?.flags || 0;
    const zeroWidth = !!(flags & 1 || dataFlags & 256);
    m.set(item.name, {
      provider: "7tv",
      id: item.id,
      name: item.name,
      zeroWidth,
      animated: !!item.data?.animated,
      urls: {
        "1": `https://cdn.7tv.app/emote/${item.id}/1x.webp`,
        "2": `https://cdn.7tv.app/emote/${item.id}/2x.webp`,
        "3": `https://cdn.7tv.app/emote/${item.id}/3x.webp`,
        "4": `https://cdn.7tv.app/emote/${item.id}/4x.webp`,
      },
    });
  }
  return m;
}

const sevenTvCache = new Map<string, Promise<ProviderScopedMap>>();

export async function fetchSevenTv(channelId: string): Promise<ProviderScopedMap> {
  const cached = sevenTvCache.get(channelId);
  if (cached) return cached;
  const p = (async (): Promise<ProviderScopedMap> => {
    const [globalData, channelData] = await Promise.all([
      fetchJsonCached("emotes:7tv:global:global", EMOTE_GLOBAL_TTL_MS, "https://7tv.io/v3/emote-sets/global"),
      fetchJsonCached(`emotes:7tv:channel:${channelId}`, EMOTE_CHANNEL_TTL_MS, `https://7tv.io/v3/users/twitch/${channelId}`),
    ]);
    const global = globalData ? toSevenTvMap(globalData.emotes) : new Map();
    const channel = channelData ? toSevenTvMap(channelData.emote_set?.emotes) : new Map();
    return { global, channel };
  })();
  sevenTvCache.set(channelId, p);
  return p;
}

// -- BTTV ---------------------------------------------------------------

// The extension hardcodes this list (plus modifier===true) as zero-width.
const BTTV_ZW_NAMES = new Set(["SoSnowy", "IceCold", "SantaHat", "TopHat", "ReinDeer", "CandyCane", "cvMask", "cvHazmat"]);

function bttvEmoteToProvider(e: any): ProviderEmote {
  const zeroWidth = e.modifier === true || BTTV_ZW_NAMES.has(e.code);
  return {
    provider: "bttv",
    id: e.id,
    name: e.code,
    zeroWidth,
    animated: e.imageType === "gif",
    urls: {
      "1": `https://cdn.betterttv.net/emote/${e.id}/1x`,
      "2": `https://cdn.betterttv.net/emote/${e.id}/2x`,
      "3": `https://cdn.betterttv.net/emote/${e.id}/3x`,
    },
  };
}

const bttvCache = new Map<string, Promise<ProviderScopedMap>>();

export async function fetchBttv(channelId: string): Promise<ProviderScopedMap> {
  const cached = bttvCache.get(channelId);
  if (cached) return cached;
  const p = (async (): Promise<ProviderScopedMap> => {
    const [globalData, channelData] = await Promise.all([
      fetchJsonCached("emotes:bttv:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.betterttv.net/3/cached/emotes/global"),
      fetchJsonCached(
        `emotes:bttv:channel:${channelId}`,
        EMOTE_CHANNEL_TTL_MS,
        `https://api.betterttv.net/3/cached/users/twitch/${channelId}`,
      ),
    ]);
    const global = new Map<string, ProviderEmote>();
    if (Array.isArray(globalData)) {
      for (const e of globalData) global.set(e.code, bttvEmoteToProvider(e));
    }
    const channel = new Map<string, ProviderEmote>();
    if (channelData) {
      for (const e of [...(channelData.channelEmotes || []), ...(channelData.sharedEmotes || [])])
        channel.set(e.code, bttvEmoteToProvider(e));
    }
    return { global, channel };
  })();
  bttvCache.set(channelId, p);
  return p;
}

// -- FFZ ------------------------------------------------------------------

function ffzEmoteToProvider(e: any): ProviderEmote {
  const urls: Partial<Record<"1" | "2" | "3" | "4", string>> = {};
  for (const k of ["1", "2", "4"] as const) {
    const u = e.urls?.[k];
    if (u) urls[k] = typeof u === "string" && u.startsWith("//") ? "https:" + u : u;
  }
  const zeroWidth = !!(e.modifier === true || (e.modifier_flags && e.modifier_flags > 0));
  return { provider: "ffz", id: String(e.id), name: e.name, zeroWidth, animated: false, urls };
}

const ffzCache = new Map<string, Promise<ProviderScopedMap>>();

export async function fetchFfz(channelId: string): Promise<ProviderScopedMap> {
  const cached = ffzCache.get(channelId);
  if (cached) return cached;
  const p = (async (): Promise<ProviderScopedMap> => {
    const [globalData, roomData] = await Promise.all([
      fetchJsonCached("emotes:ffz:global:global", EMOTE_GLOBAL_TTL_MS, "https://api.frankerfacez.com/v1/set/global"),
      fetchJsonCached(`emotes:ffz:channel:${channelId}`, EMOTE_CHANNEL_TTL_MS, `https://api.frankerfacez.com/v1/room/id/${channelId}`),
    ]);
    const global = new Map<string, ProviderEmote>();
    if (globalData) {
      for (const setId of globalData.default_sets || []) {
        const set = globalData.sets?.[setId];
        for (const e of set?.emoticons || []) global.set(e.name, ffzEmoteToProvider(e));
      }
    }
    const channel = new Map<string, ProviderEmote>();
    if (roomData) {
      const setId = roomData.room?.set;
      const set = setId != null ? roomData.sets?.[setId] : null;
      for (const e of set?.emoticons || []) channel.set(e.name, ffzEmoteToProvider(e));
    }
    return { global, channel };
  })();
  ffzCache.set(channelId, p);
  return p;
}

// -- merge --------------------------------------------------------------

// Priority high -> low: 7TV-channel, BTTV-channel, FFZ-channel, 7TV-global,
// BTTV-global, FFZ-global. Built by inserting low -> high so later writes
// (higher priority) win; channel entries are then layered over global ones
// unconditionally, so any channel emote beats any global emote regardless
// of provider.
export function mergeEmoteSets(seven: ProviderScopedMap, bttv: ProviderScopedMap, ffz: ProviderScopedMap): EmoteSets {
  const channel = new Map<string, ProviderEmote>();
  ffz.channel.forEach((v, k) => channel.set(k, v));
  bttv.channel.forEach((v, k) => channel.set(k, v));
  seven.channel.forEach((v, k) => channel.set(k, v));

  const global = new Map<string, ProviderEmote>();
  ffz.global.forEach((v, k) => global.set(k, v));
  bttv.global.forEach((v, k) => global.set(k, v));
  seven.global.forEach((v, k) => global.set(k, v));

  const all = new Map<string, ProviderEmote>();
  global.forEach((v, k) => all.set(k, v));
  channel.forEach((v, k) => all.set(k, v));

  return { channel, global, all };
}

export async function fetchEmoteSets(channelId: string): Promise<EmoteSets> {
  const [seven, bttv, ffz] = await Promise.all([fetchSevenTv(channelId), fetchBttv(channelId), fetchFfz(channelId)]);
  return mergeEmoteSets(seven, bttv, ffz);
}

// -- Twitch picker-only emotes (CHAT v4 item 1) ------------------------------
// Picker/autocomplete ONLY — never merged into the message-rendering map;
// incoming messages already carry Twitch emote positions from the IRC tag.

export type TwitchPickerEmote = { id: string; name: string; url: string };
// `other` = emotes from the user's subscriptions to other channels (local relay only).
export type TwitchEmotesResp = { channel: TwitchPickerEmote[]; global: TwitchPickerEmote[]; other?: TwitchPickerEmote[] };
export const EMPTY_TWITCH_EMOTES: TwitchEmotesResp = { channel: [], global: [] };

export async function fetchTwitchPickerEmotes(source: SourceRef, channelId: string): Promise<TwitchEmotesResp> {
  const key = `emotes:twitch:channel:${channelId}`;
  const cached = readLsCache<TwitchEmotesResp>(key, EMOTE_GLOBAL_TTL_MS);
  if (cached) return cached;
  try {
    const r = await relayApi(source, "/api/chat/twitch-emotes");
    if (!r.ok) return { ...EMPTY_TWITCH_EMOTES };
    const data = await r.json();
    const resp: TwitchEmotesResp = {
      channel: Array.isArray(data?.channel) ? data.channel : [],
      global: Array.isArray(data?.global) ? data.global : [],
      other: Array.isArray(data?.other) ? data.other : [],
    };
    writeLsCache(key, resp);
    return resp;
  } catch {
    return { ...EMPTY_TWITCH_EMOTES };
  }
}

// -- 7TV personal emote set (CHAT v4 item 1) ---------------------------------
// Best-effort, three chained lookups from the ACTING user's Twitch id; any
// missing step just omits the "7TV Personal" section silently.

function toSevenTvList(emotes: any[]): ProviderEmote[] {
  return Array.from(toSevenTvMap(emotes).values());
}

const personalCache = new Map<string, Promise<ProviderEmote[]>>();

export async function fetchSevenTvPersonal(twitchUserId: string): Promise<ProviderEmote[]> {
  const cached = personalCache.get(twitchUserId);
  if (cached) return cached;
  const p = (async (): Promise<ProviderEmote[]> => {
    try {
      const idKey = `emotes:7tv:personal-id:${twitchUserId}`;
      let sevenTvId = readLsCache<string>(idKey, EMOTE_GLOBAL_TTL_MS);
      if (sevenTvId === null) {
        const r = await fetch(`https://7tv.io/v3/users/twitch/${twitchUserId}`);
        if (!r.ok) return [];
        const data = await r.json();
        sevenTvId = data?.user?.id || "";
        writeLsCache(idKey, sevenTvId);
      }
      if (!sevenTvId) return [];

      const setKey = `emotes:7tv:personal-set:${sevenTvId}`;
      let setId = readLsCache<string>(setKey, EMOTE_GLOBAL_TTL_MS);
      if (setId === null) {
        const r2 = await fetch(`https://7tv.io/v3/users/${sevenTvId}`);
        if (!r2.ok) return [];
        const data2 = await r2.json();
        const sets: any[] = data2?.emote_sets || [];
        const personal = sets.find((s) => (s.flags & 4) === 4 || /personal/i.test(s.name || ""));
        setId = personal?.id || "";
        writeLsCache(setKey, setId);
      }
      if (!setId) return [];

      const emotesKey = `emotes:7tv:personal-emotes:${setId}`;
      const cachedEmotes = readLsCache<any[]>(emotesKey, EMOTE_CHANNEL_TTL_MS);
      if (cachedEmotes) return toSevenTvList(cachedEmotes);
      const r3 = await fetch(`https://7tv.io/v3/emote-sets/${setId}`);
      if (!r3.ok) return [];
      const data3 = await r3.json();
      const list = data3?.emotes || [];
      writeLsCache(emotesKey, list);
      return toSevenTvList(list);
    } catch {
      return [];
    }
  })();
  personalCache.set(twitchUserId, p);
  return p;
}
