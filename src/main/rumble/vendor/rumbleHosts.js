// Vendored from modtools.agu.lol render/lib/rumbleHosts.js (render commit 7704aec). Changes: none (header only).
// Which of Rumble's front-end hosts to ask, and in what order.
//
// Every Rumble request in this codebase goes to a front-end host rather than
// rumble.com, because the apex answers Node with Cloudflare's challenge (see
// lib/rumble.js). Rumble decides per HOSTNAME what each front-end serves, and
// changes it without notice: on 2026-10-27 web6-web9 — the hosts this code had
// used for months — started resetting every connection except web7's
// /chat/api/*, while wn0 kept serving everything.
//
// Measured that day (plain fetch from the VM, and agreeing with a real
// browser): wn0 serves channel pages, embedJS, service.php, the chat popup and
// the chat API; web7 serves only /chat/api/*; web1-web27 reset everything else;
// web40 doesn't answer; wn1+ don't exist. So the list is wn0, web7, then the
// rest — dead today, but tried after wn0 and web7 fail, so the day Rumble
// reopens one this finds it without a deploy.
//
// What a host serves differs between the chat API and everything else, so
// health is tracked per kind: web7 failing a channel page must not stop it
// being used for chat.

export const RUMBLE_HOSTS = ["wn0", "web7", ...Array.from({ length: 27 }, (_, i) => `web${i + 1}`).filter((h) => h !== "web7")];

// A host that failed is skipped for this long (unless it was the last one that
// worked — see hostsFor).
const DOWN_MS = 10 * 60_000;
// Hosts tried per request. A closed host fails in ~400ms, so three is ~1s of
// cost when everything is down, and a dead list is walked a few hosts per
// request rather than all at once.
const MAX_TRIES = 3;

const down = new Map(); // "kind:host" -> skip until (ms)
const lastGood = new Map(); // kind -> host

export function hostKind(path) {
  return String(path).startsWith("/chat/api/") ? "chat" : "site";
}

/**
 * Hosts to try for this kind, in order: the last one that worked first —
 * even if it has since failed once, because one timeout from the host that
 * has been serving everything is far likelier a blip than a lockdown — then
 * every other host that hasn't failed recently.
 */
export function hostsFor(kind, now = Date.now()) {
  const first = lastGood.get(kind) || RUMBLE_HOSTS[0];
  const rest = RUMBLE_HOSTS.filter((h) => h !== first && !((down.get(`${kind}:${h}`) || 0) > now));
  return [first, ...rest];
}

export function markHost(host, kind, ok, now = Date.now()) {
  if (ok) {
    down.delete(`${kind}:${host}`);
    lastGood.set(kind, host);
  } else {
    down.set(`${kind}:${host}`, now + DOWN_MS);
  }
}

/** For tests. */
export function resetHostHealth() {
  down.clear();
  lastGood.clear();
}

// What a closed host looks like from undici: the connection is reset or
// dropped before any response. Anything that came back as an HTTP response —
// a 404, a 403 — is an answer, not a dead host, and is returned to the caller.
const CONNECTION_CODES = new Set(["ECONNRESET", "ECONNREFUSED", "ENOTFOUND", "EAI_AGAIN", "UND_ERR_SOCKET", "UND_ERR_CONNECT_TIMEOUT", "ETIMEDOUT"]);

function connectionRefused(e) {
  const code = e && e.cause && e.cause.code;
  return CONNECTION_CODES.has(code);
}

/**
 * fetchImpl(`https://<host>.rumble.com${path}`, init) on the first host that
 * answers. Throws the last failure when none did.
 *
 * A write (anything but GET) that TIMED OUT is not retried on another host:
 * it may have landed, and a chat message sent twice is worse than one that
 * reports an error. A refused connection never reached Rumble, so it is safe
 * to repeat anywhere.
 */
export async function fetchFromRumble(path, init, fetchImpl) {
  const kind = hostKind(path);
  const write = String((init && init.method) || "GET").toUpperCase() !== "GET";
  let last = null;
  for (const host of hostsFor(kind).slice(0, MAX_TRIES)) {
    try {
      const res = await fetchImpl(`https://${host}.rumble.com${path}`, init);
      markHost(host, kind, true);
      return res;
    } catch (e) {
      if (init && init.signal && init.signal.aborted) throw e; // the caller hung up
      const refused = connectionRefused(e);
      const timedOut = !refused && e && (e.name === "AbortError" || e.name === "TimeoutError");
      if (!refused && !timedOut) throw e; // not a host problem
      markHost(host, kind, false);
      last = e;
      if (timedOut && write) break;
    }
  }
  throw last;
}
