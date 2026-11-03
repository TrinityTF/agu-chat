// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): PinnedCard, MenuItem and
// UnpinAfter (the card Twitch shows at the top of chat). Changes: restyled on the theme tokens, takes
// the pane's TwitchEnv, the pin / unpin / duration / hide callbacks are wired by the pane.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { ChevronIcon, ClockIcon, DotsIcon, EyeOffIcon, MinusCircleIcon, PinIcon } from "../../icons";
import { Badges, MessageText } from "../parts";
import { nameColor } from "../twitch/colors";
import { buildParts } from "../twitch/parts";
import type { ChatMsg } from "../types";
import type { TwitchEnv } from "./env";

// Twitch takes 30 seconds to 30 minutes, or "until unpinned / stream ends".
const PIN_MIN_S = 30;
const PIN_MAX_S = 1800;

export function PinnedCard({
  msg,
  channelChip,
  pinner,
  env,
  fontPx,
  readableColors,
  canMod,
  onUnpin,
  onDuration,
  onHide,
}: {
  msg: ChatMsg;
  /** "#login" when more than one channel's pin is showing. */
  channelChip?: string;
  /** The pinner's badges and colour, from their latest line in this chat. */
  pinner: { badges: string; color: string } | null;
  env: TwitchEnv;
  fontPx: number;
  readableColors: boolean;
  canMod: boolean;
  onUnpin: () => void;
  /** Seconds from now, or null for until unpinned / the stream ends. */
  onDuration: (seconds: number | null) => Promise<boolean>;
  onHide: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [menu, setMenu] = useState<null | "main" | "after">(null);
  const [now, setNow] = useState(() => Date.now());
  const menuRef = useRef<HTMLDivElement>(null);
  const endsAt = msg.pin?.endsAt ? Date.parse(msg.pin.endsAt) : null;

  // Ticks only while there's an end time, so the card goes away on time rather than at the relay's next poll.
  useEffect(() => {
    if (!endsAt) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [endsAt]);

  useEffect(() => {
    if (!menu) return;
    const onDoc = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenu(null);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [menu]);

  if (endsAt && endsAt <= now) return null;

  const by = msg.pin?.by;
  const sentAt = new Date(msg.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const left = endsAt ? Math.max(0, Math.round((endsAt - now) / 1000)) : null;
  const leftText = left == null ? null : left >= 60 ? `${Math.ceil(left / 60)} min` : `${left}s`;
  const iconBtn = "grid h-6 w-6 shrink-0 place-items-center rounded-full text-muted hover:bg-raised-2 hover:text-text";

  return (
    <div className="relative mx-1.5 mt-1.5 rounded border border-line bg-raised px-2.5 py-1.5 shadow-lg shadow-black/30">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-1 text-[11px] text-muted">
            <PinIcon className="h-3 w-3 shrink-0" />
            {channelChip && <span className="shrink-0 rounded bg-white/10 px-1 text-[10px] font-semibold">{channelChip}</span>}
            <span className="truncate">Pinned{by?.name ? " by " : ""}</span>
            {by?.name && (
              <>
                {pinner && <Badges tag={pinner.badges} map={env.badgeMap} />}
                <span className="truncate font-semibold text-text">{by.name}</span>
              </>
            )}
          </div>
          {open && msg.reply && (
            <div className="mt-1 truncate text-xs text-muted">
              Replying to <span className="font-semibold">@{msg.reply.user}</span>: {msg.reply.text}
            </div>
          )}
          <div className={`mt-0.5 break-words font-semibold text-text ${open ? "" : "line-clamp-1"}`} style={{ fontSize: fontPx }}>
            <MessageText parts={buildParts(msg.text, msg.emotes, env.emotes, null, msg.gifs)} size="small" fontPx={fontPx} emotes={env.emotes} />
          </div>
          {open && (
            <div className="mt-1 flex flex-wrap items-center gap-1 text-xs text-muted">
              <Badges tag={msg.badges} map={env.badgeMap} />
              <span className="font-semibold" style={{ color: nameColor(msg.user, readableColors) }}>
                {msg.user.name}
              </span>
              <span>sent at {sentAt}</span>
              {leftText && <span className="text-faint">· unpins in {leftText}</span>}
            </div>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-0.5" ref={menuRef}>
          <button type="button" className={iconBtn} title="More" onClick={() => setMenu(menu ? null : "main")}>
            <DotsIcon className="h-4 w-4" />
          </button>
          <button type="button" className={iconBtn} title={open ? "Collapse" : "Expand"} onClick={() => setOpen(!open)}>
            <ChevronIcon className={`h-4 w-4 transition-transform ${open ? "rotate-180" : ""}`} />
          </button>
          {menu && (
            <div className="absolute right-2 top-9 z-30 w-56 rounded border border-line bg-raised p-1 text-sm text-text shadow-xl shadow-black/40">
              {menu === "main" ? (
                <>
                  {canMod && (
                    <>
                      <MenuItem
                        icon={<MinusCircleIcon className="h-4 w-4" />}
                        label="Unpin this Message"
                        onClick={() => {
                          setMenu(null);
                          onUnpin();
                        }}
                      />
                      <MenuItem icon={<ClockIcon className="h-4 w-4" />} label="Unpin After" more onClick={() => setMenu("after")} />
                    </>
                  )}
                  <MenuItem
                    icon={<EyeOffIcon className="h-4 w-4" />}
                    label="Hide for Yourself"
                    onClick={() => {
                      setMenu(null);
                      onHide();
                    }}
                  />
                </>
              ) : (
                <UnpinAfter
                  left={left}
                  onBack={() => setMenu("main")}
                  onSave={async (secs) => {
                    if (await onDuration(secs)) setMenu(null);
                  }}
                />
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function MenuItem({ icon, label, more, onClick }: { icon: ReactNode; label: string; more?: boolean; onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} className="flex w-full items-center gap-2.5 rounded px-2.5 py-1.5 text-left hover:bg-raised-2">
      <span className="text-muted">{icon}</span>
      <span className="flex-1">{label}</span>
      {more && <ChevronIcon className="h-4 w-4 -rotate-90 text-muted" />}
    </button>
  );
}

function UnpinAfter({ left, onBack, onSave }: { left: number | null; onBack: () => void; onSave: (seconds: number | null) => void }) {
  // Starts from what's left on the pin now, in minutes when it's a minute or more.
  const [mode, setMode] = useState<"time" | "end">(left == null ? "end" : "time");
  const [unit, setUnit] = useState<"min" | "sec">(left != null && left < 60 ? "sec" : "min");
  const [amount, setAmount] = useState(() => String(left == null ? 20 : left >= 60 ? Math.ceil(left / 60) : left));
  const [busy, setBusy] = useState(false);
  const secs = Math.round(Number(amount) * (unit === "min" ? 60 : 1));
  const valid = mode === "end" || (Number.isFinite(secs) && secs >= PIN_MIN_S && secs <= PIN_MAX_S);
  return (
    <div className="p-1.5">
      <div className="relative mb-2 flex items-center justify-center">
        <button type="button" onClick={onBack} className="absolute left-0 grid h-6 w-6 place-items-center rounded hover:bg-raised-2" title="Back">
          <ChevronIcon className="h-4 w-4 rotate-90" />
        </button>
        <span className="font-semibold">Unpin After</span>
      </div>
      <label className="flex cursor-pointer items-center gap-2 py-1">
        <input type="radio" className="accent-purple-500" checked={mode === "time"} onChange={() => setMode("time")} />
        Customized time
      </label>
      <div className="flex gap-2 pb-1 pl-6">
        <input
          type="number"
          min={unit === "min" ? 1 : PIN_MIN_S}
          max={unit === "min" ? 30 : PIN_MAX_S}
          value={amount}
          onFocus={() => setMode("time")}
          onChange={(e) => setAmount(e.target.value)}
          className="w-16 rounded border border-line bg-panel px-2 py-1 text-center"
        />
        <select value={unit} onFocus={() => setMode("time")} onChange={(e) => setUnit(e.target.value as "min" | "sec")} className="rounded border border-line px-2 py-1">
          <option value="min">min</option>
          <option value="sec">sec</option>
        </select>
      </div>
      {mode === "time" && !valid && <p className="pl-6 text-[11px] text-amber-400">30 seconds to 30 minutes</p>}
      <label className="flex cursor-pointer items-center gap-2 py-1">
        <input type="radio" className="accent-purple-500" checked={mode === "end"} onChange={() => setMode("end")} />
        Manually unpinned or end of stream
      </label>
      <button
        type="button"
        disabled={!valid || busy}
        onClick={async () => {
          setBusy(true);
          await onSave(mode === "end" ? null : secs);
          setBusy(false);
        }}
        className="mt-2 w-full rounded bg-accent py-1 font-semibold text-white hover:bg-accent-strong disabled:opacity-40"
      >
        Save
      </button>
    </div>
  );
}
