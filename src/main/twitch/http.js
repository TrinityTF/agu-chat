// One small JSON-over-fetch helper for every Twitch HTTP call (id.twitch.tv, Helix,
// recent-messages). Always sends the app's honest User-Agent.
import { userAgent } from "./rt.js";

export class HttpError extends Error {
  constructor(status, message, body) {
    super(message);
    this.status = status; // 0 = never reached the server
    this.body = body ?? null;
  }
}

export async function requestJson(url, { method = "GET", headers = {}, json, form, timeoutMs = 15_000 } = {}) {
  const h = { "User-Agent": userAgent(), Accept: "application/json", ...headers };
  let body;
  if (form) {
    h["Content-Type"] = "application/x-www-form-urlencoded";
    const clean = {};
    for (const [k, v] of Object.entries(form)) if (v !== undefined && v !== null) clean[k] = String(v);
    body = new URLSearchParams(clean).toString();
  } else if (json !== undefined) {
    h["Content-Type"] = "application/json";
    body = JSON.stringify(json);
  }
  let res;
  try {
    res = await fetch(url, { method, headers: h, body, signal: AbortSignal.timeout(timeoutMs) });
  } catch (err) {
    throw new HttpError(0, `Could not reach Twitch (${err?.cause?.code || err?.name || err?.message})`);
  }
  const text = await res.text();
  let data = null;
  if (text) {
    try {
      data = JSON.parse(text);
    } catch {
      data = null;
    }
  }
  if (!res.ok) {
    throw new HttpError(res.status, (data && (data.message || data.error)) || text.slice(0, 200) || `HTTP ${res.status}`, data);
  }
  return res.status === 204 ? null : data;
}
