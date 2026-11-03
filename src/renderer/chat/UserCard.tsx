// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): UserCard (the draggable mod
// control centre) and suspiciousDetails; the Kick mod menu (KickChat.tsx KickModMenu) and Rumble's row
// actions are folded into the same card. Changes: restyled Chatterino-like on the theme tokens, one card
// for every platform (Twitch gets the /api/mod/user info + the timeout ladder / ban / unban / warn when the
// split is in mod mode and the account has the rights; Rumble / Kick get profile link + their mod
// actions; YouTube / X get the profile + recent messages), recent messages come from this split's rows,
// Esc closes the topmost card (handled by the pane), `alt` second-channel support dropped.
import { useCallback, useEffect, useRef, useState, type PointerEvent as RPointerEvent, type RefObject } from "react";
import { relayApi } from "../bridge";
import { PlatformIcon, PLATFORM_LABEL } from "../platformIcons";
import type { Platform, SourceRef } from "../protocol";
import { TrashIcon, XIcon } from "../icons";
import { fetchChatRules, TIMEOUT_LADDER, type ModCaps, type ModReach } from "./modCaps";
import { twitchAction } from "./modActions";
import type { Handlers, TwitchEnv, CoEnv } from "./rows/env";
import { MessageText } from "./parts";
import { splitKickEmotes, splitRumbleEmotes } from "./rows/CoChatRows";
import { buildParts } from "./twitch/parts";
import type { Toast } from "./toast";
import type { ChatMsg, Row, RowUser, SuspiciousUser } from "./types";
import { btnDanger, btnGhost, btnPrimary, fmtTime, inputCls } from "./ui";

export type UserCardState = { id: string; sk: string; platform: Platform; user: RowUser; left: number; top: number; above: boolean };

type ModUserResp = {
  user: { id: string; login: string; displayName: string; avatar: string; createdAt: string };
  followedAt: string | null;
  ban?: { permanent: boolean; expiresAt: string | null; reason: string } | null;
  banError?: string;
  // distinguishes "checked, genuinely not following" from "couldn't check"
  followError?: string;
};

// Spell out why Twitch flagged this user, for the display-only info line.
function suspiciousDetails(u: SuspiciousUser): string {
  const parts: string[] = [];
  if (u.types.includes("ban_evader")) {
    parts.push(u.evaluation === "likely" ? "likely ban evader" : u.evaluation === "possible" ? "possible ban evader" : "ban evader");
  }
  if (u.types.includes("manually_added")) parts.push("manually added by a moderator");
  if (u.types.includes("banned_in_shared_channel")) {
    parts.push(`banned in ${u.sharedBanChannels} shared channel${u.sharedBanChannels === 1 ? "" : "s"}`);
  }
  return parts.join(", ");
}

const PROFILE_URL: Partial<Record<Platform, (login: string) => string>> = {
  twitch: (l) => `https://twitch.tv/${l}`,
  kick: (l) => `https://kick.com/${l}`,
  rumble: (l) => `https://rumble.com/user/${l}`,
  x: (l) => `https://x.com/${l}`,
};

function Avatar({ src, name }: { src?: string; name?: string }) {
  if (src) return <img src={src} alt="" className="h-9 w-9 shrink-0 rounded-full object-cover" />;
  return (
    <span className="grid h-9 w-9 shrink-0 place-items-center rounded-full bg-raised-2 text-sm font-semibold uppercase text-muted">{(name || "?").slice(0, 1)}</span>
  );
}

export function UserCard({
  state,
  source,
  panelRef,
  rows,
  twitch,
  co,
  modMode,
  caps,
  reach,
  suspicious,
  toast,
  h,
  onClose,
  onFront,
  onMove,
  onTwitchDeleted,
  ignored,
  onIgnore,
}: {
  state: UserCardState;
  source: SourceRef;
  panelRef: RefObject<HTMLDivElement | null>;
  rows: Row[];
  twitch?: TwitchEnv;
  co?: CoEnv;
  modMode: boolean;
  caps: ModCaps;
  reach: ModReach;
  suspicious: SuspiciousUser | undefined;
  toast: Toast;
  h: Handlers;
  onClose: () => void;
  onFront: () => void;
  onMove: (left: number, top: number, above: boolean) => void;
  onTwitchDeleted: (id: string) => void;
  /** This user is on the ignore list (their messages are hidden). */
  ignored: boolean;
  /** Add to / remove from the ignore list; absent when the user has no name to ignore by. */
  onIgnore?: (on: boolean) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const dragRef = useRef<{ x: number; y: number; left: number; top: number } | null>(null);
  const isTwitch = state.platform === "twitch";
  const [data, setData] = useState<ModUserResp | null>(null);
  const [relogin, setRelogin] = useState(false);
  const [loading, setLoading] = useState(isTwitch);
  const [reasonMode, setReasonMode] = useState(false);
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [warnOpen, setWarnOpen] = useState(false);
  const [rules, setRules] = useState<string[]>([]);
  const [selectedRules, setSelectedRules] = useState<Set<string>>(new Set());
  const [customReason, setCustomReason] = useState("");

  const load = useCallback(async () => {
    if (!isTwitch || !state.user.id) {
      setLoading(false);
      return;
    }
    setLoading(true);
    setRelogin(false);
    try {
      const r = await relayApi(source, `/api/mod/user?id=${encodeURIComponent(state.user.id)}`);
      if (r.status === 403 && (await r.text()).includes("relogin")) {
        setRelogin(true);
        setData(null);
        return;
      }
      if (!r.ok) throw new Error(await r.text());
      setData(await r.json());
    } catch (e) {
      toast("Couldn't load user info: " + (e as Error).message, "warn");
    } finally {
      setLoading(false);
    }
  }, [isTwitch, source, state.user.id, toast]);

  useEffect(() => {
    load();
  }, [load]);

  // Drag: the header row is the handle. On grab, capture the CURRENT on-screen position (post-transform,
  // if any) as the new plain left/top baseline and clear `above` so there's no jump.
  const onHeaderPointerDown = (e: RPointerEvent<HTMLDivElement>) => {
    e.currentTarget.setPointerCapture(e.pointerId);
    const panel = panelRef.current;
    const card = ref.current;
    if (!panel || !card) return;
    const panelRect = panel.getBoundingClientRect();
    const cardRect = card.getBoundingClientRect();
    const left = cardRect.left - panelRect.left;
    const top = cardRect.top - panelRect.top;
    dragRef.current = { x: e.clientX, y: e.clientY, left, top };
    onFront();
    onMove(left, top, false);
  };
  const onHeaderPointerMove = (e: RPointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    const panel = panelRef.current;
    if (!d || !panel) return;
    const panelRect = panel.getBoundingClientRect();
    const left = Math.min(Math.max(0, d.left + (e.clientX - d.x)), Math.max(0, panelRect.width - 60));
    const top = Math.min(Math.max(0, d.top + (e.clientY - d.y)), Math.max(0, panelRect.height - 40));
    onMove(left, top, false);
  };
  const onHeaderPointerUp = (e: RPointerEvent<HTMLDivElement>) => {
    dragRef.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      /* ignore */
    }
  };

  useEffect(() => {
    if (!warnOpen) return;
    let dead = false;
    fetchChatRules(source).then((r) => {
      if (!dead) setRules(r);
    });
    return () => {
      dead = true;
    };
  }, [warnOpen, source]);

  const act = async (body: Record<string, unknown>, successMsg: string, onSuccess?: () => void) => {
    setBusy(true);
    try {
      const ok = await twitchAction(source, body, successMsg, toast);
      if (ok) {
        setReasonMode(false);
        setReason("");
        onSuccess?.();
        await load();
      }
    } finally {
      setBusy(false);
    }
  };

  // Newest first: keep the last 15 of this person's lines in this split.
  const mine = (r: Row): boolean => {
    if (r.sk !== state.sk) return false;
    switch (r.k) {
      case "msg":
        return r.m.user.id === state.user.id;
      case "rumble":
      case "kick":
        return r.m.user.id === state.user.id;
      case "youtube":
      case "x":
        return r.m.user.id === state.user.id;
      default:
        return false;
    }
  };
  const recent = rows.filter(mine).slice(-15).reverse();
  const lastRow = recent[0];

  const banned = data?.ban;
  const submitWarn = () => {
    const reasonText = [...Array.from(selectedRules), customReason.trim()].filter(Boolean).join("; ").slice(0, 500);
    act({ action: "warn", userId: state.user.id, reason: reasonText }, "warned", () => {
      setWarnOpen(false);
      setSelectedRules(new Set());
      setCustomReason("");
    });
  };

  const login = data?.user.login || state.user.login;
  const display = data?.user.displayName || state.user.name;
  const profile = login && PROFILE_URL[state.platform] ? PROFILE_URL[state.platform]!(login) : "";
  const showTwitchMod = isTwitch && modMode && (reach.ban || reach.timeout || reach.warn || !!banned) && (caps.ban || caps.warn);

  return (
    <div
      ref={ref}
      onMouseDownCapture={onFront}
      className="absolute z-30 w-72 overflow-hidden rounded-md border border-line bg-raised text-xs text-text shadow-2xl shadow-black/60"
      style={{ left: state.left, top: state.top, transform: state.above ? "translateY(-100%)" : undefined }}
    >
      <div
        className="flex cursor-grab items-center gap-2.5 border-b border-line p-2.5 active:cursor-grabbing"
        onPointerDown={onHeaderPointerDown}
        onPointerMove={onHeaderPointerMove}
        onPointerUp={onHeaderPointerUp}
      >
        <Avatar src={data?.user.avatar || state.user.avatar} name={display} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1.5">
            <PlatformIcon platform={state.platform} className="h-3.5 w-3.5 shrink-0" />
            {profile ? (
              <a href={profile} target="_blank" rel="noopener noreferrer" onPointerDown={(e) => e.stopPropagation()} className="truncate text-sm font-semibold text-text hover:underline">
                {display || login}
              </a>
            ) : (
              <span className="truncate text-sm font-semibold">{display || "unknown"}</span>
            )}
          </div>
          {display && login && display.toLowerCase() !== login.toLowerCase() && <div className="text-[11px] text-faint">@{login}</div>}
        </div>
        <button type="button" title="Close (Esc)" onPointerDown={(e) => e.stopPropagation()} onClick={onClose} className="rounded p-1 text-faint hover:text-text">
          <XIcon className="h-4 w-4" />
        </button>
      </div>

      {isTwitch && (
        <div className="space-y-1 border-b border-line p-2.5 text-muted">
          {relogin ? (
            <p className="text-amber-300">Log in again to enable mod tools.</p>
          ) : loading ? (
            <>
              <div className="h-3 w-40 animate-pulse rounded bg-white/10" />
              <div className="h-3 w-32 animate-pulse rounded bg-white/10" />
            </>
          ) : data ? (
            <>
              <div>Account created {new Date(data.user.createdAt).toLocaleDateString()}</div>
              {/* "Not following" only when we actually confirmed it: a failed lookup omits the line. */}
              {(data.followedAt || !data.followError) && <div>{data.followedAt ? `Following since ${new Date(data.followedAt).toLocaleDateString()}` : "Not following"}</div>}
              {data.ban ? (
                <div>
                  {data.ban.permanent ? (
                    <span className="inline-block rounded-full border border-red-500/30 bg-red-500/10 px-2 py-px text-[11px] font-semibold text-red-300">
                      Banned{data.ban.reason ? `: ${data.ban.reason}` : ""}
                    </span>
                  ) : (
                    <span className="inline-block rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-px text-[11px] font-semibold text-amber-400">
                      Timed out until {data.ban.expiresAt ? new Date(data.ban.expiresAt).toLocaleTimeString() : "?"}
                    </span>
                  )}
                </div>
              ) : data.banError ? (
                <div className="text-faint">Ban status unknown: {data.banError}</div>
              ) : null}
              {suspicious && (
                <div className="rounded border border-orange-500/30 bg-orange-500/10 px-2 py-1 text-orange-300">
                  Twitch flags this user: {suspicious.status === "restricted" ? "restricted" : "monitored"}
                  {suspiciousDetails(suspicious) && <> - {suspiciousDetails(suspicious)}</>}
                </div>
              )}
            </>
          ) : (
            <div className="text-faint">No account info.</div>
          )}
        </div>
      )}

      {showTwitchMod && (
        <div className="space-y-2.5 border-b border-line p-2.5">
          {!banned && reach.timeout && caps.ban && (
            <div>
              <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-faint">Timeout</div>
              <div className="flex flex-wrap gap-1">
                {TIMEOUT_LADDER.map((t) => (
                  <button
                    key={t.sec}
                    type="button"
                    disabled={busy}
                    onClick={() => act({ action: "timeout", userId: state.user.id, duration: t.sec }, `timed out ${t.label}`)}
                    className="rounded border border-line px-1.5 py-0.5 text-[11px] text-muted hover:border-amber-500/50 hover:text-amber-300 disabled:opacity-40"
                  >
                    {t.label}
                  </button>
                ))}
              </div>
            </div>
          )}

          {reasonMode ? (
            <div className="flex items-center gap-1.5">
              <input className={`${inputCls} min-w-0 flex-1`} placeholder="Reason (optional)" value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} />
              <button type="button" className={btnPrimary} disabled={busy} onClick={() => act({ action: "ban", userId: state.user.id, reason: reason.trim() || undefined }, "banned")}>
                Confirm
              </button>
              <button
                type="button"
                className={btnGhost}
                onClick={() => {
                  setReasonMode(false);
                  setReason("");
                }}
              >
                Cancel
              </button>
            </div>
          ) : warnOpen ? (
            <div className="space-y-2 rounded border border-line bg-black/20 p-2">
              <p className="text-muted">Select chat rules the user has violated, or enter a custom reason to warn {display || "this user"}</p>
              {rules.length > 0 && (
                <div className="max-h-28 space-y-1 overflow-y-auto">
                  {rules.map((rule) => (
                    <label key={rule} className="flex items-start gap-2 text-muted">
                      <input
                        type="checkbox"
                        className="mt-0.5"
                        checked={selectedRules.has(rule)}
                        onChange={(e) =>
                          setSelectedRules((prev) => {
                            const next = new Set(prev);
                            if (e.target.checked) next.add(rule);
                            else next.delete(rule);
                            return next;
                          })
                        }
                      />
                      <span>{rule}</span>
                    </label>
                  ))}
                </div>
              )}
              <input className={`${inputCls} w-full`} placeholder="Custom reason" value={customReason} onChange={(e) => setCustomReason(e.target.value)} maxLength={500} />
              <div className="flex gap-1.5">
                <button type="button" className={btnPrimary} disabled={busy || (selectedRules.size === 0 && !customReason.trim())} onClick={submitWarn}>
                  Warn
                </button>
                <button
                  type="button"
                  className={btnGhost}
                  onClick={() => {
                    setWarnOpen(false);
                    setSelectedRules(new Set());
                    setCustomReason("");
                  }}
                >
                  Cancel
                </button>
              </div>
            </div>
          ) : (
            <div className="flex flex-wrap gap-1.5">
              {reach.warn && caps.warn && (
                <button type="button" className={btnGhost} disabled={busy} onClick={() => setWarnOpen(true)}>
                  Warn
                </button>
              )}
              {banned ? (
                <>
                  <button type="button" className={btnGhost} disabled={busy} onClick={() => act({ action: "unban", userId: state.user.id }, banned.permanent ? "unbanned" : "untimed out")}>
                    {banned.permanent ? "Unban" : "Untimeout"}
                  </button>
                  {!banned.permanent && reach.ban && caps.ban && (
                    <button type="button" className={btnDanger} disabled={busy} onClick={() => setReasonMode(true)}>
                      Ban
                    </button>
                  )}
                </>
              ) : (
                reach.ban &&
                caps.ban && (
                  <button type="button" className={btnDanger} disabled={busy} onClick={() => setReasonMode(true)}>
                    Ban
                  </button>
                )
              )}
            </div>
          )}
        </div>
      )}

      {state.platform === "rumble" && modMode && co?.caps.canModerate && lastRow?.k === "rumble" && (
        <div className="flex flex-wrap gap-1.5 border-b border-line p-2.5">
          <button type="button" className={btnGhost} onClick={() => h.rumbleMod(state.sk, "timeout", lastRow.m)}>
            Timeout 5m
          </button>
          <button type="button" className={btnDanger} onClick={() => h.rumbleMod(state.sk, "ban", lastRow.m)}>
            Ban
          </button>
        </div>
      )}

      {state.platform === "kick" && modMode && co?.caps.canModerate && !!state.user.id && lastRow?.k === "kick" && (
        <div className="border-b border-line p-2.5">
          <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-faint">Timeout</div>
          <div className="mb-2 flex flex-wrap gap-1">
            {[
              ["1m", 60],
              ["5m", 300],
              ["10m", 600],
              ["1h", 3600],
              ["24h", 86400],
              ["7d", 604800],
            ].map(([label, sec]) => (
              <button key={label} type="button" className="rounded border border-line px-1.5 py-0.5 text-[11px] text-muted hover:border-amber-500/50 hover:text-amber-300" onClick={() => h.kickMod(state.sk, "timeout", lastRow.m, sec as number)}>
                {label}
              </button>
            ))}
          </div>
          <div className="flex gap-1.5">
            <button type="button" className={btnDanger} onClick={() => h.kickMod(state.sk, "ban", lastRow.m)}>
              Ban
            </button>
            <button type="button" className={btnGhost} onClick={() => h.kickMod(state.sk, "unban", lastRow.m)}>
              Unban
            </button>
          </div>
        </div>
      )}

      {onIgnore && (
        <div className="flex flex-wrap items-center gap-1.5 border-b border-line p-2.5">
          <button
            type="button"
            className={btnGhost}
            title={ignored ? "Show this user's messages again" : `Hide this user's messages on ${PLATFORM_LABEL[state.platform]} (Settings > Ignores)`}
            onClick={() => onIgnore(!ignored)}
          >
            {ignored ? "Unignore user" : "Ignore user"}
          </button>
          {ignored && <span className="text-[11px] text-faint">Messages hidden</span>}
        </div>
      )}

      <div className="max-h-52 overflow-y-auto p-2.5">
        <div className="mb-1 text-[10px] font-semibold uppercase tracking-wide text-faint">Recent messages in {PLATFORM_LABEL[state.platform]}</div>
        {recent.length === 0 ? (
          <p className="text-faint">No recent messages.</p>
        ) : (
          <div className="space-y-1 text-muted">
            {recent.map((r) => {
              const deleted = (r.k === "msg" || r.k === "rumble" || r.k === "kick") && !!r.m.deleted;
              return (
                <div key={r.key} className={`group/r relative flex items-start gap-1.5 ${deleted ? "line-through opacity-50" : ""}`}>
                  <span className="shrink-0 tabular-nums text-faint">{fmtTime(r.ts, false)}</span>
                  <span className="min-w-0 flex-1 whitespace-normal break-words">
                    <RecentText row={r} twitch={twitch} />
                  </span>
                  {r.k === "msg" && twitch && twitch.caps.delete && reach.delete && !r.m.deleted && (
                    <button
                      type="button"
                      title="Delete message"
                      onClick={async () => {
                        if (await twitchAction(source, { action: "delete", messageId: r.m.id }, null, toast)) onTwitchDeleted(r.m.id);
                      }}
                      className="shrink-0 rounded p-0.5 text-faint opacity-0 hover:text-red-400 group-hover/r:opacity-100"
                    >
                      <TrashIcon className="h-3.5 w-3.5" />
                    </button>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

function RecentText({ row, twitch }: { row: Row; twitch?: TwitchEnv }) {
  switch (row.k) {
    case "msg": {
      const m: ChatMsg = row.m;
      return <MessageText parts={buildParts(m.text, m.emotes, twitch?.emotes ?? null, null, m.gifs)} size="small" fontPx={12} emotes={twitch?.emotes ?? null} />;
    }
    case "kick":
      return <>{splitKickEmotes(row.m.text).map((p) => (p.kind === "emote" ? p.name : p.text)).join("")}</>;
    case "rumble":
      return <>{splitRumbleEmotes(row.m.text, {}).map((p) => (p.kind === "text" ? p.text : "")).join("")}</>;
    case "youtube":
    case "x":
      return <>{row.m.text}</>;
    default:
      return null;
  }
}
