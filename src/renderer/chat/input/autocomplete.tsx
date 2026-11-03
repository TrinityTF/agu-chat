// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2: InvEntry / AutoMatch /
// AutoState / rankAutoMatches / AutocompleteDropdown) and web/src/app/mentionAuto.tsx (findMentionQuery,
// rankMentionMatches, useMentions, handleMentionKey, MentionDropdown). Changes: restyled on the theme
// tokens; entries can carry an `insert` string (Rumble ":name:", Kick "[emote:id:name]"); Tab on a plain
// word falls back to chatter names after the emotes; useMentions reads its name list lazily (a getter)
// so the send box does not rebuild a 1000-row name list on every incoming message.
import { useCallback, useState } from "react";

// ---- emote / command autocomplete ---------------------------------------------------------------

// A flattened inventory entry. `channelScope` covers channel AND personal sets (both rank above globals).
export type InvEntry = { name: string; img: string; label: string; channelScope: boolean; insert?: string };
// `dim` is only set in "cmd" mode: a recognized slash command the account can't currently use.
export type AutoMatch = { name: string; img: string; label: string; dim?: boolean; insert?: string };
// TRIGGER A (Tab, prefix-only) vs TRIGGER B (":xx", substring) vs TRIGGER C ("/", command-name prefix).
export type AutoState = { mode: "prefix" | "colon" | "cmd"; start: number; end: number; matches: AutoMatch[]; idx: number };

// Prefix matches before substring matches, channel/personal sets before global, then alphabetical. Capped
// to 50 (the dropdown shows ~8 rows and scrolls).
export function rankAutoMatches(inv: InvEntry[], query: string, mode: "prefix" | "colon"): AutoMatch[] {
  const q = query.toLowerCase();
  if (!q) return [];
  const scored: { e: InvEntry; prefix: boolean }[] = [];
  for (const e of inv) {
    const lname = e.name.toLowerCase();
    if (mode === "prefix") {
      if (lname.startsWith(q)) scored.push({ e, prefix: true });
    } else if (lname.includes(q)) {
      scored.push({ e, prefix: lname.startsWith(q) });
    }
  }
  scored.sort((a, b) => {
    if (a.prefix !== b.prefix) return a.prefix ? -1 : 1;
    if (a.e.channelScope !== b.e.channelScope) return a.e.channelScope ? -1 : 1;
    return a.e.name.localeCompare(b.e.name);
  });
  return scored.slice(0, 50).map((s) => ({ name: s.e.name, img: s.e.img, label: s.e.label, insert: s.e.insert }));
}

/** Tab on a word: emotes first, then chatter names (completed with the "@" when the word had one). */
export function rankTabMatches(inv: InvEntry[], chatters: string[], word: string): AutoMatch[] {
  const emotes = rankAutoMatches(inv, word.replace(/^@/, ""), "prefix");
  const bare = word.replace(/^@/, "").toLowerCase();
  if (!bare || (emotes.length && !word.startsWith("@"))) return emotes;
  const names: AutoMatch[] = [];
  const seen = new Set<string>();
  for (const n of chatters) {
    const l = n.toLowerCase();
    if (seen.has(l) || !l.startsWith(bare)) continue;
    seen.add(l);
    names.push({ name: word.startsWith("@") ? "@" + n : n, img: "", label: "chatter" });
    if (names.length >= 20) break;
  }
  return word.startsWith("@") ? names.concat(emotes) : emotes.concat(names);
}

export function AutocompleteDropdown({ state, onPick }: { state: AutoState; onPick: (m: AutoMatch) => void }) {
  const isCmd = state.mode === "cmd";
  return (
    <div className="absolute bottom-full left-0 z-30 mb-1 max-h-56 w-72 overflow-y-auto rounded border border-line bg-raised py-1 shadow-2xl shadow-black/60">
      {state.matches.map((m, i) => (
        <button
          key={`${m.label}:${m.name}`}
          type="button"
          // mousedown (not click) + preventDefault so the input never loses focus/selection before the pick.
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(m);
          }}
          className={`flex w-full items-center gap-2 px-2 py-1 text-left text-xs ${i === state.idx ? "bg-accent/25 text-text" : m.dim ? "text-faint hover:bg-raised-2" : "text-muted hover:bg-raised-2"}`}
        >
          {isCmd ? (
            <>
              <span className="shrink-0 font-mono font-semibold text-accent-soft">/{m.name}</span>
              <span className="min-w-0 flex-1 truncate text-faint">{m.label}</span>
              {m.dim && <span className="shrink-0 text-[10px] text-amber-500">(not available)</span>}
            </>
          ) : (
            <>
              {m.img ? <img src={m.img} alt="" decoding="async" className="h-5 w-5 shrink-0 object-contain" /> : <span className="grid h-5 w-5 shrink-0 place-items-center text-faint">@</span>}
              <span className="min-w-0 flex-1 truncate">{m.name}</span>
              <span className="shrink-0 text-[10px] text-faint">{m.label}</span>
            </>
          )}
        </button>
      ))}
    </div>
  );
}

// ---- @name completion ---------------------------------------------------------------------------

export type MentionState = { start: number; end: number; matches: string[]; idx: number };

// A trailing "@word" at the caret. The "@" must start a word: mid-word "@" is an email or a handle
// already typed, neither of which wants a dropdown over it.
const MENTION_RE = /(^|\s)@([^\s@]{0,40})$/;

export function findMentionQuery(before: string): { start: number; query: string } | null {
  const m = MENTION_RE.exec(before);
  if (!m) return null;
  return { start: before.length - m[2].length - 1, query: m[2] };
}

// Prefix matches first, then substring, then alphabetical.
export function rankMentionMatches(names: Iterable<string>, query: string, limit = 8): string[] {
  const q = query.toLowerCase();
  const scored: { name: string; prefix: boolean; i: number }[] = [];
  const seen = new Set<string>();
  let i = 0;
  for (const name of names) {
    if (!name) continue;
    const lower = name.toLowerCase();
    if (seen.has(lower)) continue;
    // An empty query lists recent chatters: a useful "who is here" rather than an empty dropdown.
    if (q && !lower.includes(q)) continue;
    seen.add(lower);
    scored.push({ name, prefix: lower.startsWith(q), i: i++ });
  }
  scored.sort((a, b) => {
    if (a.prefix !== b.prefix) return a.prefix ? -1 : 1;
    // With no query the incoming order (newest first) is the ranking.
    if (!q) return a.i - b.i;
    return a.name.localeCompare(b.name);
  });
  return scored.slice(0, limit).map((s) => s.name);
}

/**
 * Owns the dropdown state for one input. `refresh` is called on every change with the new value and caret;
 * `apply` returns the rewritten text and where the caret should land, or null when there is nothing to
 * apply. The caller keeps ownership of its own text state.
 */
export function useMentions(getNames: () => string[]) {
  const [state, setState] = useState<MentionState | null>(null);

  const refresh = useCallback(
    (value: string, caret: number) => {
      const hit = findMentionQuery(value.slice(0, caret));
      if (!hit) return setState(null);
      const matches = rankMentionMatches(getNames(), hit.query);
      setState(matches.length ? { start: hit.start, end: caret, matches, idx: 0 } : null);
    },
    [getNames],
  );

  const close = useCallback(() => setState(null), []);

  const move = useCallback((delta: number) => {
    setState((s) => (s ? { ...s, idx: (s.idx + delta + s.matches.length) % s.matches.length } : s));
  }, []);

  const apply = useCallback(
    (value: string, choice?: string): { text: string; caret: number } | null => {
      if (!state) return null;
      const name = choice ?? state.matches[state.idx];
      if (!name) return null;
      // One trailing space, and none added when the next character already is one.
      const after = value.slice(state.end);
      const insert = `@${name}` + (after.startsWith(" ") ? "" : " ");
      setState(null);
      return { text: value.slice(0, state.start) + insert + after, caret: state.start + insert.length };
    },
    [state],
  );

  return { state, refresh, close, move, apply };
}

/**
 * Handles the keys the dropdown owns while it is open. Returns true when the key was consumed, so the
 * caller's own Enter-sends / Escape-clears behaviour only runs once the dropdown is closed. Tab (and
 * Shift+Tab) COMPLETES, like Enter; choosing is what the arrows are for.
 */
export function handleMentionKey(
  e: { key: string; shiftKey: boolean; preventDefault: () => void },
  mentions: ReturnType<typeof useMentions>,
  commit: () => void,
): boolean {
  if (!mentions.state) return false;
  if (e.key === "Tab" || e.key === "Enter") {
    e.preventDefault();
    commit();
    return true;
  }
  if (e.key === "ArrowUp") {
    e.preventDefault();
    mentions.move(-1);
    return true;
  }
  if (e.key === "ArrowDown") {
    e.preventDefault();
    mentions.move(1);
    return true;
  }
  if (e.key === "Escape") {
    e.preventDefault();
    mentions.close();
    return true;
  }
  return false;
}

export function MentionDropdown({ state, onPick }: { state: MentionState; onPick: (name: string) => void }) {
  return (
    <div className="absolute bottom-full left-0 z-30 mb-1 max-h-56 w-56 overflow-y-auto rounded border border-line bg-raised py-1 shadow-2xl shadow-black/60">
      {state.matches.map((name, i) => (
        <button
          key={name}
          type="button"
          onMouseDown={(e) => {
            e.preventDefault();
            onPick(name);
          }}
          className={`flex w-full items-center gap-2 px-2 py-1 text-left text-xs ${i === state.idx ? "bg-accent/25 text-text" : "text-muted hover:bg-raised-2"}`}
        >
          <span className="shrink-0 text-faint">@</span>
          <span className="min-w-0 flex-1 truncate">{name}</span>
        </button>
      ))}
    </div>
  );
}
