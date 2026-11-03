// The per-pane message store: a frame reducer for every source of a split, merged into ONE ordered row
// list. Frame handling is ported from the socket handler in modtools.agu.lol web/src/app/pages/Chat.tsx
// (commit 1b826c2: chatlog / chat / chatnotice / rewardinfo / clearchat / clearmsg / pin / modfeed /
// modes / automod / automodupdate / suspicious / suspicioususer / esub) and from the useRumbleChat,
// useKickChat, useYouTubeChat and useXChat hooks. Changes: one store for all sources instead of one
// useState per platform, rows are immutable objects (a delete REPLACES the row so memoized rows notice),
// updates are batched to one notification per animation frame.
import type { ConnFrame, Frame, Platform, SourceRef } from "../protocol";
import type {
  ChatMsg,
  EsubStatus,
  HeldMsg,
  KickEmotes,
  KickMsg,
  Notice,
  RewardInfo,
  Row,
  RumbleBadgeDefs,
  RumbleEmotes,
  RumbleMsg,
  SuspiciousUser,
  XMsg,
  YouTubeMsg,
} from "./types";
import type { KnownChatter } from "./twitch/parts";

export const sourceKey = (s: SourceRef) => `${s.platform}:${s.channel}`;

export type SourceState = {
  sk: string;
  ref: SourceRef;
  conn: ConnFrame | null;
  /** Twitch channel id (7TV/BTTV/FFZ are keyed by it); from a frame's `channelId`. */
  channelId: string | null;
  loaded: boolean;
  pinned: ChatMsg | null;
  suspicious: Map<string, SuspiciousUser>;
  esub: EsubStatus;
  modes: any;
  status: any;
  rumbleBadges: RumbleBadgeDefs;
  rumbleEmotes: RumbleEmotes;
  kickEmotes: KickEmotes;
  known: Map<string, KnownChatter>;
};

export type Snapshot = { version: number; rows: Row[] };

function newState(ref: SourceRef): SourceState {
  return {
    sk: sourceKey(ref),
    ref,
    conn: null,
    channelId: null,
    loaded: false,
    pinned: null,
    suspicious: new Map(),
    esub: { active: false, reason: null },
    modes: null,
    status: null,
    rumbleBadges: {},
    rumbleEmotes: {},
    kickEmotes: {},
    known: new Map(),
  };
}

const ROW_CAP = 1000;
const ROW_CAP_HELD = 1600; // while the user has scrolled up, trimming from the top would move what they are reading

export class ChatStore {
  sources = new Map<string, SourceState>();
  private rows: Row[] = [];
  private keys = new Set<string>();
  private version = 0;
  private snap: Snapshot = { version: 0, rows: [] };
  private listeners = new Set<() => void>();
  private raf = 0;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private seq = 0;
  private arrival = 0;
  private held = false;

  subscribe = (cb: () => void) => {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  };

  getSnapshot = (): Snapshot => {
    if (this.snap.version !== this.version) this.snap = { version: this.version, rows: this.rows.slice() };
    return this.snap;
  };

  /** Make the store hold exactly these sources: new ones get state, dropped ones lose their rows. */
  sync(refs: SourceRef[]) {
    const want = new Map(refs.map((r) => [sourceKey(r), r]));
    let changed = false;
    for (const sk of [...this.sources.keys()]) {
      if (!want.has(sk)) {
        this.sources.delete(sk);
        this.removeRowsOf(sk);
        changed = true;
      }
    }
    for (const [sk, ref] of want) {
      if (!this.sources.has(sk)) {
        this.sources.set(sk, newState(ref));
        changed = true;
      }
    }
    if (changed) this.touch();
  }

  /** A fresh subscription replays a snapshot: forget what we knew about this source's chat first. */
  resetSource(sk: string) {
    const st = this.sources.get(sk);
    if (!st) return;
    this.removeRowsOf(sk);
    st.loaded = false;
    st.pinned = null;
    st.suspicious = new Map();
    this.touch();
  }

  setHeld(held: boolean) {
    if (this.held === held) return;
    this.held = held;
    if (!held) {
      this.trim();
      this.touch();
    }
  }

  /** A channel id learned from an API answer rather than a frame (7TV/BTTV/FFZ need it). */
  setChannelId(sk: string, id: string) {
    const st = this.sources.get(sk);
    if (!st || st.channelId === id) return;
    st.channelId = id;
    this.touch();
  }

  /** Local edits (e.g. a delete the user just did): mark a Twitch message deleted. */
  markDeleted(sk: string, id: string) {
    this.mapRows((r) => (r.sk === sk && r.k === "msg" && r.m.id === id && !r.m.deleted ? { ...r, m: { ...r.m, deleted: true } } : r));
  }

  /** The relay says an AutoMod hold is gone (404): no broadcast will follow, so flip it here. */
  markAutomodExpired(sk: string, id: string) {
    this.mapRows((r) => (r.sk === sk && r.k === "automod" && r.h.id === id && r.h.status === "held" ? { ...r, h: { ...r.h, status: "expired" } } : r));
  }

  // ---- internals --------------------------------------------------------------------------------

  private touch() {
    this.version++;
    if (this.raf || this.timer) return;
    const run = () => {
      this.raf = 0;
      this.timer = null;
      for (const l of this.listeners) l();
    };
    // requestAnimationFrame does not run in a hidden window; a timer keeps the list current there.
    if (typeof requestAnimationFrame === "function" && !document.hidden) this.raf = requestAnimationFrame(run);
    else this.timer = setTimeout(run, 50);
  }

  private removeRowsOf(sk: string) {
    this.rows = this.rows.filter((r) => {
      if (r.sk !== sk) return true;
      this.keys.delete(r.key);
      return false;
    });
  }

  private trim() {
    const cap = this.held ? ROW_CAP_HELD : ROW_CAP;
    if (this.rows.length <= cap) return;
    const drop = this.rows.splice(0, this.rows.length - cap);
    for (const r of drop) this.keys.delete(r.key);
  }

  private mapRows(fn: (r: Row) => Row) {
    let changed = false;
    for (let i = 0; i < this.rows.length; i++) {
      const next = fn(this.rows[i]);
      if (next !== this.rows[i]) {
        this.rows[i] = next;
        changed = true;
      }
    }
    if (changed) this.touch();
  }

  private addRow(row: Row) {
    if (this.keys.has(row.key)) return;
    this.keys.add(row.key);
    row.ord = ++this.arrival;
    const rows = this.rows;
    if (!rows.length || row.ts >= rows[rows.length - 1].ts) rows.push(row);
    else {
      // Out of order (a co-chat's batch, a late backlog line): upper-bound insert by ts.
      let lo = 0;
      let hi = rows.length;
      while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (rows[mid].ts <= row.ts) lo = mid + 1;
        else hi = mid;
      }
      rows.splice(lo, 0, row);
    }
  }

  private replaceBacklog(sk: string, rows: Row[]) {
    this.removeRowsOf(sk);
    rows.sort((a, b) => a.ts - b.ts);
    for (const r of rows) this.addRow(r);
    this.trim();
  }

  private register(st: SourceState, u: { id: string; login: string; name: string; color?: string } | undefined) {
    if (!u?.login) return;
    st.known.set(u.login.toLowerCase(), { name: u.name, color: u.color || "", id: u.id });
  }

  // ---- frames -----------------------------------------------------------------------------------

  apply(sk: string, frame: Frame) {
    const st = this.sources.get(sk);
    if (!st || !frame || typeof (frame as any).type !== "string") return;
    const f = frame as Record<string, any>;
    if (f.channelId != null && st.channelId !== String(f.channelId)) st.channelId = String(f.channelId);
    if (f.type === "conn") {
      st.conn = f as ConnFrame;
      this.touch();
      return;
    }
    switch (st.ref.platform) {
      case "twitch":
        return this.twitch(st, f);
      case "rumble":
        return this.rumble(st, f);
      case "kick":
        return this.kick(st, f);
      case "youtube":
        return this.youtube(st, f);
      case "x":
        return this.xchat(st, f);
    }
  }

  private mk(st: SourceState, prefix: string, id: string): string {
    return `${st.sk}|${prefix}:${id}`;
  }

  private msgRow(st: SourceState, m: ChatMsg): Row {
    return { key: this.mk(st, "m", m.id), ts: m.ts, sk: st.sk, platform: "twitch", k: "msg", m };
  }
  private noticeRow(st: SourceState, n: Notice): Row {
    return { key: this.mk(st, "n", n.id), ts: n.ts, sk: st.sk, platform: "twitch", k: "notice", n };
  }
  private feedRow(st: SourceState, f: { ts: number; kind?: string; feedKind?: string; text: string }): Row {
    return {
      key: this.mk(st, "f", String(++this.seq)),
      ts: f.ts,
      sk: st.sk,
      platform: "twitch",
      k: "modfeed",
      f: { ts: f.ts, feedKind: (f.feedKind || f.kind || "mode") as any, text: f.text },
    };
  }
  private automodRow(st: SourceState, h: HeldMsg): Row {
    return { key: this.mk(st, "a", h.id), ts: h.ts, sk: st.sk, platform: "twitch", k: "automod", h };
  }

  private twitch(st: SourceState, f: Record<string, any>) {
    switch (f.type) {
      case "chatlog": {
        const rows: Row[] = [];
        for (const raw of (f.messages || []) as any[]) {
          if (raw && raw.notice) {
            const { notice: _n, ...rest } = raw;
            this.register(st, rest.user);
            rows.push(this.noticeRow(st, rest as Notice));
          } else if (raw) {
            this.register(st, raw.user);
            rows.push(this.msgRow(st, raw as ChatMsg));
          }
        }
        for (const x of (f.modfeed || []) as any[]) rows.push(this.feedRow(st, x));
        for (const h of (f.automod || []) as HeldMsg[]) rows.push(this.automodRow(st, h));
        this.replaceBacklog(st.sk, rows);
        st.suspicious = new Map(((f.suspicious || []) as SuspiciousUser[]).map((u) => [u.id, u]));
        st.esub = (f.esub as EsubStatus | undefined) ?? { active: false, reason: null };
        st.pinned = (f.pinned as ChatMsg | null | undefined) ?? null;
        st.loaded = true;
        break;
      }
      case "chat":
        if (!f.msg) return;
        this.register(st, f.msg.user);
        this.addRow(this.msgRow(st, f.msg as ChatMsg));
        this.trim();
        break;
      case "chatnotice":
        if (!f.notice) return;
        this.register(st, f.notice.user);
        this.addRow(this.noticeRow(st, f.notice as Notice));
        this.trim();
        break;
      // A reward the relay learned after its redeem's message went out.
      case "rewardinfo":
        this.mapRows((r) =>
          r.sk === st.sk && r.k === "msg" && r.m.rewardId === f.rewardId && !r.m.reward ? { ...r, m: { ...r.m, reward: f.reward as RewardInfo } } : r,
        );
        return;
      case "clearchat":
        this.mapRows((r) => {
          if (r.sk !== st.sk || r.k !== "msg" || r.m.deleted) return r;
          // Same test as the site: no user = the whole chat was cleared.
          if (f.userId == null || r.m.user.id === f.userId || (f.login && r.m.user.login === f.login)) return { ...r, m: { ...r.m, deleted: true } };
          return r;
        });
        return;
      case "clearmsg":
        this.mapRows((r) => (r.sk === st.sk && r.k === "msg" && r.m.id === f.id && !r.m.deleted ? { ...r, m: { ...r.m, deleted: true } } : r));
        return;
      case "pin":
        st.pinned = (f.msg as ChatMsg | null) ?? null;
        break;
      case "modfeed":
        if (!f.feed) return;
        this.addRow(this.feedRow(st, f.feed));
        this.trim();
        break;
      case "modes":
        st.modes = f.settings;
        break;
      case "automod":
        if (!f.held) return;
        this.addRow(this.automodRow(st, f.held as HeldMsg));
        this.trim();
        break;
      // A held message got resolved (by us or another mod): flip its status in place.
      case "automodupdate":
        this.mapRows((r) =>
          r.sk === st.sk && r.k === "automod" && r.h.id === f.id ? { ...r, h: { ...r.h, status: f.status, resolvedBy: (f.resolvedBy as string | null) ?? null } } : r,
        );
        return;
      // A suspicious user spoke; `msg` is only non-null for restricted users (their messages never reach IRC).
      case "suspicious": {
        const u = f.user as SuspiciousUser;
        if (u) st.suspicious.set(u.id, u);
        if (f.msg) {
          this.register(st, f.msg.user);
          this.addRow(this.msgRow(st, f.msg as ChatMsg));
          this.trim();
        }
        break;
      }
      case "suspicioususer":
        if (f.user) st.suspicious.set(f.userId, f.user as SuspiciousUser);
        else st.suspicious.delete(f.userId);
        break;
      case "esub":
        st.esub = f.esub as EsubStatus;
        break;
      default:
        return;
    }
    this.touch();
  }

  private rumble(st: SourceState, f: Record<string, any>) {
    const row = (m: RumbleMsg): Row => ({ key: this.mk(st, "r", m.id), ts: m.ts, sk: st.sk, platform: "rumble", k: "rumble", m });
    switch (f.type) {
      case "chatlog":
        // Sent twice by design (empty on connect, again once history lands): replaces the list wholesale.
        this.replaceBacklog(st.sk, ((f.messages || []) as RumbleMsg[]).map(row));
        st.status = f.status ?? null;
        if (f.badges) st.rumbleBadges = f.badges;
        if (f.emotes) st.rumbleEmotes = f.emotes;
        st.loaded = true;
        break;
      case "chat": {
        const m = (f.msg ?? f.message) as RumbleMsg | undefined;
        if (!m) return;
        this.addRow(row(m));
        this.trim();
        break;
      }
      case "status":
        st.status = f.status ?? null;
        break;
      case "badges":
        st.rumbleBadges = f.badges || {};
        break;
      case "emotes":
        st.rumbleEmotes = f.emotes || {};
        break;
      case "delete": {
        const ids = new Set<string>(f.ids || []);
        this.mapRows((r) => (r.sk === st.sk && r.k === "rumble" && ids.has(r.m.id) && !r.m.deleted ? { ...r, m: { ...r.m, deleted: true } } : r));
        return;
      }
      case "mute": {
        const users = new Set<string>(f.userIds || []);
        this.mapRows((r) => (r.sk === st.sk && r.k === "rumble" && users.has(r.m.user.id) && !r.m.deleted ? { ...r, m: { ...r.m, deleted: true } } : r));
        return;
      }
      case "unconfigured":
        st.loaded = true;
        break;
      default:
        return;
    }
    this.touch();
  }

  private kick(st: SourceState, f: Record<string, any>) {
    const row = (m: KickMsg): Row => ({ key: this.mk(st, "k", m.id), ts: m.ts, sk: st.sk, platform: "kick", k: "kick", m });
    switch (f.type) {
      case "chatlog":
        this.replaceBacklog(st.sk, ((f.messages || []) as KickMsg[]).map(row));
        st.status = f.status ?? null;
        if (f.emotes) st.kickEmotes = f.emotes;
        st.loaded = true;
        break;
      case "chat": {
        // The site's Kick hook reads `data.message`, Rumble's reads `data.msg`: accept either.
        const m = (f.message ?? f.msg) as KickMsg | undefined;
        if (!m) return;
        this.addRow(row(m));
        this.trim();
        break;
      }
      case "status":
        st.status = f.status ?? null;
        break;
      case "emotes":
        st.kickEmotes = f.emotes || {};
        break;
      // Kick's ban event clears that user's history, matching what its own chat does.
      case "banned": {
        const uid = String(f.userId || "");
        this.mapRows((r) => (r.sk === st.sk && r.k === "kick" && r.m.user.id === uid && !r.m.deleted ? { ...r, m: { ...r.m, deleted: true } } : r));
        return;
      }
      case "unconfigured":
        st.loaded = true;
        break;
      default:
        return;
    }
    this.touch();
  }

  /** YouTube: a poll returns many messages, shipped together in `messages`. */
  private youtube(st: SourceState, f: Record<string, any>) {
    const row = (m: YouTubeMsg): Row => ({ key: this.mk(st, "y", m.id), ts: m.ts, sk: st.sk, platform: "youtube", k: "youtube", m });
    switch (f.type) {
      case "chatlog":
        this.replaceBacklog(st.sk, ((f.messages || []) as YouTubeMsg[]).map(row));
        st.status = f.status ?? null;
        st.loaded = true;
        break;
      case "chat":
        for (const m of (f.messages || []) as YouTubeMsg[]) this.addRow(row(m));
        this.trim();
        break;
      case "status":
        st.status = f.status ?? null;
        break;
      case "unconfigured":
        st.loaded = true;
        break;
      default:
        return;
    }
    this.touch();
  }

  private xchat(st: SourceState, f: Record<string, any>) {
    const row = (m: XMsg): Row => ({ key: this.mk(st, "x", m.id), ts: m.ts, sk: st.sk, platform: "x", k: "x", m });
    switch (f.type) {
      case "chatlog":
        this.replaceBacklog(st.sk, ((f.messages || []) as XMsg[]).map(row));
        st.status = f.status ?? null;
        st.loaded = true;
        break;
      case "chat":
        for (const m of (f.messages || []) as XMsg[]) this.addRow(row(m));
        this.trim();
        break;
      // X chat carries user ids, not names: when a name turns up it belongs on what is already on screen.
      case "names": {
        const users: Record<string, XMsg["user"]> = {};
        for (const u of (f.users || []) as XMsg["user"][]) users[u.id] = u;
        if (!Object.keys(users).length) return;
        this.mapRows((r) => (r.sk === st.sk && r.k === "x" && users[r.m.user.id] ? { ...r, m: { ...r.m, user: { ...r.m.user, ...users[r.m.user.id] } } } : r));
        return;
      }
      case "status":
        st.status = f.status ?? null;
        break;
      case "unconfigured":
        st.loaded = true;
        break;
      default:
        return;
    }
    this.touch();
  }

  dispose() {
    if (this.raf) cancelAnimationFrame(this.raf);
    if (this.timer) clearTimeout(this.timer);
    this.raf = 0;
    this.timer = null;
    this.listeners.clear();
  }
}

/** Display names newest-first, for @name completion (the approach kickChatterNames/rumbleChatterNames take). */
export function chatterNames(rows: Row[], sks?: Set<string>): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (let i = rows.length - 1; i >= 0 && out.length < 400; i--) {
    const r = rows[i];
    if (sks && !sks.has(r.sk)) continue;
    let name = "";
    if (r.k === "msg" || r.k === "rumble" || r.k === "kick") name = r.m.user.name || r.m.user.login;
    else if (r.k === "youtube" || r.k === "x") name = r.m.user.name;
    else if (r.k === "notice") name = r.n.user.name;
    if (!name) continue;
    const key = name.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(name);
  }
  return out;
}

export function platformOfKey(sk: string): Platform {
  return sk.slice(0, sk.indexOf(":")) as Platform;
}
