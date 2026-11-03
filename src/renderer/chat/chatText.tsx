// Ported from modtools.agu.lol web/src/app/chatText.tsx (commit 1b826c2). Changes: the hover link preview
// (GET /api/chat/link-preview, a relay route the local relay does not have) is dropped, so ChatLink is a
// plain anchor that main opens in the system browser.
import type { ReactNode } from "react";

// What counts as a link: a scheme or "www." is unambiguous; bare domains only for a list of TLDs people
// actually paste stream and clip links from.
// A regex LITERAL, not new RegExp(`...`): in a template literal `\s` and `\.`
// are unknown escapes and collapse to `s` and `.`, which silently turns
// [^\s]+ into "anything but the letter s". Worth the inlined TLD list.
const LINK_WORD =
  /^(?<lead>[([{"'<]*)(?<url>(?:https?:\/\/|www\.)[^\s]+|[a-z0-9-]+(?:\.[a-z0-9-]+)*\.(?:com|net|org|tv|gg|io|co|me|dev|app|live|xyz|link|to|be|ly|lol|gl|gd|cc|fm|ai|sh|im|uk|de|fr|nl|pl|se|no|fi|cz|ro|ch|ca|au|br|es|jp|ru)(?:\/[^\s]*)?)$/i;

// Punctuation a sentence puts after a URL, which is not part of it.
const LINK_TRAIL = /[.,!?;:)\]}'"]+$/;

/** The word as a link, or null. Splits off punctuation the URL does not own. */
export function asLink(word: string): { lead: string; href: string; text: string; trail: string } | null {
  const m = LINK_WORD.exec(word);
  if (!m || !m.groups) return null;
  let text = m.groups.url;
  let trail = "";
  const t = LINK_TRAIL.exec(text);
  // Only trim trailing punctuation when it is not load-bearing: a closing
  // paren inside a real URL (Wikipedia does this) is kept when the URL also
  // opened one.
  if (t) {
    const cut = text.slice(0, -t[0].length);
    if (cut && !(t[0] === ")" && (cut.match(/\(/g) || []).length > (cut.match(/\)/g) || []).length)) {
      trail = t[0];
      text = cut;
    }
  }
  if (!text) return null;
  const href = /^https?:\/\//i.test(text) ? text : "https://" + text;
  // Never build an href out of anything but http(s) — the pattern above should
  // already guarantee it, and this is the line that has to be right anyway.
  if (!/^https?:\/\//i.test(href)) return null;
  return { lead: m.groups.lead || "", href, text, trail };
}

// ---------------------------------------------------------------------------
// Mentions
// ---------------------------------------------------------------------------

/** A run of the characters a login can contain. */
export const LOGIN_RUN = /[A-Za-z0-9_]+/g;

/** One word split into leading punctuation, the name, and trailing punctuation. */
export const MENTION_WORD = /^([^A-Za-z0-9_]*)([A-Za-z0-9_]+)([^A-Za-z0-9_]*)$/;

/**
 * Is this message addressed at `loginLower`?
 *
 * Every run of login-legal characters, not every whitespace-separated word.
 * Splitting on spaces alone missed "@name," and "name:" — punctuation after a
 * name is how people actually write, and those are exactly the messages
 * someone is being addressed in.
 */
export function textMentionsLogin(text: string, loginLower: string): boolean {
  if (!loginLower) return false;
  for (const run of text.match(LOGIN_RUN) || []) {
    if (run.toLowerCase() === loginLower) return true;
  }
  return false;
}


export function ChatLink({ href, text }: { href: string; text: string }) {
  return (
    <a
      href={href}
      target="_blank"
      // noreferrer as well as noopener: a chat link should not tell the far side which app it was clicked from.
      rel="noreferrer noopener nofollow"
      title={href}
      className="break-words text-sky-400 hover:text-sky-300 hover:underline hover:underline-offset-2"
    >
      {text}
    </a>
  );
}

/**
 * Render one plain-text run of a message: links become anchors, "@name" becomes a mention.
 *
 * For the co-chat rows, which have no chatter map to resolve a mention
 * against — the Twitch column tokenizes mentions itself in buildParts, where
 * it can attach the person's colour and make the chip clickable. Here a
 * mention is styling only, which is still the useful 90%: it is how you spot
 * that a message is aimed at someone while scrolling.
 *
 * No "mentions you" variant here on purpose: a viewer's Kick or Rumble name is
 * not their Twitch login, so highlighting one against the other would mark the
 * wrong messages. That needs the per-platform username plumbed through first.
 */
export function TextRun({ text }: { text: string }) {
  const out: ReactNode[] = [];
  // Split KEEPING the whitespace, so the message reads exactly as typed.
  for (const [i, w] of text.split(/(\s+)/).entries()) {
    if (!w) continue;
    if (/^\s+$/.test(w)) {
      out.push(<span key={i}>{w}</span>);
      continue;
    }
    const link = asLink(w);
    if (link) {
      if (link.lead) out.push(<span key={`${i}l`}>{link.lead}</span>);
      out.push(<ChatLink key={i} href={link.href} text={link.text} />);
      if (link.trail) out.push(<span key={`${i}t`}>{link.trail}</span>);
      continue;
    }
    // Only an explicit "@" is a mention here. Without a chatter list there is
    // nothing to check a bare word against, and treating every word as a
    // possible name would light up half the message.
    const m = w.startsWith("@") ? MENTION_WORD.exec(w) : null;
    if (m) {
      out.push(
        <span key={i} className="font-semibold text-text">
          @{m[2]}
        </span>
      );
      if (m[3]) out.push(<span key={`${i}t`}>{m[3]}</span>);
      continue;
    }
    out.push(<span key={i}>{w}</span>);
  }
  return <>{out}</>;
}
