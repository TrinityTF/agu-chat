// Turns whatever the user typed into a Twitch login.
const LOGIN_RE = /^[a-z0-9_]{1,25}$/;
// twitch.tv paths that are not channels.
const NOT_CHANNELS = new Set(["videos", "directory", "downloads", "settings", "p", "jobs", "store", "turbo", "subscriptions", "wallet"]);

/** "login", "#login", "@login", "twitch.tv/login", "https://www.twitch.tv/popout/login/chat" -> "login". Throws Error(userMessage). */
export function normalizeChannel(input) {
  let v = String(input ?? "").trim();
  if (!v) throw new Error("Enter a Twitch channel name.");
  const url = /^(?:https?:\/\/)?(?:www\.|m\.|go\.)?twitch\.tv\/(?:popout\/|moderator\/|embed\/)?([^/?#\s]+)/i.exec(v);
  if (url) v = url[1];
  v = v.replace(/^[#@]+/, "").toLowerCase();
  if (!LOGIN_RE.test(v) || NOT_CHANNELS.has(v)) throw new Error(`"${String(input).trim().slice(0, 40)}" is not a Twitch channel name (letters, digits and underscores, up to 25).`);
  return v;
}
