// Split search (Ctrl+F): which rows match a query. Pure (type-only imports), tested with `node --test`.
//
// Query syntax: whitespace-separated words, all of which must appear (case-insensitive) in the message text
// or the author's name / login. `from:name` (or `from:@name`) keeps only that author's rows; several
// `from:` words mean any of those authors.
import type { Row } from "./types";

export type SearchQuery = { from: string[]; words: string[] };

export function parseSearch(q: string): SearchQuery {
  const from: string[] = [];
  const words: string[] = [];
  for (const tok of q.trim().split(/\s+/)) {
    if (!tok) continue;
    const m = /^from:@?(.*)$/i.exec(tok);
    if (m) {
      if (m[1]) from.push(m[1].toLowerCase());
    } else words.push(tok.toLowerCase());
  }
  return { from, words };
}

export const isEmptyQuery = (q: SearchQuery) => !q.from.length && !q.words.length;

const lc = (s: string | undefined) => (s || "").toLowerCase();

/** The author of a row as lowercase [login, display name], or null (mod feed lines have none). */
export function rowAuthorNames(r: Row): string[] | null {
  switch (r.k) {
    case "msg":
    case "rumble":
    case "kick":
    case "x":
      return [lc(r.m.user.login), lc(r.m.user.name)];
    case "youtube":
      return [lc(r.m.user.name).replace(/^@/, ""), lc(r.m.user.name)];
    case "notice":
      return [lc(r.n.user.login), lc(r.n.user.name)];
    case "automod":
      return [lc(r.h.user.login), lc(r.h.user.name)];
    default:
      return null;
  }
}

export function rowText(r: Row): string {
  switch (r.k) {
    case "msg":
    case "rumble":
    case "kick":
    case "youtube":
    case "x":
      return r.m.text || "";
    case "notice":
      return [r.n.system, r.n.text].filter(Boolean).join(" ");
    case "modfeed":
      return r.f.text;
    case "automod":
      return r.h.text;
  }
}

export function matchRow(r: Row, q: SearchQuery): boolean {
  const names = rowAuthorNames(r);
  if (q.from.length && (!names || !q.from.some((f) => names.includes(f)))) return false;
  if (!q.words.length) return true;
  const hay = `${rowText(r)}\n${names ? names.join(" ") : ""}`.toLowerCase();
  return q.words.every((w) => hay.includes(w));
}

export function searchRows(rows: Row[], query: string): Row[] {
  const q = parseSearch(query);
  if (isEmptyQuery(q)) return rows;
  return rows.filter((r) => matchRow(r, q));
}
