// Validation for the `highlightRules` setting (Settings > Highlights). No Electron imports, so
// `node --test` can load it. Keep the shape in sync with HighlightRule in src/renderer/protocol.ts.

export const MAX_RULES = 200;
export const MAX_PATTERN = 500;
const KINDS = new Set(["phrase", "regex", "user"]);
const COLOR = /^#[0-9a-f]{6}$/i;
// A sound file is a bare name inside userData/sounds (see sounds.js), never a path.
const SOUND_FILE = /^[A-Za-z0-9._ -]{1,120}$/;

/** One rule cleaned up, or null when it is not a rule at all. */
export function sanitizeHighlightRule(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const id = typeof raw.id === "string" ? raw.id.slice(0, 64) : "";
  if (!id) return null;
  const soundFile = typeof raw.soundFile === "string" && SOUND_FILE.test(raw.soundFile) && !raw.soundFile.startsWith(".") ? raw.soundFile : "";
  return {
    id,
    enabled: raw.enabled !== false,
    kind: KINDS.has(raw.kind) ? raw.kind : "phrase",
    pattern: typeof raw.pattern === "string" ? raw.pattern.slice(0, MAX_PATTERN) : "",
    caseSensitive: raw.caseSensitive === true,
    color: typeof raw.color === "string" && COLOR.test(raw.color) ? raw.color.toLowerCase() : "#9147ff",
    sound: raw.sound === true,
    soundFile,
    flash: raw.flash === true,
  };
}

/** The whole list cleaned up (bad entries and duplicate ids dropped), or undefined when it is not a list. */
export function sanitizeHighlightRules(value) {
  if (!Array.isArray(value)) return undefined;
  const out = [];
  const ids = new Set();
  for (const raw of value) {
    const rule = sanitizeHighlightRule(raw);
    if (!rule || ids.has(rule.id)) continue;
    ids.add(rule.id);
    out.push(rule);
    if (out.length >= MAX_RULES) break;
  }
  return out;
}

export function isSoundFileName(name) {
  return typeof name === "string" && SOUND_FILE.test(name) && !name.startsWith(".");
}
