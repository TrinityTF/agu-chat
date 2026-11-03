// Vendored from modtools.agu.lol render/lib/hiddenChatters.js (render commit 7704aec): chatterKey, cleanName, editDistance, closeness, suggestChatters.
// Changes: only those functions (the hidden-chatters list/filter half is not needed here); HIDDEN_PLATFORMS inlined.

const HIDDEN_PLATFORMS = ["twitch", "kick", "youtube", "rumble", "x"];

export function chatterKey(name) {
  if (typeof name !== "string") return "";
  return cleanName(name).toLowerCase();
}

function cleanName(name) {
  return name.trim().replace(/^@+/, "").replace(/\s+/g, " ").trim();
}

// Edit distance, given up past `max` — only "is this a typo of that" is asked.
function editDistance(a, b, max) {
  if (Math.abs(a.length - b.length) > max) return max + 1;
  let prev = Array.from({ length: b.length + 1 }, (_, i) => i);
  for (let i = 1; i <= a.length; i++) {
    const cur = [i];
    let best = i;
    for (let j = 1; j <= b.length; j++) {
      cur[j] = Math.min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
      if (cur[j] < best) best = cur[j];
    }
    if (best > max) return max + 1;
    prev = cur;
  }
  return prev[b.length];
}

// How close a chatter's key is to what was typed; lower is closer, null is
// not close at all. Exact, then starts-with, then contains, then a typo of it
// (the whole name, or its first q.length letters, a few edits away).
function closeness(key, q) {
  if (!key) return null;
  if (key === q) return 0;
  if (key.startsWith(q)) return 1;
  if (key.includes(q)) return 2;
  if (q.length < 3) return null;
  const max = q.length < 6 ? 1 : 2;
  const d = Math.min(editDistance(key, q, max), editDistance(key.slice(0, q.length), q, max));
  return d <= max ? 3 + d : null;
}

// The chatters closest to `query` across the channel's chats, one row per
// account per platform — the same name on two platforms is two rows, so a
// streamer can hide just one of them. `pools` is [{ platform, messages }]
// straight from the relays' backlogs; within a closeness, whoever spoke most
// recently comes first.
//
// `hide` is the name to store: the display name when it is just the login in
// other letters (reads better), else the login (a localized display name can
// change; the login is what the account is).
export function suggestChatters(query, pools, limit = 8) {
  const q = chatterKey(query);
  if (!q) return [];
  const best = new Map();
  for (const pool of pools || []) {
    if (!pool || !HIDDEN_PLATFORMS.includes(pool.platform)) continue;
    for (const m of pool.messages || []) {
      const u = m && m.user;
      if (!u || typeof u !== "object") continue;
      const login = chatterKey(u.login);
      const nameKey = chatterKey(u.name);
      const id = pool.platform + "|" + (login || nameKey);
      if (!login && !nameKey) continue;
      const scores = [closeness(login, q), closeness(nameKey, q)].filter((s) => s !== null);
      if (!scores.length) continue;
      const score = Math.min(...scores);
      const ts = Number(m.ts) || 0;
      const was = best.get(id);
      if (was && was.score <= score && was.ts >= ts) continue;
      const name = cleanName(String(u.name || u.login || ""));
      const hide = !login || nameKey === login ? name : cleanName(String(u.login));
      best.set(id, {
        score: was ? Math.min(was.score, score) : score,
        ts: was ? Math.max(was.ts, ts) : ts,
        row: { name, login: login && login !== nameKey ? cleanName(String(u.login)) : "", hide, platform: pool.platform },
      });
    }
  }
  return [...best.values()]
    .sort((a, b) => a.score - b.score || b.ts - a.ts)
    .slice(0, limit)
    .map((b) => b.row);
}
