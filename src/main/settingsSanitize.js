// Validation for settings values that are not plain scalars (lists of ignore rules). Pure: no Electron, so
// it can be unit tested with `node --test`. The renderer sends whole lists; anything malformed is dropped
// entry by entry rather than rejecting the patch, and a settings.json edited by hand is cleaned on load.

const PLATFORMS = new Set(["twitch", "kick", "youtube", "rumble", "x"]);
const MAX_ENTRIES = 1000;
const MAX_LEN = 500;

const str = (v, max = MAX_LEN) => (typeof v === "string" ? v.slice(0, max) : "");

/** `[{ platform, login }]`: login lowercased, leading @ dropped, duplicates removed. */
export function sanitizeIgnoredUsers(list) {
  if (!Array.isArray(list)) return null;
  const out = [];
  const seen = new Set();
  for (const e of list) {
    if (!e || typeof e !== "object" || !PLATFORMS.has(e.platform)) continue;
    const login = str(e.login, 100).trim().replace(/^@/, "").toLowerCase();
    if (!login) continue;
    const key = `${e.platform}:${login}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ platform: e.platform, login });
    if (out.length >= MAX_ENTRIES) break;
  }
  return out;
}

/** `[{ pattern, regex, caseSensitive, action: "hide" | "replace", replacement }]`. Empty patterns are dropped. */
export function sanitizeIgnoredPhrases(list) {
  if (!Array.isArray(list)) return null;
  const out = [];
  for (const e of list) {
    if (!e || typeof e !== "object") continue;
    const pattern = str(e.pattern);
    if (!pattern.trim()) continue;
    out.push({
      pattern,
      regex: e.regex === true,
      caseSensitive: e.caseSensitive === true,
      action: e.action === "replace" ? "replace" : "hide",
      replacement: str(e.replacement),
    });
    if (out.length >= MAX_ENTRIES) break;
  }
  return out;
}

const LIST_SANITIZERS = {
  ignoredUsers: sanitizeIgnoredUsers,
  ignoredPhrases: sanitizeIgnoredPhrases,
};

/**
 * The value to store for `key`, or `undefined` to ignore it. Scalars must have the default's type (strings
 * are trimmed); arrays need a sanitizer for that key.
 */
export function sanitizeSetting(key, value, defaults) {
  if (!Object.prototype.hasOwnProperty.call(defaults, key)) return undefined;
  const def = defaults[key];
  if (Array.isArray(def)) {
    const fn = LIST_SANITIZERS[key];
    const v = fn ? fn(value) : null;
    return v === null ? undefined : v;
  }
  if (value === null || typeof value !== typeof def || Array.isArray(value)) return undefined;
  return typeof value === "string" ? value.trim() : value;
}

/** Defaults overlaid with whatever valid keys a stored settings object has. */
export function loadSettings(stored, defaults) {
  const out = { ...defaults };
  if (!stored || typeof stored !== "object") return out;
  for (const [k, v] of Object.entries(stored)) {
    const s = sanitizeSetting(k, v, defaults);
    if (s !== undefined) out[k] = s;
  }
  return out;
}
