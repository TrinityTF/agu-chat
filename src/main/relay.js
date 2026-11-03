// The in-process "local relay": ref-counted live sources plus the api router.
// See docs/ARCHITECTURE.md ("Sources", "The api router").
import { randomUUID } from "node:crypto";

const LINGER_MS = 30_000;

export class SourceManager {
  /** @param {Record<string, any>} modules platform -> module; @param ctx shared context; @param send (subId, frame) => void */
  constructor(modules, ctx, send) {
    this.modules = modules;
    this.ctx = ctx;
    this.send = send;
    this.sources = new Map(); // key -> { source, subs:Set<subId>, lingerTimer }
    this.subs = new Map(); // subId -> { key, owner }
  }

  keyOf(ref) {
    const mod = this.modules[ref?.platform];
    if (!mod) throw new Error(`Unknown platform: ${ref?.platform}`);
    return { mod, key: `${ref.platform}:${mod.normalizeChannel(String(ref.channel || ""))}` };
  }

  subscribe(ref, owner) {
    const { mod, key } = this.keyOf(ref);
    let entry = this.sources.get(key);
    if (!entry) {
      const channel = key.slice(ref.platform.length + 1);
      entry = { subs: new Set(), lingerTimer: null, source: null };
      entry.source = mod.createSource({
        channel,
        ctx: this.ctx,
        emit: (frame) => {
          for (const subId of entry.subs) this.send(subId, frame);
        },
      });
      this.sources.set(key, entry);
      entry.source.start();
    }
    clearTimeout(entry.lingerTimer);
    entry.lingerTimer = null;
    const subId = randomUUID();
    entry.subs.add(subId);
    this.subs.set(subId, { key, owner });
    // Replay the current state so a late subscriber sees what everyone else sees.
    for (const frame of entry.source.snapshot() || []) this.send(subId, frame);
    return subId;
  }

  unsubscribe(subId) {
    const sub = this.subs.get(subId);
    if (!sub) return;
    this.subs.delete(subId);
    const entry = this.sources.get(sub.key);
    if (!entry) return;
    entry.subs.delete(subId);
    if (entry.subs.size === 0) {
      entry.lingerTimer = setTimeout(() => {
        if (entry.subs.size) return;
        this.sources.delete(sub.key);
        try {
          entry.source.stop();
        } catch (err) {
          this.ctx.log("[relay] stop failed", sub.key, err);
        }
      }, LINGER_MS);
    }
  }

  /** Drop every subscription a closed window held. */
  dropOwner(owner) {
    for (const [subId, sub] of this.subs) if (sub.owner === owner) this.unsubscribe(subId);
  }

  stopAll() {
    for (const entry of this.sources.values()) {
      clearTimeout(entry.lingerTimer);
      try {
        entry.source.stop();
      } catch {}
    }
    this.sources.clear();
    this.subs.clear();
  }
}

/** Routes "METHOD /path" to the platform modules' handlers. */
export function makeApiRouter(modules, ctx) {
  const table = new Map();
  for (const mod of Object.values(modules)) {
    for (const [route, handler] of Object.entries(mod.routes || {})) table.set(route, handler);
  }
  return async function api(source, path, init = {}) {
    const method = String(init.method || "GET").toUpperCase();
    const url = new URL(String(path), "http://local");
    const handler = table.get(`${method} ${url.pathname}`) || table.get(`* ${url.pathname}`);
    if (!handler) return { status: 404, body: { error: "not found" } };
    let body = init.body;
    if (typeof body === "string") {
      try {
        body = JSON.parse(body);
      } catch {}
    }
    try {
      const res = await handler({ source, method, query: url.searchParams, body: body ?? null, ctx });
      return { status: res?.status ?? 200, body: res?.body ?? null };
    } catch (err) {
      ctx.log("[api]", method, url.pathname, err);
      return { status: err?.status || 500, body: { error: String(err?.message || err) } };
    }
  };
}
