// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: split into its own
// module and exported; `use7TvStyle` unchanged.
import { useEffect, useState, type CSSProperties } from "react";
import { readLsCache, writeLsCache } from "./emotes";

export type SevenTvPaintStop = { at: number; color: number };
export type SevenTvShadow = { x_offset: number; y_offset: number; radius: number; color: number };
export type SevenTvPaint = {
  id: string;
  name: string;
  function: string;
  color: number | null;
  angle: number;
  repeat: boolean;
  image_url: string | null;
  stops: SevenTvPaintStop[];
  shadows: SevenTvShadow[];
};
export type SevenTvBadgeDef = { id: string; name: string; tooltip: string };
export type SevenTvDefs = { paints: Map<string, SevenTvPaint>; badges: Map<string, SevenTvBadgeDef> };

// Key follows the same `emotes:<provider>:<scope>:<...>` scheme as the
// emote-set caches above, stored in localStorage (not sessionStorage) so it
// survives tab close — same 6h TTL as before.
const SEVENTV_DEFS_CACHE_KEY = "emotes:7tv:cosmetics:global";
const SEVENTV_DEFS_TTL_MS = 6 * 3600_000;
const SEVENTV_GQL_QUERY =
  "{cosmetics{paints{id name function color angle repeat image_url stops{at color}shadows{x_offset y_offset radius color}}badges{id name tooltip}}}";

function defsFromRaw(data: any): SevenTvDefs {
  const paints = new Map<string, SevenTvPaint>();
  for (const p of data?.paints || []) paints.set(p.id, p);
  const badges = new Map<string, SevenTvBadgeDef>();
  for (const b of data?.badges || []) badges.set(b.id, b);
  return { paints, badges };
}

let sevenTvDefsPromise: Promise<SevenTvDefs> | null = null;

export function fetchSevenTvDefs(): Promise<SevenTvDefs> {
  if (sevenTvDefsPromise) return sevenTvDefsPromise;
  sevenTvDefsPromise = (async (): Promise<SevenTvDefs> => {
    const cached = readLsCache<any>(SEVENTV_DEFS_CACHE_KEY, SEVENTV_DEFS_TTL_MS);
    if (cached) return defsFromRaw(cached);
    try {
      const r = await fetch("https://7tv.io/v3/gql", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ query: SEVENTV_GQL_QUERY }),
      });
      if (!r.ok) return { paints: new Map(), badges: new Map() };
      const json = await r.json();
      const data = json?.data?.cosmetics;
      writeLsCache(SEVENTV_DEFS_CACHE_KEY, data);
      return defsFromRaw(data);
    } catch {
      return { paints: new Map(), badges: new Map() };
    }
  })();
  return sevenTvDefsPromise;
}

export type ChatterStyle = { color: number | null; paintId: string | null; badgeId: string | null };

// Map cache holds negatives (null = fetched, no style / 404) so repeat
// lookups for chatters without cosmetics don't refire. Concurrency-limited
// to <=3 in-flight fetches via a tiny queue.
const chatterStyleCache = new Map<string, ChatterStyle | null>();
const chatterStylePending = new Map<string, Promise<ChatterStyle | null>>();
let chatterStyleActive = 0;
const chatterStyleQueue: (() => void)[] = [];

function fetchChatterStyle(userId: string): Promise<ChatterStyle | null> {
  if (chatterStyleCache.has(userId)) return Promise.resolve(chatterStyleCache.get(userId) ?? null);
  const pending = chatterStylePending.get(userId);
  if (pending) return pending;
  const p = new Promise<ChatterStyle | null>((resolve) => {
    const run = async () => {
      chatterStyleActive++;
      try {
        const r = await fetch(`https://7tv.io/v3/users/twitch/${userId}`);
        if (!r.ok) {
          chatterStyleCache.set(userId, null);
          resolve(null);
          return;
        }
        const data = await r.json();
        const style = data?.user?.style;
        const val: ChatterStyle | null = style
          ? { color: style.color ?? null, paintId: style.paint_id ?? null, badgeId: style.badge_id ?? null }
          : null;
        chatterStyleCache.set(userId, val);
        resolve(val);
      } catch {
        chatterStyleCache.set(userId, null);
        resolve(null);
      } finally {
        chatterStyleActive--;
        chatterStylePending.delete(userId);
        const next = chatterStyleQueue.shift();
        if (next) next();
      }
    };
    if (chatterStyleActive < 3) run();
    else chatterStyleQueue.push(run);
  });
  chatterStylePending.set(userId, p);
  return p;
}

export function use7TvStyle(userId: string, enabled: boolean): ChatterStyle | null | undefined {
  const [style, setStyle] = useState<ChatterStyle | null | undefined>(() =>
    enabled && chatterStyleCache.has(userId) ? (chatterStyleCache.get(userId) ?? null) : undefined,
  );
  useEffect(() => {
    if (!enabled || !userId) {
      setStyle(undefined);
      return;
    }
    if (chatterStyleCache.has(userId)) {
      setStyle(chatterStyleCache.get(userId) ?? null);
      return;
    }
    let dead = false;
    fetchChatterStyle(userId).then((s) => {
      if (!dead) setStyle(s);
    });
    return () => {
      dead = true;
    };
  }, [userId, enabled]);
  return style;
}

// Paint colors arrive as signed 32-bit RGBA ints — unsigned-shift (>>>) so
// the top byte (which flips the sign bit for most colors) decodes correctly.
export function decodeSignedRgba(c: number): string {
  const r = (c >>> 24) & 255;
  const g = (c >>> 16) & 255;
  const b = (c >>> 8) & 255;
  const a = (c & 255) / 255;
  return `rgba(${r},${g},${b},${a})`;
}

// B5: 7TV image paints render the texture LAYERED OVER the gradient/color,
// not instead of it — build the gradient/color background first (same as
// the non-image branch), then prepend the image layer as an extra
// background-image when one is present. Falls back to the image alone when
// there are no stops and no solid color to layer over.
export function paintStyle(paint: SevenTvPaint): CSSProperties {
  const shadows = (paint.shadows || [])
    .map((s) => `drop-shadow(${s.x_offset}px ${s.y_offset}px ${s.radius}px ${decodeSignedRgba(s.color)})`)
    .join(" ");
  const base: CSSProperties = {
    color: "transparent",
    WebkitBackgroundClip: "text",
    backgroundClip: "text",
    filter: shadows || undefined,
  };
  const stopsCss = (paint.stops || []).map((s) => `${decodeSignedRgba(s.color)} ${s.at * 100}%`).join(", ");
  let underlay: string | null = null;
  if (paint.function === "LINEAR_GRADIENT" && stopsCss) {
    const fn = paint.repeat ? "repeating-linear-gradient" : "linear-gradient";
    underlay = `${fn}(${paint.angle}deg, ${stopsCss})`;
  } else if (paint.function === "RADIAL_GRADIENT" && stopsCss) {
    // Honor `repeat` for radial paints too — several 7TV paints (e.g. "Heavy
    // Rain") are REPEATING radial gradients whose concentric-ring pattern
    // collapses into one smooth blob when rendered as a plain radial-gradient.
    const fn = paint.repeat ? "repeating-radial-gradient" : "radial-gradient";
    underlay = `${fn}(circle, ${stopsCss})`;
  } else if (paint.color != null) {
    const solid = decodeSignedRgba(paint.color);
    underlay = `linear-gradient(${solid}, ${solid})`;
  }
  if (paint.image_url) {
    return {
      ...base,
      backgroundImage: underlay ? `url(${paint.image_url}), ${underlay}` : `url(${paint.image_url})`,
      backgroundSize: underlay ? "cover, cover" : "cover",
    };
  }
  if (underlay) return { ...base, backgroundImage: underlay };
  return base;
}
