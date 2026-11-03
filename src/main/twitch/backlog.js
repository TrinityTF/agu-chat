// Chat history for a freshly opened source: recent-messages.robotty.de keeps the last
// few hundred raw IRC lines of a channel. We shape them into the same message / notice
// objects the live path builds, marked `historical: true` so the UI can dim them.
// The service is a courtesy: down, slow or empty just means no history.
import { parseIrcLine, buildChatMsg, buildNotice } from "./vendor/irc.js";
import { requestJson } from "./http.js";
import { log } from "./rt.js";

const BASE = "https://recent-messages.robotty.de/api/v2/recent-messages";

/** Raw lines for a channel, or [] on any failure. One retry when the service says it only just started tracking the channel. */
export async function fetchRecent(login, { limit = 200, retryDelayMs = 3000 } = {}) {
  for (let attempt = 0; attempt < 2; attempt++) {
    try {
      const data = await requestJson(`${BASE}/${encodeURIComponent(login)}?limit=${limit}`, { timeoutMs: 8000 });
      const lines = Array.isArray(data?.messages) ? data.messages : [];
      if (lines.length || !data?.error_code || attempt) return lines;
    } catch (err) {
      log(`recent-messages unavailable for ${login}: ${err.message}`);
      return [];
    }
    await new Promise((r) => setTimeout(r, retryDelayMs));
  }
  return [];
}

/** Raw lines -> {entries, roomId}. Entries are chat messages and `{...notice, notice:true}` rows in time order. */
export function shapeRecent(lines) {
  const entries = [];
  let roomId = null;
  for (const line of lines) {
    let m;
    try {
      m = parseIrcLine(line);
    } catch {
      continue;
    }
    if (m.tags["room-id"]) roomId = m.tags["room-id"];
    const stamp = Number(m.tags["tmi-sent-ts"]) || Number(m.tags["rm-received-ts"]) || Date.now();
    if (m.command === "PRIVMSG") {
      const msg = buildChatMsg(m, m.tags["display-name"] || m.user);
      msg.ts = stamp;
      msg.historical = true;
      entries.push(msg);
    } else if (m.command === "USERNOTICE") {
      const notice = buildNotice(m);
      if (notice) entries.push({ ...notice, ts: stamp, historical: true, notice: true });
    } else if (m.command === "CLEARCHAT") {
      const userId = m.tags["target-user-id"] || null;
      for (const e of entries) if (!e.notice && (!userId || e.user?.id === userId)) e.deleted = true;
    } else if (m.command === "CLEARMSG") {
      const id = m.tags["target-msg-id"];
      const hit = id && entries.find((e) => e.id === id);
      if (hit) hit.deleted = true;
    }
  }
  return { entries, roomId };
}
