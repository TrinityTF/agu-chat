// Kept apart from parts.ts (which pulls in React helpers) so node --test can load it.
import type { MsgPart } from "./parts";

/** A reply's text starts with "@parent": the reply line above already says who, so the row drops it. */
export function dropReplyMention(parts: MsgPart[], parent: string): MsgPart[] {
  const who = parent.toLowerCase();
  const first = parts[0];
  if (!first) return parts;
  if (first.kind === "mention" && (first.login.toLowerCase() === who || first.name.toLowerCase() === who)) {
    const rest = parts.slice(1);
    const next = rest[0];
    if (next?.kind === "text") {
      const t = next.text.replace(/^\s+/, "");
      return t ? [{ kind: "text", text: t }, ...rest.slice(1)] : rest.slice(1);
    }
    return rest;
  }
  if (first.kind === "text") {
    const m = /^@(\S+)\s*/.exec(first.text);
    if (m && m[1].replace(/[,:]$/, "").toLowerCase() === who) {
      const t = first.text.slice(m[0].length);
      return t ? [{ kind: "text", text: t }, ...parts.slice(1)] : parts.slice(1);
    }
  }
  return parts;
}
