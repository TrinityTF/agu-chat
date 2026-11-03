// Ignored users and ignored phrases, applied to a split's rows before they are drawn. Pure (type-only
// imports), so it runs under `node --test` as well as in the renderer.
//
// * An ignored user (platform + login) hides every chat message of theirs on that platform.
// * An ignored phrase either hides the message, or replaces each match with a literal replacement. All
//   replace rules are matched against the ORIGINAL text in one pass (earliest match wins, then the longest),
//   so rules never rewrite each other's output and Twitch's positional emote / GIF ranges can be remapped.
import type { IgnoredPhrase, IgnoredUser, Platform } from "../protocol";
import type { Row } from "./types";

export type CompiledIgnores = {
  users: Set<string>;
  hide: RegExp[];
  replace: { re: RegExp; with: string }[];
  /** Nothing to do: callers can skip the pass entirely. */
  empty: boolean;
};

export const NO_IGNORES: CompiledIgnores = { users: new Set(), hide: [], replace: [], empty: true };

/** What an ignore entry stores for a user: lowercase login without "@" (YouTube has no login: its name). */
export function ignoreLogin(user: { login?: string; name?: string }): string {
  return (user.login || user.name || "").trim().replace(/^@/, "").toLowerCase();
}

export const ignoreKey = (platform: Platform, login: string) => `${platform}:${ignoreLogin({ login })}`;

export function isUserIgnored(list: IgnoredUser[], platform: Platform, login: string): boolean {
  const l = ignoreLogin({ login });
  return !!l && list.some((u) => u.platform === platform && u.login === l);
}

/** The list with this user added (or removed when `on` is false). */
export function toggleIgnoredUser(list: IgnoredUser[], platform: Platform, login: string, on: boolean): IgnoredUser[] {
  const l = ignoreLogin({ login });
  const rest = list.filter((u) => !(u.platform === platform && u.login === l));
  return on && l ? [...rest, { platform, login: l }] : rest;
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** Why a phrase can't be used (an invalid regular expression), or null. */
export function phraseError(p: Pick<IgnoredPhrase, "pattern" | "regex" | "caseSensitive">): string | null {
  if (!p.regex) return null;
  try {
    new RegExp(p.pattern, p.caseSensitive ? "gu" : "giu");
    return null;
  } catch (e) {
    return (e as Error).message.replace(/^Invalid regular expression: /, "").replace(/^\/.*\/[a-z]*: /s, "");
  }
}

function compilePhrase(p: IgnoredPhrase): RegExp | null {
  if (!p.pattern) return null;
  try {
    return new RegExp(p.regex ? p.pattern : escapeRe(p.pattern), p.caseSensitive ? "gu" : "giu");
  } catch {
    return null; // shown as an error in settings; skipped here
  }
}

export function compileIgnores(users: IgnoredUser[] | undefined, phrases: IgnoredPhrase[] | undefined): CompiledIgnores {
  const out: CompiledIgnores = { users: new Set(), hide: [], replace: [], empty: true };
  for (const u of users ?? []) if (u && u.login) out.users.add(ignoreKey(u.platform, u.login));
  for (const p of phrases ?? []) {
    const re = compilePhrase(p);
    if (!re) continue;
    if (p.action === "replace") out.replace.push({ re, with: p.replacement ?? "" });
    else out.hide.push(re);
  }
  out.empty = !out.users.size && !out.hide.length && !out.replace.length;
  return out;
}

/** Does `re` (a global regex) find a non-empty match in `text`? */
function hits(re: RegExp, text: string): boolean {
  re.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text))) {
    if (m[0].length) return true;
    re.lastIndex++; // zero-length match: step over it
  }
  return false;
}

/** One replaced span of the original text, in UTF-16 units. */
export type Edit = { start: number; end: number; with: string };

/** Non-overlapping matches of every replace rule, in text order. */
export function findEdits(text: string, rules: CompiledIgnores["replace"]): Edit[] {
  const all: Edit[] = [];
  for (const r of rules) {
    r.re.lastIndex = 0;
    let m: RegExpExecArray | null;
    while ((m = r.re.exec(text))) {
      if (!m[0].length) {
        r.re.lastIndex++;
        continue;
      }
      all.push({ start: m.index, end: m.index + m[0].length, with: r.with });
    }
  }
  all.sort((a, b) => a.start - b.start || b.end - a.end);
  const out: Edit[] = [];
  let at = 0;
  for (const e of all) {
    if (e.start < at) continue;
    out.push(e);
    at = e.end;
  }
  return out;
}

export function applyEdits(text: string, edits: Edit[]): string {
  let out = "";
  let at = 0;
  for (const e of edits) {
    out += text.slice(at, e.start) + e.with;
    at = e.end;
  }
  return out + text.slice(at);
}

const cpLen = (s: string) => {
  let n = 0;
  for (const _ of s) n++;
  return n;
};

/**
 * Twitch's `emotes` / `gifs` tags index the text by code point. After replacing, keep the ranges that lie
 * wholly outside every edit, shifted to their new position, and drop the ones an edit touched.
 */
function remapper(text: string, edits: Edit[]): (start: number, end: number) => [number, number] | null {
  // Kept segments of the original text, in code points: [oldStart, oldEnd) -> newStart.
  const segs: { from: number; to: number; at: number }[] = [];
  let oldCp = 0;
  let newCp = 0;
  let pos = 0;
  for (const e of [...edits, { start: text.length, end: text.length, with: "" }]) {
    const keep = cpLen(text.slice(pos, e.start));
    segs.push({ from: oldCp, to: oldCp + keep, at: newCp });
    oldCp += keep + cpLen(text.slice(e.start, e.end));
    newCp += keep + cpLen(e.with);
    pos = e.end;
  }
  return (start, end) => {
    const s = segs.find((g) => start >= g.from && end < g.to);
    return s ? [s.at + start - s.from, s.at + end - s.from] : null;
  };
}

export function remapEmoteTag(tag: string, map: (s: number, e: number) => [number, number] | null): string {
  if (!tag) return tag;
  const out: string[] = [];
  for (const part of tag.split("/")) {
    const [id, ranges] = part.split(":");
    if (!id || !ranges) continue;
    const kept: string[] = [];
    for (const r of ranges.split(",")) {
      const [s, e] = r.split("-").map(Number);
      const m = Number.isFinite(s) && Number.isFinite(e) ? map(s, e) : null;
      if (m) kept.push(`${m[0]}-${m[1]}`);
    }
    if (kept.length) out.push(`${id}:${kept.join(",")}`);
  }
  return out.join("/");
}

export function remapGifTag(tag: string, map: (s: number, e: number) => [number, number] | null): string {
  if (!tag) return tag;
  // Same entry splitting as parseTwitchGifTag: a URL may carry commas.
  const entries: string[] = [];
  for (const frag of tag.split(",")) {
    if (/^\d+-\d+\|/.test(frag) || entries.length === 0) entries.push(frag);
    else entries[entries.length - 1] += "," + frag;
  }
  const out: string[] = [];
  for (const entry of entries) {
    const m = /^(\d+)-(\d+)(\|.*)$/s.exec(entry);
    const r = m ? map(Number(m[1]), Number(m[2])) : null;
    if (m && r) out.push(`${r[0]}-${r[1]}${m[3]}`);
  }
  return out.join(",");
}

/** The author of a chat message row as an ignore key, or null for rows that aren't chat messages. */
export function rowIgnoreKey(r: Row): string | null {
  switch (r.k) {
    case "msg":
    case "rumble":
    case "kick":
    case "x":
      return ignoreKey(r.platform, r.m.user.login);
    case "youtube":
      return ignoreKey(r.platform, r.m.user.name);
    default:
      return null;
  }
}

/**
 * The row as it should be shown: `null` to hide it, the same object when nothing applies (so memoized rows
 * stay memoized), or a copy with the replaced text. Only chat messages are affected, not notices or mod lines.
 */
export function applyIgnores(r: Row, c: CompiledIgnores): Row | null {
  if (c.empty) return r;
  const key = rowIgnoreKey(r);
  if (key === null) return r;
  if (c.users.has(key)) return null;
  if (r.k !== "msg" && r.k !== "rumble" && r.k !== "kick" && r.k !== "youtube" && r.k !== "x") return r;
  const text = r.m.text || "";
  for (const re of c.hide) if (hits(re, text)) return null;
  if (!c.replace.length) return r;

  if (r.k === "youtube") {
    // Rendered from `parts`: replace inside each text part (emoji parts are left alone).
    let changed = false;
    const parts = r.m.parts.map((p) => {
      if (p.t !== "text") return p;
      const edits = findEdits(p.v, c.replace);
      if (!edits.length) return p;
      changed = true;
      return { ...p, v: applyEdits(p.v, edits) };
    });
    const edits = findEdits(text, c.replace);
    if (!changed && !edits.length) return r;
    return { ...r, m: { ...r.m, parts, text: applyEdits(text, edits) } };
  }

  const edits = findEdits(text, c.replace);
  if (!edits.length) return r;
  const next = applyEdits(text, edits);
  if (r.k === "msg") {
    const map = remapper(text, edits);
    return { ...r, m: { ...r.m, text: next, emotes: remapEmoteTag(r.m.emotes, map), gifs: r.m.gifs ? remapGifTag(r.m.gifs, map) : r.m.gifs } };
  }
  return { ...r, m: { ...r.m, text: next } } as Row;
}

/** Apply ignores to a list, reusing results for rows already seen with the same rules (`cache`). */
export function filterIgnored(rows: Row[], c: CompiledIgnores, cache: WeakMap<Row, Row | null>): Row[] {
  if (c.empty) return rows;
  const out: Row[] = [];
  for (const r of rows) {
    let v = cache.get(r);
    if (v === undefined) {
      v = applyIgnores(r, c);
      cache.set(r, v);
    }
    if (v) out.push(v);
  }
  return out;
}
