// The chat view of one split: message list, emotes, badges, mod tools, user cards and the input box.
// The props are the contract with layout/ (see docs/ARCHITECTURE.md). Ported from Chat() in
// modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2) and the four co-chat pages; the pieces live
// in chat/store.ts (frame reducer), chat/rows/* (row renderers), chat/input/* (send box, autocomplete,
// emote picker), chat/UserCard.tsx and chat/ModActionPopover.tsx. Changes: one pane renders any mix of
// sources (single channel or merged co-chat) from one ChatStore; the site's page chrome (headers, layout
// picker, settings popovers, Modes panel, second channel) is not part of the split.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { agu } from "../bridge";
import { useAccounts, useSettings } from "../hooks";
import type { ConnFrame, HighlightRule, Settings, SourceRef } from "../protocol";
import { alertOnce } from "../alerts";
import { textMentionsLogin } from "./chatText";
import { compileHighlights, matchInputOf } from "./highlights";
import { EmoteTipLayer } from "./emoteTip";
import { compileIgnores, filterIgnored, ignoreLogin, isUserIgnored, toggleIgnoredUser } from "./ignores";
import { MessageList } from "./MessageList";
import { ModActionPopover, type ModPopoverState } from "./ModActionPopover";
import { fetchCoCaps, NO_CO_CAPS, ALL_FALSE_CAPS, badgeNames, REACH_ALL, modReach, type CoCaps, type ModReach } from "./modCaps";
import { kickModAction, pinMessage, resolveAutomod, rumbleModAction, setPinDuration, twitchAction, twitchDelete, unpinMessage } from "./modActions";
import { buildKickInventory, buildRumbleInventory, buildTwitchInventory, EMPTY_INVENTORY, type Inventory } from "./input/inventory";
import { postText, SendBox, type SendSource } from "./input/SendBox";
import { PinnedCard } from "./rows/PinnedCard";
import { AutomodRow, ModFeedRow, NoticeRow, TwitchMessageRow } from "./rows/TwitchRows";
import { KickRow, RumbleRow, XRow, YouTubeRow } from "./rows/CoChatRows";
import type { CoEnv, Handlers, ReplyTarget, RowCtx, TwitchEnv } from "./rows/env";
import { chatterNames } from "./store";
import { useToasts } from "./toast";
import { mergeEmoteSets, type EmoteSets, type ProviderScopedMap } from "./twitch/emotes";
import { noticeVisible } from "./twitch/events";
import { EMPTY_TWITCH_DATA, useTwitchData, type TwitchData } from "./twitch/useTwitchData";
import type { Row } from "./types";
import { useChatFeed } from "./useChatFeed";
import { DEFAULT_MOD_BUTTONS, expandTemplate, formatDuration, normalizeModButtons } from "./modButtons";
import { confirmedCommand, parseUserArg, runSlashCommand, splitArgs } from "./input/slash";
import { UserCard, type UserCardState } from "./UserCard";
import { PopoutWindow, focusPopout } from "./PopoutWindow";

export type ChatPaneProps = {
  /** One source = a single channel; several = a merged co-chat. */
  sources: SourceRef[];
  /** Chatterino's "mod mode": inline mod buttons on every row. */
  modMode: boolean;
  /** This split has keyboard focus (focus its input when true). */
  focused: boolean;
  /** Called whenever a source's latest conn frame changes, keyed `${platform}:${channel}` as given in `sources`. */
  onStatus?: (status: Record<string, ConnFrame>) => void;
  /** Live chat arrived (not history): "highlight" when any of it matched a highlight rule or mentions you. */
  onActivity?: (kind: Activity) => void;
};

/** What live chat a split got: plain messages, or at least one highlight / mention. */
export type Activity = "message" | "highlight";

// Used until the real settings arrive (keep in sync with DEFAULT_SETTINGS in src/main/store.js).
const FALLBACK_SETTINGS: Settings = {
  twitchClientId: "",
  alwaysOnTop: false,
  updateChannel: "beta",
  timestamps: false,
  timestampSeconds: false,
  alternateRows: true,
  fontSize: 13,
  emoteSize: "medium",
  showBadges: true,
  readableColors: true,
  sevenTv: true,
  bttv: true,
  ffz: true,
  sevenTvCosmetics: true,
  animatedEmotes: true,
  deletedMessages: "strike",
  showModButtons: true,
  showModActions: true,
  showAutomod: true,
  pauseChat: "scroll",
  showPlatformMarks: true,
  hotkeys: {},
  modButtons: DEFAULT_MOD_BUTTONS,
  highlightRules: [],
  mentionSound: false,
  mentionFlash: true,
  highlightSoundFile: "",
  highlightVolume: 70,
  highlightSoundFocused: true,
  ignoredUsers: [],
  ignoredPhrases: [],
};

const NO_RULES: HighlightRule[] = [];

const EMPTY_SCOPED: ProviderScopedMap = { global: new Map(), channel: new Map() };
const MAX_CARDS = 4;

/** Returns the cached value while every dep is identical, else rebuilds it (keeps memoized rows' props stable). */
function cacheBy<T>(cache: Map<string, { deps: unknown[]; val: T }>, key: string, deps: unknown[], make: () => T): T {
  const hit = cache.get(key);
  if (hit && hit.deps.length === deps.length && hit.deps.every((d, i) => Object.is(d, deps[i]))) return hit.val;
  const val = make();
  cache.set(key, { deps, val });
  return val;
}

function loadHiddenPins(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem("hiddenPins") || "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

export function ChatPane({ sources, modMode, focused, onStatus, onActivity }: ChatPaneProps) {
  const rsLive = useSettings();
  const rs = rsLive ?? FALLBACK_SETTINGS;
  const accounts = useAccounts();
  const { store, snap } = useChatFeed(sources, onStatus);
  const { toast, node: toastNode } = useToasts();
  const panelRef = useRef<HTMLDivElement | null>(null);

  const [cards, setCards] = useState<UserCardState[]>([]);
  const [modPop, setModPop] = useState<ModPopoverState | null>(null);
  const [replyTarget, setReplyTarget] = useState<ReplyTarget | null>(null);
  const [hiddenPins, setHiddenPins] = useState<string[]>(loadHiddenPins);
  const [coCaps, setCoCaps] = useState<Record<string, CoCaps>>({});

  const me = accounts?.twitch.user;
  const meId = me?.id || "";
  const meLoginLower = (me?.login || "").toLowerCase();
  const twitchConnected = !!accounts?.twitch.connected;

  const states = [...store.sources.values()];
  const twitchInputs = states.filter((s) => s.ref.platform === "twitch").map((s) => ({ sk: s.sk, ref: s.ref, channelId: s.channelId }));
  const { data: twitchData, personal, sevenDefs } = useTwitchData(twitchInputs, { id: meId, connected: twitchConnected }, (sk, id) => store.setChannelId(sk, id));

  // Rumble / Kick: can the account moderate here (re-asked when the account connects or the source set changes).
  const coSig = states.filter((s) => s.ref.platform === "rumble" || s.ref.platform === "kick").map((s) => s.sk).join("\u0001");
  const coAcct = `${accounts?.rumble.connected ? 1 : 0}${accounts?.kick.connected ? 1 : 0}`;
  useEffect(() => {
    let dead = false;
    for (const s of [...store.sources.values()]) {
      if (s.ref.platform !== "rumble" && s.ref.platform !== "kick") continue;
      fetchCoCaps(s.ref).then((c) => {
        if (!dead) setCoCaps((prev) => ({ ...prev, [s.sk]: c }));
      });
    }
    return () => {
      dead = true;
    };
  }, [store, coSig, coAcct]);

  // ---- per-source row environments (identity-stable so memoized rows only re-render when they must) -------
  const caches = useRef({
    env: new Map<string, { deps: unknown[]; val: TwitchEnv }>(),
    co: new Map<string, { deps: unknown[]; val: CoEnv }>(),
    inv: new Map<string, { deps: unknown[]; val: Inventory }>(),
    send: new Map<string, { deps: unknown[]; val: SendSource }>(),
  }).current;

  // 7TV / BTTV / FFZ, with the settings applied, merged by priority for message rendering.
  const perTwitch = useMemo(() => {
    const out = new Map<string, { emotes: EmoteSets | null; inventory: Inventory }>();
    for (const [sk, d] of Object.entries(twitchData)) {
      const sets = d.providerSets
        ? { seven: rs.sevenTv ? d.providerSets.seven : EMPTY_SCOPED, bttv: rs.bttv ? d.providerSets.bttv : EMPTY_SCOPED, ffz: rs.ffz ? d.providerSets.ffz : EMPTY_SCOPED }
        : null;
      out.set(sk, {
        emotes: sets ? mergeEmoteSets(sets.seven, sets.bttv, sets.ffz) : null,
        inventory: buildTwitchInventory({ providerSets: sets, twitchEmotes: d.twitchEmotes, personal: rs.sevenTv ? personal : [] }),
      });
    }
    return out;
  }, [twitchData, personal, rs.sevenTv, rs.bttv, rs.ffz]);

  const rows = snap.rows;

  // ---- highlights (Settings > Highlights) ------------------------------------------------------------------
  const highlighter = useMemo(() => compileHighlights(rs.highlightRules ?? NO_RULES), [rs.highlightRules]);
  const ownLogin = useMemo(() => {
    const out: Record<string, { id: string; login: string }> = {};
    for (const [p, a] of Object.entries(accounts ?? {})) if (a?.user) out[p] = { id: a.user.id, login: (a.user.login || "").toLowerCase() };
    return out;
  }, [accounts]);
  // Rows are immutable (an edit replaces the object), so a row's match is cached by the row object.
  const ruleCache = useMemo(() => new WeakMap<Row, HighlightRule | null>(), [highlighter, ownLogin]);
  /** The viewer's own message: never highlighted by a rule and never alerts. */
  const isOwn = (r: Row): boolean => {
    const me = ownLogin[r.platform];
    if (!me || !(r.k === "msg" || r.k === "rumble" || r.k === "kick" || r.k === "youtube" || r.k === "x")) return false;
    const u = r.m.user as { id?: string; login?: string };
    return (!!me.id && u.id === me.id) || (!!me.login && (u.login || "").toLowerCase() === me.login);
  };
  const ruleOf = (r: Row): HighlightRule | null => {
    if (!highlighter.size) return null;
    let hit = ruleCache.get(r);
    if (hit === undefined) {
      const input = matchInputOf(r);
      hit = input && !isOwn(r) ? highlighter.match(input) : null;
      ruleCache.set(r, hit);
    }
    return hit;
  };

  // The viewer's lead-moderator status is read off their own badge in this chat (there is no API for it).
  const leadOf = (sk: string): boolean => {
    if (!meId) return false;
    for (let i = rows.length - 1; i >= 0; i--) {
      const r = rows[i];
      if (r.sk === sk && r.k === "msg" && r.m.user.id === meId) return badgeNames(r.m.badges).has("lead_moderator");
    }
    return false;
  };

  const twitchEnvOf = (sk: string): TwitchEnv | undefined => {
    const st = store.sources.get(sk);
    if (!st || st.ref.platform !== "twitch") return undefined;
    const d: TwitchData = twitchData[sk] ?? EMPTY_TWITCH_DATA;
    const pt = perTwitch.get(sk);
    const broadcaster = d.caps.isBroadcaster ?? (!!meLoginLower && st.ref.channel.toLowerCase().replace(/^#/, "") === meLoginLower);
    const lead = leadOf(sk);
    return cacheBy(caches.env, sk, [st.ref, d.badgeMap, pt?.emotes ?? null, sevenDefs, d.caps, st.known, meId, meLoginLower, broadcaster, lead], () => ({
      source: st.ref,
      badgeMap: d.badgeMap,
      emotes: pt?.emotes ?? null,
      sevenDefs,
      caps: d.caps,
      known: st.known,
      viewer: { id: meId, broadcaster, lead },
      meLoginLower,
    }));
  };

  const coEnvOf = (sk: string): CoEnv | undefined => {
    const st = store.sources.get(sk);
    if (!st) return undefined;
    const caps = coCaps[sk] ?? NO_CO_CAPS;
    const canSend = !!st.conn?.canSend;
    return cacheBy(caches.co, sk, [st.ref, caps, canSend, st.rumbleBadges, st.rumbleEmotes], () => ({
      source: st.ref,
      caps,
      canSend,
      rumbleBadges: st.rumbleBadges,
      rumbleEmotes: st.rumbleEmotes,
    }));
  };

  // ---- handlers (stable; they read the live store, never render state) --------------------------------------
  const refOf = useCallback((sk: string): SourceRef | undefined => store.sources.get(sk)?.ref, [store]);

  const h: Handlers = useMemo(
    () => ({
      openCard(target, sk, user) {
        const container = panelRef.current;
        const st = store.sources.get(sk);
        if (!container || !st) return;
        const id = `${sk}|${user.id || user.name.toLowerCase()}`;
        const rect = target.getBoundingClientRect();
        const cRect = container.getBoundingClientRect();
        if (focusPopout(id)) return;
        setCards((prev) => {
          const existing = prev.findIndex((c) => c.id === id);
          if (existing >= 0) {
            if (existing === prev.length - 1) return prev;
            const next = prev.slice();
            const [c] = next.splice(existing, 1);
            next.push(c);
            return next;
          }
          // One card per user; new ones stagger so stacked opens don't land exactly on top of each other.
          const stagger = 24 * (prev.length % 4);
          const relTop = rect.top - cRect.top + stagger;
          let left = rect.left - cRect.left + stagger;
          const maxLeft = cRect.width - 296;
          if (left > maxLeft) left = Math.max(8, maxLeft);
          if (left < 8) left = 8;
          const above = relTop > cRect.height / 2;
          // The pop-out window opens beside the name: below it, or above it in the lower half (cards run ~380px tall).
          const screenX = window.screenX + rect.left + stagger;
          const screenY = window.screenY + (above ? rect.top - 380 : rect.bottom + 4) + stagger;
          const card: UserCardState = { id, sk, platform: st.ref.platform, user, left, top: above ? relTop - 4 : relTop + rect.height + 4, above, screenX, screenY };
          const next = prev.length >= MAX_CARDS ? prev.slice(1) : prev.slice();
          next.push(card);
          return next;
        });
      },
      copy(text) {
        navigator.clipboard.writeText(text).then(
          () => toast("Copied"),
          () => toast("Couldn't copy", "warn"),
        );
      },
      reply: (t) => setReplyTarget(t),
      pin(sk, id) {
        const ref = refOf(sk);
        if (ref) void pinMessage(ref, id, toast);
      },
      deleteMsg(sk, id) {
        const ref = refOf(sk);
        if (ref) void twitchDelete(ref, id, toast).then((ok) => ok && store.markDeleted(sk, id));
      },
      quickTimeout(sk, msg, seconds) {
        const ref = refOf(sk);
        if (ref) void twitchAction(ref, { action: "timeout", userId: msg.user.id, duration: seconds }, `timed out ${msg.user.name} (${formatDuration(seconds)})`, toast);
      },
      twitchCommand(sk, msg, template) {
        const ref = refOf(sk);
        if (!ref) return;
        const line = expandTemplate(template, { user: msg.user.login, name: msg.user.name, userId: msg.user.id, msgId: msg.id, channel: ref.channel, msg: msg.text }).trim();
        if (!line) return;
        void (async () => {
          try {
            if (line.startsWith("/") && !/^\/me(\s|$)/i.test(line)) {
              const m = /^\/(\S+)\s*([\s\S]*)$/.exec(line);
              const cmd = m ? m[1].toLowerCase() : "";
              if (cmd === "clear" || cmd === "raid") {
                const login = parseUserArg(splitArgs(m?.[2] ?? "")[0] || "");
                if (cmd === "raid" && !login) return toast("Usage: /raid <user>", "warn");
                if (!window.confirm(cmd === "clear" ? "Clear the whole chat?" : `Raid ${login}?`)) return;
                await confirmedCommand(ref, cmd === "clear" ? { kind: "clear" } : { kind: "raid", login }, toast);
                return;
              }
              await runSlashCommand(line, { source: ref, toast, lastMessageId: () => msg.id, pinnedId: () => store.sources.get(sk)?.pinned?.id ?? null });
            } else {
              const err = await postText(ref, "/api/chat/send", line.slice(0, 500));
              if (err) toast(err, "warn");
            }
          } catch (e) {
            toast((e as Error).message, "warn");
          }
        })();
      },
      modPopover(target, kind, sk, msg) {
        const container = panelRef.current;
        if (!container) return;
        const rect = target.getBoundingClientRect();
        const cRect = container.getBoundingClientRect();
        let left = rect.left - cRect.left;
        const maxLeft = cRect.width - 272;
        if (left > maxLeft) left = Math.max(8, maxLeft);
        if (left < 8) left = 8;
        const relTop = rect.top - cRect.top;
        const above = relTop > cRect.height / 2;
        setModPop({ sk, msg, kind, left, top: above ? relTop - 4 : relTop + rect.height + 4, above });
      },
      async automod(sk, id, action) {
        const ref = refOf(sk);
        if (!ref) return;
        const res = await resolveAutomod(ref, id, action, toast);
        if (res === "gone") store.markAutomodExpired(sk, id);
      },
      rumbleMod(sk, action, msg, seconds) {
        const ref = refOf(sk);
        if (ref) void rumbleModAction(ref, action, msg, toast, seconds);
      },
      kickMod(sk, action, msg, seconds) {
        const ref = refOf(sk);
        if (ref) void kickModAction(ref, action, msg, toast, seconds);
      },
    }),
    [store, toast, refOf],
  );

  const showPlatform = rs.showPlatformMarks || sources.length > 1;
  const inlineMod = modMode && rs.showModButtons;
  const modList = useMemo(() => normalizeModButtons(rs.modButtons), [rs.modButtons]);
  const ctx: RowCtx = useMemo(() => ({ rs, h, showPlatform, modButtons: inlineMod, modList }), [rs, h, showPlatform, inlineMod, modList]);

  // ---- rows ---------------------------------------------------------------------------------------------------
  const visible = useMemo(() => {
    const bombs = new Set<string>();
    for (const r of rows) if (r.k === "notice" && r.n.giftId && /submysterygift$/.test(r.n.sourceMsgId || r.n.msgId)) bombs.add(r.n.giftId);
    return rows.filter(
      (r) => (r.k !== "notice" || noticeVisible(r.n, bombs)) && (r.k !== "modfeed" || rs.showModActions) && (r.k !== "automod" || rs.showAutomod),
    );
  }, [rows, rs.showModActions, rs.showAutomod]);

  // Ignored users / phrases. Results are cached per row object (rows are immutable; a change makes a new
  // object), and the cache is thrown away when the rules change.
  const ignores = useMemo(() => compileIgnores(rs.ignoredUsers, rs.ignoredPhrases), [rs.ignoredUsers, rs.ignoredPhrases]);
  const ignoreCache = useMemo(() => new WeakMap<Row, Row | null>(), [ignores]);
  const shown = useMemo(() => filterIgnored(visible, ignores, ignoreCache), [visible, ignores, ignoreCache]);

  const setIgnored = useCallback(
    (platform: SourceRef["platform"], login: string, on: boolean) => {
      // Read the latest list from main so two splits toggling at once don't overwrite each other.
      void agu.settings
        .get()
        .then((cur) => agu.settings.set({ ignoredUsers: toggleIgnoredUser(cur.ignoredUsers ?? [], platform, login, on) }))
        .then(
          () => toast(on ? `Ignoring ${login}` : `No longer ignoring ${login}`),
          () => toast("Couldn't save the ignore list", "warn"),
        );
    },
    [toast],
  );

  // ---- alerts and tab activity: once per live row (history batches never ring) ----------------------------
  const isIgnored = useCallback((r: Row) => filterIgnored([r], ignores, ignoreCache).length === 0, [ignores, ignoreCache]);
  const live = useRef({ rs, ruleOf, isOwn, isIgnored, meLoginLower, onActivity });
  live.current = { rs, ruleOf, isOwn, isIgnored, meLoginLower, onActivity };
  const seenOrd = useRef(0);
  useEffect(() => {
    const { rs, ruleOf, isOwn, isIgnored, meLoginLower, onActivity } = live.current;
    let maxOrd = seenOrd.current;
    let activity: Activity | null = null;
    for (const r of rows) {
      const ord = r.ord ?? 0;
      if (ord <= seenOrd.current) continue;
      if (ord > maxOrd) maxOrd = ord;
      if (r.backlog || r.k === "modfeed" || isOwn(r) || isIgnored(r)) continue; // ignored rows never ring
      if (r.k === "notice" && !noticeVisible(r.n, new Set())) continue;
      activity ??= "message";
      const mention = r.k === "msg" && textMentionsLogin(r.m.text, meLoginLower);
      const rule = mention ? null : ruleOf(r);
      if (!mention && !rule) continue;
      activity = "highlight";
      const key = `${r.sk.toLowerCase()}|${r.key.slice(r.sk.length)}`;
      alertOnce(key, mention ? { sound: rs.mentionSound, soundFile: "", flash: rs.mentionFlash } : { sound: rule!.sound, soundFile: rule!.soundFile, flash: rule!.flash }, rs);
    }
    seenOrd.current = maxOrd;
    if (activity) onActivity?.(activity);
  }, [rows]);

  const renderRow = (r: Row): ReactNode => {
    switch (r.k) {
      case "msg": {
        const env = twitchEnvOf(r.sk);
        if (!env) return null;
        return (
          <TwitchMessageRow key={r.key} row={r} ctx={ctx} env={env} susp={store.sources.get(r.sk)?.suspicious.get(r.m.user.id)?.status} modMode={inlineMod} rule={ruleOf(r)} />
        );
      }
      case "notice": {
        const env = twitchEnvOf(r.sk);
        return env ? <NoticeRow key={r.key} row={r} ctx={ctx} env={env} /> : null;
      }
      case "modfeed":
        return <ModFeedRow key={r.key} row={r} ctx={ctx} />;
      case "automod": {
        const env = twitchEnvOf(r.sk);
        return env ? <AutomodRow key={r.key} row={r} ctx={ctx} env={env} /> : null;
      }
      case "rumble": {
        const env = coEnvOf(r.sk);
        return env ? <RumbleRow key={r.key} row={r} ctx={ctx} env={env} modMode={inlineMod} rule={ruleOf(r)} /> : null;
      }
      case "kick": {
        const env = coEnvOf(r.sk);
        return env ? <KickRow key={r.key} row={r} ctx={ctx} env={env} modMode={inlineMod} rule={ruleOf(r)} /> : null;
      }
      case "youtube":
        return <YouTubeRow key={r.key} row={r} ctx={ctx} rule={ruleOf(r)} />;
      case "x":
        return <XRow key={r.key} row={r} ctx={ctx} rule={ruleOf(r)} />;
    }
  };

  // ---- send box sources ------------------------------------------------------------------------------------------
  const sendSources: SendSource[] = [];
  for (const st of states) {
    const conn = st.conn;
    const d = twitchData[st.sk] ?? EMPTY_TWITCH_DATA;
    const plat = st.ref.platform;
    const inv: Inventory =
      plat === "twitch"
        ? (perTwitch.get(st.sk)?.inventory ?? EMPTY_INVENTORY)
        : plat === "rumble"
          ? cacheBy(caches.inv, st.sk, [st.rumbleEmotes], () => buildRumbleInventory(st.rumbleEmotes))
          : plat === "kick"
            ? cacheBy(caches.inv, st.sk, [st.kickEmotes], () => buildKickInventory(st.kickEmotes))
            : EMPTY_INVENTORY;
    const canSend = !!conn?.canSend;
    const reason = conn?.readOnlyReason || (conn && conn.state === "unavailable" ? conn.detail : undefined);
    const pending = !conn || conn.state === "connecting";
    sendSources.push(
      cacheBy(caches.send, st.sk, [st.ref, canSend, reason, pending, d.caps, inv], () => ({ sk: st.sk, ref: st.ref, canSend, readOnlyReason: reason, pending, caps: d.caps, inventory: inv })),
    );
  }
  const sendRef = useRef<SendSource[]>([]);
  if (sendRef.current.length !== sendSources.length || sendRef.current.some((s, i) => s !== sendSources[i])) sendRef.current = sendSources;
  const stableSendSources = sendRef.current;

  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const getChatters = useCallback((sks: string[]) => chatterNames(rowsRef.current, new Set(sks)), []);
  const getLastMessageId = useCallback((sk: string) => {
    const list = rowsRef.current;
    for (let i = list.length - 1; i >= 0; i--) {
      const r = list[i];
      if (r.sk === sk && r.k === "msg") return r.m.id;
    }
    return null;
  }, []);
  const getPinnedId = useCallback((sk: string) => store.sources.get(sk)?.pinned?.id ?? null, [store]);
  const clearReply = useCallback(() => setReplyTarget(null), []);

  // ---- user cards -----------------------------------------------------------------------------------------------
  const closeCard = (id: string) => setCards((prev) => prev.filter((c) => c.id !== id));
  const frontCard = (id: string) =>
    setCards((prev) => {
      const i = prev.findIndex((c) => c.id === id);
      if (i < 0 || i === prev.length - 1) return prev;
      const next = prev.slice();
      next.push(next.splice(i, 1)[0]);
      return next;
    });
  const moveCard = (id: string, left: number, top: number, above: boolean) => setCards((prev) => prev.map((c) => (c.id === id ? { ...c, left, top, above } : c)));

  // Esc closes the topmost card (unless something inside already used the key, like an autocomplete).
  const cardsRef = useRef(cards);
  cardsRef.current = cards;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented || !cardsRef.current.length) return;
      const top = cardsRef.current[cardsRef.current.length - 1];
      const root = panelRef.current;
      // Only the pane that owns the card (or the one under the pointer) reacts, not every split on screen.
      if (!root || !(root.contains(document.activeElement) || root.matches(":hover"))) return;
      setCards((prev) => prev.filter((c) => c.id !== top.id));
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const reachFor = (card: UserCardState): ModReach => {
    if (card.platform !== "twitch") return REACH_ALL;
    const env = twitchEnvOf(card.sk);
    if (!env) return REACH_ALL;
    // No line from them in chat = no badges to go on: nothing is ruled out except banning yourself.
    for (let i = rows.length - 1; i >= 0; i--) {
      const r = rows[i];
      if (r.sk === card.sk && r.k === "msg" && r.m.user.id === card.user.id) return modReach(env.viewer, { id: card.user.id, badges: r.m.badges });
    }
    return card.user.id === meId ? { ...REACH_ALL, ban: false } : REACH_ALL;
  };

  // ---- pinned messages ----------------------------------------------------------------------------------------------
  const hidePin = (id: string) => {
    const next = [...hiddenPins.filter((x) => x !== id), id].slice(-20);
    setHiddenPins(next);
    try {
      localStorage.setItem("hiddenPins", JSON.stringify(next));
    } catch {
      /* private mode: hidden until reload */
    }
  };
  const pinnerLook = (sk: string, id: string | undefined) => {
    if (!id) return null;
    for (let i = rows.length - 1; i >= 0; i--) {
      const r = rows[i];
      if (r.sk === sk && r.k === "msg" && r.m.user.id === id) return { badges: r.m.badges, color: r.m.user.color };
    }
    return null;
  };
  const multiTwitch = twitchInputs.length > 1;

  // ---- empty / loading state ------------------------------------------------------------------------------------------
  const anyLoaded = states.some((s) => s.loaded);
  const empty: ReactNode = (
    <div className="px-4 py-10 text-center text-xs text-faint">
      {!anyLoaded && states.every((s) => !s.conn || s.conn.state === "connecting") ? (
        <span className="inline-flex items-center gap-2">
          <span className="h-3 w-3 animate-spin rounded-full border-2 border-faint border-t-transparent" /> Connecting...
        </span>
      ) : (
        <>
          <div>No messages yet.</div>
          {states
            .filter((s) => s.conn && s.conn.state !== "open" && s.conn.detail)
            .map((s) => (
              <div key={s.sk} className="mt-1">
                {s.ref.platform}: {s.conn!.detail}
              </div>
            ))}
        </>
      )}
    </div>
  );

  return (
    <div ref={panelRef} className="relative flex h-full min-h-0 flex-col bg-panel text-text" style={{ fontSize: rs.fontSize }}>
      {states
        .filter((s) => s.ref.platform === "twitch" && s.pinned && !hiddenPins.includes(s.pinned.id))
        .map((s) => {
          const env = twitchEnvOf(s.sk);
          if (!env || !s.pinned) return null;
          const p = s.pinned;
          return (
            <PinnedCard
              key={`${s.sk}|${p.id}`}
              msg={p}
              channelChip={multiTwitch ? `#${s.ref.channel}` : undefined}
              pinner={pinnerLook(s.sk, p.pin?.by.id)}
              env={env}
              fontPx={rs.fontSize}
              readableColors={rs.readableColors}
              canMod={env.caps.pin}
              onUnpin={() => void unpinMessage(s.ref, p.id, toast)}
              onDuration={(secs) => setPinDuration(s.ref, p.id, secs, toast)}
              onHide={() => hidePin(p.id)}
            />
          );
        })}

      <MessageList rows={shown} renderRow={renderRow} empty={empty} onHold={(held) => store.setHeld(held)} focused={focused} pauseOn={rs.pauseChat} />

      {cards.map((c) => {
        const ref = refOf(c.sk);
        if (!ref) return null;
        const ignoreAs = ignoreLogin(c.user);
        return (
          <PopoutWindow key={c.id} id={c.id} x={c.screenX} y={c.screenY} width={300} title={`${c.user.name} - Agu Chat`} onClose={() => closeCard(c.id)}>
          <UserCard
            windowed
            state={c}
            source={ref}
            panelRef={panelRef}
            rows={rows}
            twitch={twitchEnvOf(c.sk)}
            co={coEnvOf(c.sk)}
            caps={(twitchData[c.sk] ?? EMPTY_TWITCH_DATA).caps ?? ALL_FALSE_CAPS}
            reach={reachFor(c)}
            suspicious={store.sources.get(c.sk)?.suspicious.get(c.user.id)}
            toast={toast}
            h={h}
            onClose={() => closeCard(c.id)}
            onFront={() => frontCard(c.id)}
            onMove={(l, t, a) => moveCard(c.id, l, t, a)}
            onTwitchDeleted={(id) => store.markDeleted(c.sk, id)}
            ignored={!!ignoreAs && isUserIgnored(rs.ignoredUsers ?? [], c.platform, ignoreAs)}
            onIgnore={ignoreAs ? (on) => setIgnored(c.platform, ignoreAs, on) : undefined}
          />
          </PopoutWindow>
        );
      })}

      {modPop &&
        (() => {
          const ref = refOf(modPop.sk);
          return ref ? <ModActionPopover pop={modPop} source={ref} rows={rows} toast={toast} onClose={() => setModPop(null)} onDeleted={(id) => store.markDeleted(modPop.sk, id)} /> : null;
        })()}

      {toastNode}

      <SendBox
        sources={stableSendSources}
        focused={focused}
        replyTarget={replyTarget}
        onClearReply={clearReply}
        toast={toast}
        getChatters={getChatters}
        getLastMessageId={getLastMessageId}
        getPinnedId={getPinnedId}
      />
      <EmoteTipLayer />
    </div>
  );
}
