// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): the `emoteInventory` useMemo
// (categorized picker sections + the flattened autocomplete inventory) and SendBox's rumbleSection /
// kickSection, as pure builders. Changes: split out, takes the pane's already-filtered provider sets
// (settings.sevenTv / bttv / ffz applied by the caller).
import { providerEmoteUrl, type ProviderEmote, type ProviderScopedMap, type TwitchEmotesResp, type TwitchPickerEmote } from "../twitch/emotes";
import type { KickEmotes, RumbleEmotes } from "../types";
import type { InvEntry } from "./autocomplete";
import type { PickerItem, PickerSection } from "./EmotePicker";
import { kickEmoteUrl } from "../rows/CoChatRows";

export type Inventory = { sections: PickerSection[]; inventory: InvEntry[] };
export const EMPTY_INVENTORY: Inventory = { sections: [], inventory: [] };

export function buildTwitchInventory(args: {
  providerSets: { seven: ProviderScopedMap; bttv: ProviderScopedMap; ffz: ProviderScopedMap } | null;
  twitchEmotes: TwitchEmotesResp;
  personal: ProviderEmote[];
}): Inventory {
  const { providerSets, twitchEmotes, personal } = args;
  const sections: PickerSection[] = [];
  const invMap = new Map<string, InvEntry>();

  const addSection = (id: string, label: string, provider: PickerSection["provider"], items: PickerItem[]) => {
    if (items.length) sections.push({ id, label, provider, items });
  };
  const addInv = (list: PickerItem[], label: string, channelScope: boolean) => {
    for (const it of list) invMap.set(it.name, { name: it.name, img: it.img, label, channelScope });
  };
  const toItems = (list: ProviderEmote[], tag: string): PickerItem[] =>
    list.map((e) => ({ key: `${e.provider}:${e.id}`, name: e.name, img: providerEmoteUrl(e, "small"), tooltip: `${e.name} (${tag})` }));

  // Twitch's own emotes first: they are what most messages use.
  const twitchItem = (e: TwitchPickerEmote): PickerItem => ({ key: `tw:${e.id}`, name: e.name, img: e.url, tooltip: `${e.name} (Twitch)` });
  const twitchChannelItems = twitchEmotes.channel.map(twitchItem);
  const twitchGlobalItems = twitchEmotes.global.map(twitchItem);
  addSection("twitch-channel", "Twitch Channel", "twitch", twitchChannelItems);
  addSection("twitch-global", "Twitch Global", "twitch", twitchGlobalItems);
  const twitchOtherItems = (twitchEmotes.other || []).map(twitchItem);
  addSection("twitch-other", "Twitch Subscriptions", "twitch", twitchOtherItems);

  const personalItems = personal.map((e) => ({ key: `p:${e.id}`, name: e.name, img: providerEmoteUrl(e, "small"), tooltip: `${e.name} (7TV Personal)` }));
  addSection("7tv-personal", "7TV Personal", "7tv", personalItems);

  if (providerSets) {
    const sevenChannelAll = Array.from(providerSets.seven.channel.values());
    const sevenChannel = sevenChannelAll.filter((e) => !e.zeroWidth);
    const sevenChannelZw = sevenChannelAll.filter((e) => e.zeroWidth);
    const sevenGlobalAll = Array.from(providerSets.seven.global.values());
    const sevenGlobal = sevenGlobalAll.filter((e) => !e.zeroWidth);
    const sevenGlobalZw = sevenGlobalAll.filter((e) => e.zeroWidth);
    const bttvChannel = Array.from(providerSets.bttv.channel.values());
    const bttvGlobal = Array.from(providerSets.bttv.global.values());
    const ffzChannel = Array.from(providerSets.ffz.channel.values());
    const ffzGlobal = Array.from(providerSets.ffz.global.values());

    addSection("7tv-channel", "7TV Channel", "7tv", toItems(sevenChannel, "7TV"));
    addSection("7tv-zerowidth", "7TV Zero-Width", "7tv", toItems([...sevenChannelZw, ...sevenGlobalZw], "7TV zero-width"));
    addSection("7tv-global", "7TV Global", "7tv", toItems(sevenGlobal, "7TV"));
    addSection("bttv-channel", "BetterTTV Channel", "bttv", toItems(bttvChannel, "BetterTTV"));
    addSection("bttv-global", "BetterTTV Global", "bttv", toItems(bttvGlobal, "BetterTTV"));
    addSection("ffz-channel", "FrankerFaceZ Channel", "ffz", toItems(ffzChannel, "FrankerFaceZ"));
    addSection("ffz-global", "FrankerFaceZ Global", "ffz", toItems(ffzGlobal, "FrankerFaceZ"));

    // Lowest priority first so a later (higher priority) write wins the name, like mergeEmoteSets.
    addInv(toItems(ffzGlobal, "FrankerFaceZ"), "FrankerFaceZ", false);
    addInv(toItems(bttvGlobal, "BetterTTV"), "BetterTTV", false);
    addInv(toItems(sevenGlobal, "7TV"), "7TV", false);
    addInv(toItems([...sevenGlobalZw], "7TV zero-width"), "7TV zero-width", false);
    addInv(twitchGlobalItems, "Twitch", false);
    addInv(toItems(ffzChannel, "FrankerFaceZ"), "FrankerFaceZ", true);
    addInv(toItems(bttvChannel, "BetterTTV"), "BetterTTV", true);
    addInv(toItems(sevenChannel, "7TV"), "7TV", true);
    addInv(toItems(sevenChannelZw, "7TV zero-width"), "7TV zero-width", true);
  } else {
    addInv(twitchGlobalItems, "Twitch", false);
  }
  addInv(twitchOtherItems, "Twitch", true);
  addInv(twitchChannelItems, "Twitch", true);
  addInv(personalItems, "7TV Personal", true);

  return { sections, inventory: Array.from(invMap.values()) };
}

export function buildRumbleInventory(emotes: RumbleEmotes): Inventory {
  const names = Object.keys(emotes);
  if (!names.length) return EMPTY_INVENTORY;
  const items: PickerItem[] = names.map((name) => ({ key: "rumble:" + name, name, img: emotes[name].file, tooltip: `:${name}:`, insert: `:${name}:` }));
  return {
    sections: [{ id: "rumble", label: "Rumble", provider: "rumble", items }],
    inventory: items.map((it) => ({ name: it.name, img: it.img, label: "Rumble", channelScope: true, insert: it.insert })),
  };
}

export function buildKickInventory(emotes: KickEmotes): Inventory {
  const names = Object.keys(emotes);
  if (!names.length) return EMPTY_INVENTORY;
  const items: PickerItem[] = names.map((name) => ({
    key: "kick:" + name,
    name,
    img: kickEmoteUrl(emotes[name].id),
    tooltip: name,
    // Kick's own wire format, which is what its API expects back.
    insert: `[emote:${emotes[name].id}:${name}]`,
  }));
  return {
    sections: [{ id: "kick", label: "Kick", provider: "kick", items }],
    inventory: items.map((it) => ({ name: it.name, img: it.img, label: "Kick", channelScope: true, insert: it.insert })),
  };
}

export function mergeInventories(list: Inventory[]): Inventory {
  if (list.length === 1) return list[0];
  const map = new Map<string, InvEntry>();
  const sections: PickerSection[] = [];
  const seen = new Set<string>();
  for (const inv of list) {
    for (const s of inv.sections) {
      if (seen.has(s.id)) continue;
      seen.add(s.id);
      sections.push(s);
    }
    for (const e of inv.inventory) if (!map.has(e.name + "\0" + (e.insert || ""))) map.set(e.name + "\0" + (e.insert || ""), e);
  }
  return { sections, inventory: Array.from(map.values()) };
}
