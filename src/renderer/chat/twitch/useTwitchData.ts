// Loads, per Twitch source of a split, everything a row renders against: badges, 7TV/BTTV/FFZ channel +
// global emotes, Twitch's own picker emotes, the account's mod caps, plus the account's 7TV personal set
// and 7TV cosmetic definitions. Ported from the data effects at the top of Chat() in
// modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: keyed by source (a split can hold
// several Twitch channels), every relay call is relayApi(source, ...), the badges cache is
// per source, caps reload when the logged-in account changes.
import { useEffect, useRef, useState } from "react";
import { relayApi } from "../../bridge";
import type { SourceRef } from "../../protocol";
import { ALL_FALSE_CAPS, fetchModCaps, type ModCaps } from "../modCaps";
import { buildBadgeMap, type BadgeMap, type BadgesResp } from "./badges";
import { fetchSevenTvDefs, type SevenTvDefs } from "./cosmetics";
import {
  EMPTY_TWITCH_EMOTES,
  fetchBttv,
  fetchFfz,
  fetchSevenTv,
  fetchSevenTvPersonal,
  fetchTwitchPickerEmotes,
  type ProviderEmote,
  type ProviderScopedMap,
  type TwitchEmotesResp,
} from "./emotes";

export type ProviderSets = { seven: ProviderScopedMap; bttv: ProviderScopedMap; ffz: ProviderScopedMap };

export type TwitchData = {
  badgeMap: BadgeMap;
  providerSets: ProviderSets | null;
  twitchEmotes: TwitchEmotesResp;
  caps: ModCaps;
};

export const EMPTY_TWITCH_DATA: TwitchData = { badgeMap: new Map(), providerSets: null, twitchEmotes: EMPTY_TWITCH_EMOTES, caps: ALL_FALSE_CAPS };

export type TwitchSourceInput = { sk: string; ref: SourceRef; channelId: string | null };

export function useTwitchData(
  inputs: TwitchSourceInput[],
  account: { id: string; connected: boolean },
  onChannelId: (sk: string, id: string) => void,
): { data: Record<string, TwitchData>; personal: ProviderEmote[]; sevenDefs: SevenTvDefs | null } {
  const [data, setData] = useState<Record<string, TwitchData>>({});
  const [personal, setPersonal] = useState<ProviderEmote[]>([]);
  const [sevenDefs, setSevenDefs] = useState<SevenTvDefs | null>(null);
  const inputsRef = useRef(inputs);
  inputsRef.current = inputs;
  const onChannelIdRef = useRef(onChannelId);
  onChannelIdRef.current = onChannelId;

  const patch = (sk: string, p: Partial<TwitchData>) => setData((prev) => ({ ...prev, [sk]: { ...(prev[sk] || EMPTY_TWITCH_DATA), ...p } }));

  const skSig = inputs.map((i) => i.sk).join("\u0001");
  const idSig = inputs.map((i) => `${i.sk}=${i.channelId || ""}`).join("\u0001");
  const acctSig = `${account.connected ? 1 : 0}:${account.id}`;

  // Badges (cached in sessionStorage per source, 1h TTL, like the site). The answer may also tell us the
  // channel id (7TV/BTTV/FFZ are keyed by it) when no frame did.
  useEffect(() => {
    let dead = false;
    for (const { sk, ref } of inputsRef.current) {
      const key = `chatBadges:${sk}`;
      try {
        const cached = sessionStorage.getItem(key);
        if (cached) {
          const parsed = JSON.parse(cached);
          if (parsed && Date.now() - parsed.ts < 3600_000) {
            patch(sk, { badgeMap: buildBadgeMap(parsed.data as BadgesResp) });
            if (parsed.data?.channelId) onChannelIdRef.current(sk, String(parsed.data.channelId));
            continue;
          }
        }
      } catch {
        /* ignore corrupt cache */
      }
      relayApi(ref, "/api/chat/badges")
        .then(async (r) => {
          if (dead || !r.ok) return;
          const d = await r.json();
          if (!d) return;
          try {
            sessionStorage.setItem(key, JSON.stringify({ ts: Date.now(), data: d }));
          } catch {
            /* quota */
          }
          if (!dead) patch(sk, { badgeMap: buildBadgeMap(d as BadgesResp) });
          if (!dead && d.channelId) onChannelIdRef.current(sk, String(d.channelId));
        })
        .catch(() => {});
    }
    return () => {
      dead = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [skSig]);

  // 7TV + BTTV + FFZ, kept raw per provider so the pane can apply settings.sevenTv/bttv/ffz and the picker
  // can categorize.
  useEffect(() => {
    let dead = false;
    for (const { sk, channelId } of inputsRef.current) {
      if (!channelId) continue;
      Promise.all([fetchSevenTv(channelId), fetchBttv(channelId), fetchFfz(channelId)])
        .then(([seven, bttv, ffz]) => {
          if (!dead) patch(sk, { providerSets: { seven, bttv, ffz } });
        })
        .catch(() => {});
    }
    return () => {
      dead = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idSig]);

  // Twitch's own emote lists (picker / autocomplete only).
  useEffect(() => {
    let dead = false;
    for (const { sk, ref, channelId } of inputsRef.current) {
      fetchTwitchPickerEmotes(ref, channelId || sk).then((r) => {
        if (!dead) patch(sk, { twitchEmotes: r });
      });
    }
    return () => {
      dead = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [idSig]);

  // Mod capabilities: gate pins, inline mod buttons, AutoMod Allow/Deny, slash-command dimming. Refetched
  // when the logged-in account changes.
  useEffect(() => {
    let dead = false;
    for (const { sk, ref } of inputsRef.current) {
      fetchModCaps(ref).then((c) => {
        if (!dead) patch(sk, { caps: c });
      });
    }
    return () => {
      dead = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [skSig, acctSig]);

  // The ACTING user's 7TV personal emote set.
  useEffect(() => {
    if (!account.id) {
      setPersonal([]);
      return;
    }
    let dead = false;
    fetchSevenTvPersonal(account.id).then((r) => {
      if (!dead) setPersonal(r);
    });
    return () => {
      dead = true;
    };
  }, [account.id]);

  // 7TV cosmetic definitions (paints/badges): a cheap single cached fetch.
  useEffect(() => {
    let dead = false;
    fetchSevenTvDefs().then((d) => {
      if (!dead) setSevenDefs(d);
    });
    return () => {
      dead = true;
    };
  }, []);

  return { data, personal, sevenDefs };
}
