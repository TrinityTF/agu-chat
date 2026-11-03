// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 4eaea5f): ModesButton + BlockedTermsSection,
// the Twitch room settings (GET/POST /api/chat/settings), Shield Mode (/api/chat/shield) and blocked terms
// (/api/chat/terms). Changes: one button per split header, shown only to someone who can moderate one of its
// Twitch channels, with a channel picker when the split merges several; every call goes through
// relayApi(source, ...); errors show in the panel (the header has no toaster); the panel opens with fresh
// settings (?fresh=1) and the button turns accent while a restricting mode is on.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { createPortal } from "react-dom";
import { agu, relayApi } from "../bridge";
import type { SourceRef } from "../protocol";
import { ShieldIcon, XIcon } from "../icons";
import { fetchModCaps, type ModCaps } from "../chat/modCaps";
import { postJson } from "../chat/input/slash";
import { Toggle, inputCls } from "./ui";
import { sourceKey } from "./tree";

type HelixChatSettings = {
  emote_mode: boolean | null;
  follower_mode: boolean | null;
  follower_mode_duration: number | null;
  slow_mode: boolean | null;
  slow_mode_wait_time: number | null;
  subscriber_mode: boolean | null;
  unique_chat_mode: boolean | null;
};

const FOLLOWER_MODE_OPTIONS: { v: number; label: string }[] = [
  { v: -1, label: "Off" },
  { v: 0, label: "Any" },
  { v: 10, label: "10m" },
  { v: 30, label: "30m" },
  { v: 60, label: "1h" },
  { v: 1440, label: "1d" },
  { v: 10080, label: "1w" },
  { v: 43200, label: "1mo" },
  { v: 129600, label: "3mo" },
];
const SLOW_MODE_OPTIONS = [0, 3, 5, 10, 20, 30, 60, 120];

const canModerate = (c: ModCaps | undefined) => !!c && (c.modes || c.shield || c.terms);
const restricted = (m: HelixChatSettings | null, shield: boolean) =>
  shield || !!(m && (m.emote_mode || m.follower_mode || m.slow_mode || m.subscriber_mode || m.unique_chat_mode));
const selectCls = inputCls.replace("w-full", "w-24 shrink-0") + " py-0.5";
const errText = (e: unknown) => (e instanceof Error ? e.message : String(e)) || "update failed";

/** The Modes button for a split's header: nothing when the user can't moderate any of its Twitch channels. */
export function ModesButton({ sources }: { sources: SourceRef[] }) {
  const twitch = sources.filter((s) => s.platform === "twitch");
  const sig = twitch.map(sourceKey).join("|");
  const [caps, setCaps] = useState<Record<string, ModCaps>>({});
  const [pick, setPick] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const [modes, setModes] = useState<HelixChatSettings | null>(null);
  const [shield, setShield] = useState(false);
  const btnRef = useRef<HTMLButtonElement>(null);

  // Caps per channel, again whenever the account changes (log in / out / new scopes).
  useEffect(() => {
    let alive = true;
    const load = () =>
      Promise.all(twitch.map(async (s) => [sourceKey(s), await fetchModCaps(s)] as const)).then((all) => alive && setCaps(Object.fromEntries(all)));
    load();
    const off = agu.accounts.onChange(load);
    return () => {
      alive = false;
      off();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sig]);

  const modded = twitch.filter((s) => canModerate(caps[sourceKey(s)]));
  const source = modded.find((s) => sourceKey(s) === pick) ?? modded[0];
  const key = source ? sourceKey(source) : "";
  const c = source ? caps[key] : undefined;

  // The current modes, for the button's colour; the panel reloads them fresh when it opens.
  const load = useCallback(
    async (fresh: boolean) => {
      if (!source || !c) return;
      const [m, sh] = await Promise.all([
        c.modes ? relayApi(source, `/api/chat/settings${fresh ? "?fresh=1" : ""}`).then(async (r) => (r.ok ? ((await r.json()) as HelixChatSettings) : null)) : null,
        c.shield ? relayApi(source, "/api/chat/shield").then(async (r) => (r.ok ? ((await r.json()) as { is_active?: boolean }) : null)) : null,
      ]).catch(() => [null, null] as const);
      if (m) setModes(m);
      if (sh) setShield(!!sh.is_active);
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [key, c?.modes, c?.shield],
  );
  useEffect(() => {
    setModes(null);
    setShield(false);
    load(false);
  }, [load]);

  if (!source || !c) return null;
  const on = restricted(modes, shield);
  return (
    <>
      <button
        ref={btnRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-pressed={open}
        title={on ? "Chat modes (a mode is on)" : "Chat modes"}
        aria-label="Chat modes"
        className={
          "flex h-5 w-5 shrink-0 items-center justify-center rounded hover:bg-[var(--color-raised-2)] " +
          (on ? "text-[var(--color-accent-soft)]" : "text-[var(--color-faint)] hover:text-[var(--color-text)]")
        }
      >
        <ShieldIcon className="h-3.5 w-3.5" />
      </button>
      {open && (
        <ModesPanel
          anchor={btnRef}
          channels={modded}
          source={source}
          caps={c}
          modes={modes}
          shield={shield}
          onPick={setPick}
          onModes={setModes}
          onShield={setShield}
          reload={() => load(true)}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  );
}

function useAnchoredPanel(anchor: React.RefObject<HTMLElement | null>, width = 288) {
  const [style, setStyle] = useState<CSSProperties | null>(null);
  useLayoutEffect(() => {
    const place = () => {
      const b = anchor.current?.getBoundingClientRect();
      if (!b) return;
      const M = 8;
      // Prefer right-aligned with the button, but never past either edge.
      const w = Math.min(width, window.innerWidth - 2 * M);
      const left = Math.max(M, Math.min(b.right - w, window.innerWidth - w - M));
      const below = window.innerHeight - b.bottom - M - 6;
      const above = b.top - M - 6;
      const flip = below < 220 && above > below;
      setStyle({
        left,
        width: w,
        maxHeight: Math.max(140, Math.min(560, flip ? above : below)),
        ...(flip ? { bottom: window.innerHeight - b.top + 6 } : { top: b.bottom + 6 }),
      });
    };
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [anchor, width]);
  return style;
}

function ModesPanel({
  anchor,
  channels,
  source,
  caps,
  modes,
  shield,
  onPick,
  onModes,
  onShield,
  reload,
  onClose,
}: {
  anchor: React.RefObject<HTMLButtonElement | null>;
  channels: SourceRef[];
  source: SourceRef;
  caps: ModCaps;
  modes: HelixChatSettings | null;
  shield: boolean;
  onPick: (key: string) => void;
  onModes: (m: HelixChatSettings) => void;
  onShield: (on: boolean) => void;
  reload: () => Promise<void>;
  onClose: () => void;
}) {
  const style = useAnchoredPanel(anchor);
  const ref = useRef<HTMLDivElement>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const key = sourceKey(source);

  useEffect(() => {
    let alive = true;
    setLoading(true);
    setError("");
    reload().finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (anchor.current?.contains(e.target as Node)) return; // the button toggles
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const esc = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("keydown", esc, true);
    window.addEventListener("blur", onClose);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("keydown", esc, true);
      window.removeEventListener("blur", onClose);
    };
  }, [anchor, onClose]);

  const patch = async (body: Record<string, unknown>) => {
    setBusy(true);
    setError("");
    try {
      const r = await postJson(source, "/api/chat/settings", body);
      onModes((await r.json()) as HelixChatSettings);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const toggleShield = async (v: boolean) => {
    setBusy(true);
    setError("");
    try {
      const r = await postJson(source, "/api/chat/shield", { is_active: v }, "PUT");
      const data = await r.json();
      onShield(typeof data?.is_active === "boolean" ? data.is_active : v);
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const disabled = !caps.modes || busy;
  const row = "flex items-center justify-between gap-3 text-[var(--color-text)]";
  const heading = "mb-1 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-faint)]";

  return createPortal(
    <div
      ref={ref}
      className="fixed z-[90] space-y-3 overflow-y-auto overscroll-contain rounded-lg border border-[var(--color-line)] bg-[var(--color-raised)] p-3 text-[12px] shadow-2xl"
      style={style ?? { left: -9999, top: -9999 }}
    >
      <div className="flex items-center gap-2">
        {channels.length > 1 ? (
          <select className={inputCls + " min-w-0 flex-1 py-0.5"} value={key} onChange={(e) => onPick(e.target.value)} aria-label="Channel">
            {channels.map((s) => (
              <option key={sourceKey(s)} value={sourceKey(s)}>
                {s.channel}
              </option>
            ))}
          </select>
        ) : (
          <div className="min-w-0 flex-1 truncate font-semibold">{source.channel} channel modes</div>
        )}
        <button type="button" onClick={onClose} aria-label="Close" className="rounded p-0.5 text-[var(--color-faint)] hover:text-[var(--color-text)]">
          <XIcon className="h-3.5 w-3.5" />
        </button>
      </div>

      <label className={row}>
        Shield Mode
        <Toggle checked={shield} disabled={!caps.shield || busy || loading} onChange={toggleShield} label="Shield Mode" />
      </label>
      {!caps.modes && <p className="text-amber-300">Log in again to enable chat mode controls.</p>}
      {loading && !modes ? (
        <div className="space-y-2">
          <div className="h-3.5 w-32 animate-pulse rounded bg-white/10" />
          <div className="h-3.5 w-24 animate-pulse rounded bg-white/10" />
        </div>
      ) : (
        modes && (
          <>
            <label className={row}>
              Subscribers-Only
              <Toggle checked={!!modes.subscriber_mode} disabled={disabled} onChange={(v) => patch({ subscriber_mode: v })} label="Subscribers-Only" />
            </label>
            <label className={row}>
              Emote-Only
              <Toggle checked={!!modes.emote_mode} disabled={disabled} onChange={(v) => patch({ emote_mode: v })} label="Emote-Only" />
            </label>
            <label className={row}>
              Unique (R9K)
              <Toggle checked={!!modes.unique_chat_mode} disabled={disabled} onChange={(v) => patch({ unique_chat_mode: v })} label="Unique (R9K)" />
            </label>
            <div className={row}>
              <span>Followers-Only</span>
              <select
                className={selectCls}
                disabled={disabled}
                aria-label="Followers-Only"
                value={modes.follower_mode ? (modes.follower_mode_duration ?? 0) : -1}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  patch(v < 0 ? { follower_mode: false } : { follower_mode: true, follower_mode_duration: v });
                }}
              >
                {withCurrent(FOLLOWER_MODE_OPTIONS, modes.follower_mode ? (modes.follower_mode_duration ?? 0) : -1, (v) => `${v}m`).map((o) => (
                  <option key={o.v} value={o.v}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
            <div className={row}>
              <span>Slow Mode</span>
              <select
                className={selectCls}
                disabled={disabled}
                aria-label="Slow Mode"
                value={modes.slow_mode ? (modes.slow_mode_wait_time ?? 0) : 0}
                onChange={(e) => {
                  const v = Number(e.target.value);
                  patch(v <= 0 ? { slow_mode: false } : { slow_mode: true, slow_mode_wait_time: v });
                }}
              >
                {withCurrent(
                  SLOW_MODE_OPTIONS.map((s) => ({ v: s, label: s === 0 ? "Off" : `${s}s` })),
                  modes.slow_mode ? (modes.slow_mode_wait_time ?? 0) : 0,
                  (v) => `${v}s`,
                ).map((o) => (
                  <option key={o.v} value={o.v}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
          </>
        )
      )}
      {error && <p className="text-red-300">{error}</p>}
      <BlockedTermsSection key={key} source={source} caps={caps} heading={heading} />
    </div>,
    document.body,
  );
}

/** The options, plus the channel's current value when it is one the list doesn't have (set elsewhere). */
function withCurrent(opts: { v: number; label: string }[], current: number, label: (v: number) => string) {
  return opts.some((o) => o.v === current) ? opts : [...opts, { v: current, label: label(current) }].sort((a, b) => a.v - b.v);
}

// Twitch has no API for PERMITTED terms (twitch-web-only): the dim note says so rather than pretending a section
// for them exists.
function BlockedTermsSection({ source, caps, heading }: { source: SourceRef; caps: ModCaps; heading: string }) {
  const [open, setOpen] = useState(false);
  const [loading, setLoading] = useState(false);
  const [terms, setTerms] = useState<{ id: string; text: string }[]>([]);
  const [newTerm, setNewTerm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const r = await relayApi(source, "/api/chat/terms");
      if (r.ok) {
        const data = await r.json();
        setTerms(Array.isArray(data?.terms) ? data.terms.slice(0, 100) : []);
      }
    } catch {
      /* the list just stays empty/stale */
    } finally {
      setLoading(false);
    }
  }, [source]);

  useEffect(() => {
    if (open && caps.terms) load();
  }, [open, caps.terms, load]);

  const addTerm = async () => {
    const text = newTerm.trim();
    if (!text || busy) return;
    setBusy(true);
    setError("");
    try {
      await postJson(source, "/api/chat/terms", { text });
      setNewTerm("");
      await load();
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const removeTerm = async (id: string) => {
    setBusy(true);
    setError("");
    try {
      await postJson(source, `/api/chat/terms?id=${encodeURIComponent(id)}`, undefined, "DELETE");
      setTerms((prev) => prev.filter((x) => x.id !== id));
    } catch (e) {
      setError(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border-t border-[var(--color-line)] pt-2.5">
      <button type="button" className={heading + " flex w-full items-center justify-between hover:text-[var(--color-text)]"} onClick={() => setOpen((v) => !v)}>
        Blocked terms
        <span>{open ? "▾" : "▸"}</span>
      </button>
      {open && (
        <div className="mt-1.5 space-y-2">
          {!caps.terms && <p className="text-amber-300">Log in again to manage blocked terms.</p>}
          {loading ? (
            <div className="h-3.5 w-24 animate-pulse rounded bg-white/10" />
          ) : (
            <div className="max-h-32 space-y-1 overflow-y-auto">
              {terms.length === 0 && <p className="text-[var(--color-faint)]">No blocked terms.</p>}
              {terms.map((t) => (
                <div key={t.id} className="flex items-center gap-1.5 text-[var(--color-text)]">
                  <span className="min-w-0 flex-1 truncate">{t.text}</span>
                  <button
                    type="button"
                    disabled={!caps.terms || busy}
                    onClick={() => removeTerm(t.id)}
                    aria-label={`Remove ${t.text}`}
                    className="shrink-0 rounded p-0.5 text-[var(--color-faint)] hover:text-red-400 disabled:opacity-40"
                  >
                    <XIcon className="h-3.5 w-3.5" />
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="flex items-center gap-1.5">
            <input
              className={inputCls + " min-w-0 flex-1"}
              placeholder="Add a blocked term"
              value={newTerm}
              disabled={!caps.terms || busy}
              onChange={(e) => setNewTerm(e.target.value)}
              maxLength={100}
              onKeyDown={(e) => {
                e.stopPropagation();
                if (e.key === "Enter") {
                  e.preventDefault();
                  addTerm();
                }
              }}
            />
            <button
              type="button"
              className="shrink-0 rounded border border-[var(--color-line)] px-2 py-1 text-[var(--color-text)] hover:bg-[var(--color-raised-2)] disabled:opacity-40"
              disabled={!caps.terms || busy || !newTerm.trim()}
              onClick={addTerm}
            >
              Add
            </button>
          </div>
          {error && <p className="text-red-300">{error}</p>}
          <p className="text-[11px] text-[var(--color-faint)]">Permitted terms aren't exposed by Twitch's API.</p>
        </div>
      )}
    </div>
  );
}
