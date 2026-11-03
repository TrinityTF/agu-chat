// Highlight rules (Settings > Highlights): compile the user's rules once, then match each chat row against
// them. Pure logic with type-only imports, so `node --test` can load this file directly (see test/).
import type { HighlightRule } from "../protocol";
import type { Row } from "./types";

/** What a rule looks at: the message text and who wrote it. */
export type MatchInput = { text: string; login: string; name: string };

type Compiled = { rule: HighlightRule; test: (m: MatchInput) => boolean };

export type Highlighter = {
  /** The first enabled rule (in list order) that matches, or null. */
  match(m: MatchInput): HighlightRule | null;
  /** How many rules can match at all (enabled, non-empty, valid). */
  size: number;
};

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** A user rule's names: separated by commas or whitespace, a leading "@" is ignored. */
export function ruleUsers(pattern: string): string[] {
  return pattern
    .split(/[\s,]+/)
    .map((s) => s.replace(/^@+/, ""))
    .filter(Boolean);
}

/** The problem with a regex rule's pattern, or null when it compiles. */
export function regexError(pattern: string, caseSensitive: boolean): string | null {
  try {
    new RegExp(pattern, caseSensitive ? "" : "i");
    return null;
  } catch (e) {
    return e instanceof Error ? e.message.replace(/^Invalid regular expression: /, "") : String(e);
  }
}

function compileRule(rule: HighlightRule): Compiled | null {
  if (!rule.enabled) return null;
  const pattern = rule.pattern.trim();
  if (!pattern) return null;
  const cs = rule.caseSensitive;
  switch (rule.kind) {
    case "phrase": {
      // Whole words: "agu" matches "hi agu!" but not "agurk". Letters/digits/_ in any script count as word
      // characters, so a phrase that starts or ends with punctuation ("!drop") still works.
      const re = new RegExp(`(?<![\\p{L}\\p{N}_])${escapeRe(pattern)}(?![\\p{L}\\p{N}_])`, cs ? "u" : "iu");
      return { rule, test: (m) => re.test(m.text) };
    }
    case "regex": {
      let re: RegExp;
      try {
        re = new RegExp(pattern, cs ? "" : "i");
      } catch {
        return null;
      }
      return { rule, test: (m) => re.test(m.text) };
    }
    case "user": {
      const fold = (s: string) => (cs ? s : s.toLowerCase());
      const names = new Set(ruleUsers(pattern).map(fold));
      if (!names.size) return null;
      return { rule, test: (m) => (!!m.login && names.has(fold(m.login))) || (!!m.name && names.has(fold(m.name))) };
    }
    default:
      return null;
  }
}

export function compileHighlights(rules: readonly HighlightRule[] | null | undefined): Highlighter {
  const list: Compiled[] = [];
  for (const r of rules || []) {
    const c = compileRule(r);
    if (c) list.push(c);
  }
  return {
    size: list.length,
    match(m) {
      for (const c of list) if (c.test(m)) return c.rule;
      return null;
    },
  };
}

/** The text and author a rule is matched against, or null for rows rules never apply to (system lines). */
export function matchInputOf(row: Row): MatchInput | null {
  switch (row.k) {
    case "msg":
      return { text: row.m.text, login: row.m.user.login, name: row.m.user.name };
    case "rumble":
      return { text: row.m.text, login: row.m.user.login, name: row.m.user.name };
    case "kick":
      // Emotes are inline "[emote:123:name]" tokens: match on the emote's name as it reads on screen.
      return { text: row.m.text.replace(/\[emote:\d+:([^\]]{1,80})\]/g, "$1"), login: row.m.user.login, name: row.m.user.name };
    case "youtube":
      return { text: row.m.text, login: "", name: row.m.user.name };
    case "x":
      return { text: row.m.text, login: row.m.user.login, name: row.m.user.name };
    default:
      return null;
  }
}

/** "#rrggbb" plus an alpha, as rgba(); anything else falls back to the accent purple. */
export function tint(color: string, alpha: number): string {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(color);
  const [r, g, b] = m ? [m[1], m[2], m[3]].map((h) => parseInt(h, 16)) : [145, 71, 255];
  return `rgba(${r},${g},${b},${alpha})`;
}

/** The row style for a matched rule: a coloured left bar and a light tint of the same colour. */
export function ruleRowStyle(color: string): { borderLeft: string; background: string; paddingLeft: number } {
  return { borderLeft: `3px solid ${tint(color, 1)}`, background: tint(color, 0.14), paddingLeft: 6 };
}

/** Colours offered for a new rule, in order; the first one no rule uses yet is picked. */
export const RULE_COLORS = ["#ff9f1a", "#2bd4a4", "#3fa9ff", "#ff5cc8", "#f5d90a", "#a970ff", "#ff5c5c", "#7ddc1f"];

export function nextRuleColor(rules: readonly HighlightRule[]): string {
  const used = new Set(rules.map((r) => r.color.toLowerCase()));
  return RULE_COLORS.find((c) => !used.has(c)) ?? RULE_COLORS[rules.length % RULE_COLORS.length];
}
