// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2). Changes: split into its own
// module (pure, no React): Twitch emote/GIF tag parsing and message -> renderable parts.
import { asLink, MENTION_WORD } from "../chatText";
import type { EmoteProvider, EmoteSets, ProviderEmote } from "./emotes";

export type TwitchEmoteRange = { id: string; start: number; end: number };

export function parseTwitchEmoteTag(tag: string): TwitchEmoteRange[] {
  if (!tag) return [];
  const out: TwitchEmoteRange[] = [];
  for (const part of tag.split("/")) {
    const [id, ranges] = part.split(":");
    if (!id || !ranges) continue;
    for (const r of ranges.split(",")) {
      const [s, e] = r.split("-").map(Number);
      if (Number.isFinite(s) && Number.isFinite(e)) out.push({ id, start: s, end: e });
    }
  }
  return out.sort((a, b) => a.start - b.start);
}

export type GifRange = { start: number; end: number; id: string; url: string };

/**
 * Parse Twitch's `gifs` tag: `<start>-<end>|<gifID>|<gifURL>`, comma-separated.
 *
 * Splitting naively on "," is not quite safe — a Giphy URL is a query string,
 * and nothing stops one carrying a comma. So the split is re-joined wherever a
 * fragment doesn't begin a fresh `<digits>-<digits>|` entry, which puts the URL
 * back together instead of silently truncating it into a broken image.
 */
export function parseTwitchGifTag(tag: string): GifRange[] {
  if (!tag) return [];
  const entries: string[] = [];
  for (const frag of tag.split(",")) {
    if (/^\d+-\d+\|/.test(frag) || entries.length === 0) entries.push(frag);
    else entries[entries.length - 1] += "," + frag;
  }

  const out: GifRange[] = [];
  for (const entry of entries) {
    // Only the first two separators are structural; everything after the
    // second "|" is the URL, verbatim.
    const first = entry.indexOf("|");
    const second = entry.indexOf("|", first + 1);
    if (first < 0 || second < 0) continue;
    const [s, e] = entry.slice(0, first).split("-").map(Number);
    const url = entry.slice(second + 1);
    // http(s) only: this lands in an <img src>, and a tag value is remote data.
    if (!Number.isFinite(s) || !Number.isFinite(e) || !/^https?:\/\//i.test(url)) continue;
    out.push({ start: s, end: e, id: entry.slice(first + 1, second), url });
  }
  return out.sort((a, b) => a.start - b.start);
}

// "twitch" is positional (IRC ranges); the rest are name-matched provider emotes merged into `EmoteSets.all`.
export type EmoteSource = "twitch" | EmoteProvider;
export type KnownChatter = { name: string; color: string; id: string };

export type MsgPart =
  | { kind: "text"; text: string }
  | {
      kind: "emote";
      source: EmoteSource;
      id: string;
      name: string;
      overlays: { source: EmoteSource; id: string; name: string }[];
    }
  | { kind: "mention"; login: string; name: string; color: string; userId: string }
  | { kind: "link"; href: string; text: string }
  | { kind: "gif"; id: string; url: string; alt: string };

// `knownChatters` is optional — only MessageRow (the live chat feed) passes
// it; other buildParts call sites (notices, the user card's recent-messages
// list, the pinned banner) omit it and simply never produce mention parts.
// Matching is O(words in this one message) via a Map lookup, never a scan of
// the whole buffer — the map itself is maintained incrementally by the
// caller (Chat()'s knownChattersRef) as messages arrive.
export function buildParts(
  text: string,
  emotesTag: string,
  emotes: EmoteSets | null,
  knownChatters?: Map<string, KnownChatter> | null,
  gifsTag?: string,
): MsgPart[] {
  const chars = Array.from(text);
  // Twitch emotes and GIFs both index the SAME raw text, so they are walked as
  // one ordered range list rather than in two passes — two passes would either
  // double-consume an overlapping span or renumber the second tag's positions.
  const ranges: ({ start: number; end: number } & ({ t: "emote"; id: string } | { t: "gif"; id: string; url: string }))[] = [
    ...parseTwitchEmoteTag(emotesTag).map((r) => ({ ...r, t: "emote" as const })),
    ...parseTwitchGifTag(gifsTag || "").map((r) => ({ ...r, t: "gif" as const })),
  ].sort((a, b) => a.start - b.start);

  const raw: (
    | { kind: "text"; text: string }
    | { kind: "emote"; source: "twitch"; id: string; name: string }
    | { kind: "gif"; id: string; url: string; alt: string }
  )[] = [];
  let i = 0;
  for (const r of ranges) {
    if (r.start < i) continue; // guard against overlapping/out-of-order tag data
    if (r.start > i) raw.push({ kind: "text", text: chars.slice(i, r.start).join("") });
    const span = chars.slice(r.start, r.end + 1).join("");
    // The span a GIF covers is its fallback label — "[Happy Dance GIF by World
    // of Warcraft]" — which is what showed up as literal text before this
    // existed. Kept as the alt/title so the message still reads sensibly when
    // the image fails or is read aloud.
    if (r.t === "gif") raw.push({ kind: "gif", id: r.id, url: r.url, alt: span });
    else raw.push({ kind: "emote", source: "twitch", id: r.id, name: span });
    i = r.end + 1;
  }
  if (i < chars.length) raw.push({ kind: "text", text: chars.slice(i).join("") });

  const lookup = (word: string): ProviderEmote | undefined => emotes?.all.get(word);

  type MidTok =
    | { kind: "text"; text: string }
    | { kind: "emote"; source: EmoteSource; id: string; name: string; zeroWidth: boolean }
    | { kind: "mention"; login: string; name: string; color: string; userId: string }
    | { kind: "link"; href: string; text: string };
  const mid: (MidTok | { kind: "gif"; id: string; url: string; alt: string })[] = [];
  for (const tok of raw) {
    // A GIF is already a resolved image; it must not go through the
    // word-splitting/emote-lookup pass below.
    if (tok.kind === "gif") {
      mid.push(tok);
      continue;
    }
    if (tok.kind === "emote") {
      mid.push({ ...tok, zeroWidth: false });
      continue;
    }
    for (const w of tok.text.split(/(\s+)/)) {
      if (w === "") continue;
      if (/^\s+$/.test(w)) {
        mid.push({ kind: "text", text: w });
        continue;
      }
      const link = asLink(w);
      if (link) {
        if (link.lead) mid.push({ kind: "text", text: link.lead });
        mid.push({ kind: "link", href: link.href, text: link.text });
        if (link.trail) mid.push({ kind: "text", text: link.trail });
        continue;
      }
      const e = lookup(w);
      if (e) {
        mid.push({ kind: "emote", source: e.provider, id: e.id, name: e.name, zeroWidth: e.zeroWidth });
        continue;
      }
      // CHAT v7 item 2: mention chip — word matches a chatter seen in the
      // buffer (with or without a leading "@"), case-insensitively.
      //
      // The name is peeled out of its surrounding punctuation rather than
      // required to be the whole word: "@a_g_u__," and "a_g_u__:" are mentions,
      // and the comma or colon is pushed back as ordinary text so the sentence
      // still reads the way it was typed.
      const m = w.match(MENTION_WORD);
      const known = m && knownChatters ? knownChatters.get(m[2].toLowerCase()) : undefined;
      if (m && known) {
        // The chip renders its own "@", so a leading one is dropped rather
        // than pushed back — putting it back gave "@@name". Anything else in
        // front (a bracket, a quote) is real text and survives.
        const lead = m[1].replace(/@+$/, "");
        if (lead) mid.push({ kind: "text", text: lead });
        mid.push({ kind: "mention", login: m[2].toLowerCase(), name: known.name, color: known.color, userId: known.id });
        if (m[3]) mid.push({ kind: "text", text: m[3] });
      } else mid.push({ kind: "text", text: w });
    }
  }

  const out: MsgPart[] = [];
  for (const tok of mid) {
    if (tok.kind === "gif") {
      out.push(tok);
      continue;
    }
    if (tok.kind === "emote" && tok.source !== "twitch" && tok.zeroWidth) {
      // CHAT v5 bug fix: zero-width emotes must overlay the PRECEDING emote,
      // but the raw text almost always has a single space between them
      // (e.g. "widepeepoHappy CandyCane") which `mid` preserves as its own
      // whitespace text token — so the naive "look at the last `out` entry"
      // check used to see that space token instead of the emote and fall
      // through to rendering the zero-width emote as a normal inline emote
      // (appearing BESIDE the base emote instead of on top of it). Walk back
      // past exactly one connecting whitespace-only text token to find the
      // real preceding emote, and drop that token (it's now purely visual
      // overlay, not adjacent text) so the two render as one unit. This also
      // makes multiple zero-width emotes in a row stack correctly, since
      // each one leaves the base emote as the new last `out` entry.
      let prevIdx = out.length - 1;
      const maybeGap = out[prevIdx];
      if (maybeGap && maybeGap.kind === "text" && /^\s+$/.test(maybeGap.text) && out[prevIdx - 1]?.kind === "emote") {
        out.splice(prevIdx, 1);
        prevIdx -= 1;
      }
      const prev = out[prevIdx];
      if (prev && prev.kind === "emote") {
        prev.overlays.push({ source: tok.source, id: tok.id, name: tok.name });
        continue;
      }
      out.push({ kind: "emote", source: tok.source, id: tok.id, name: tok.name, overlays: [] });
      continue;
    }
    if (tok.kind === "emote") out.push({ kind: "emote", source: tok.source, id: tok.id, name: tok.name, overlays: [] });
    else out.push(tok);
  }
  return out;
}
