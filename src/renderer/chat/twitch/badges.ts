// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: split into its own
// module and exported; the site cached badges in sessionStorage per channel, this caches per source
// in the store (see store.ts), so only the pure helpers live here.
export type BadgeVersion = { id: string; image_url_2x: string; title: string };
export type BadgeSet = { set_id: string; versions: BadgeVersion[] };
export type BadgesResp = { global: BadgeSet[]; channel: BadgeSet[] };
export type BadgeMap = Map<string, { url: string; title: string }>;

export function buildBadgeMap(resp: BadgesResp): BadgeMap {
  const m: BadgeMap = new Map();
  for (const set of [...(resp.global || []), ...(resp.channel || [])]) {
    for (const v of set.versions || []) {
      m.set(`${set.set_id}/${v.id}`, { url: v.image_url_2x, title: v.title || set.set_id });
    }
  }
  return m;
}

export function parseBadgeList(tag: string, map: BadgeMap): { url: string; title: string }[] {
  if (!tag) return [];
  const out: { url: string; title: string }[] = [];
  for (const b of tag.split(",")) {
    const hit = map.get(b);
    if (hit) out.push(hit);
  }
  return out;
}

// CHAT v4 alignment round 2: badge <img>s get the same vertical-align:middle
// + negative vertical margin treatment as emotes, so an 18px badge doesn't
// inflate the line box and lift the name button above the text baseline.
export const BADGE_PX = 16;
export const BADGE_VMARGIN = -3;
