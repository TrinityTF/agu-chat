// The shared dense row shell (Chatterino look: no cards or bubbles, ~13px text, tight line height,
// optional alternating background, optional HH:MM[:SS] timestamp in --color-faint, an optional platform
// mark, then whatever the platform's row puts inline: mod buttons, badges, name, colon, message).
import { memo, type CSSProperties, type ReactNode } from "react";
import type { Platform } from "../../protocol";
import { PlatformIcon } from "../../platformIcons";
import { CopyIcon, ReplyIcon, TrashIcon, PinIcon, MinusCircleIcon, ClockIcon } from "../../icons";
import { fmtTime } from "../ui";
import { LINE_HEIGHT } from "../parts";
import type { RowCtx } from "./env";

export function RowFrame({
  ctx,
  n,
  ts,
  platform,
  className = "",
  style,
  before,
  mod,
  hover,
  label,
  children,
}: {
  ctx: RowCtx;
  n: number | undefined;
  ts: number;
  platform: Platform;
  className?: string;
  style?: CSSProperties;
  /** Above the first line (a reply's context). */
  before?: ReactNode;
  /** Inline mod buttons, rendered at the very start of the line. */
  mod?: ReactNode;
  /** Floating hover toolbar. */
  hover?: ReactNode;
  /** Small uppercase tag in the top-right corner (FIRST MESSAGE, ...). */
  label?: { text: string; className: string } | null;
  children: ReactNode;
}) {
  const { rs } = ctx;
  const alt = rs.alternateRows && (n ?? 0) % 2 === 1;
  return (
    <div
      className={`group relative break-words px-2 py-px ${alt && !style?.background ? "bg-panel-alt" : ""} ${className}`}
      style={{ lineHeight: LINE_HEIGHT, ...style }}
    >
      {before}
      <div>
        {mod}
        {rs.timestamps && (
          <span className="mr-1.5 select-none tabular-nums text-faint" style={{ fontSize: "0.85em" }}>
            {fmtTime(ts, rs.timestampSeconds)}
          </span>
        )}
        {ctx.showPlatform && <PlatformIcon platform={platform} className="mr-1 inline-block h-[0.95em] w-[0.95em] align-[-0.12em]" />}
        {children}
      </div>
      {label && <span className={`absolute right-1.5 top-0.5 select-none text-[9px] font-bold tracking-wide ${label.className}`}>{label.text}</span>}
      {hover && (
        <div className="absolute right-1.5 top-0 z-10 flex -translate-y-1/2 items-center gap-0.5 rounded border border-line bg-raised px-0.5 py-0.5 opacity-0 shadow-lg shadow-black/50 transition-opacity group-hover:opacity-100">
          {hover}
        </div>
      )}
    </div>
  );
}

const hoverBtn = "rounded p-1 text-muted hover:bg-raised-2 hover:text-text";

export function HoverButtons({
  onCopy,
  onReply,
  onPin,
  onDelete,
}: {
  onCopy?: () => void;
  onReply?: () => void;
  onPin?: () => void;
  onDelete?: () => void;
}) {
  return (
    <>
      {onCopy && (
        <button type="button" title="Copy" onClick={onCopy} className={hoverBtn}>
          <CopyIcon className="h-3.5 w-3.5" />
        </button>
      )}
      {onReply && (
        <button type="button" title="Reply" onClick={onReply} className={hoverBtn}>
          <ReplyIcon className="h-3.5 w-3.5" />
        </button>
      )}
      {onPin && (
        <button type="button" title="Pin" onClick={onPin} className={hoverBtn}>
          <PinIcon className="h-3.5 w-3.5" />
        </button>
      )}
      {onDelete && (
        <button type="button" title="Delete message" onClick={onDelete} className={`${hoverBtn} hover:!text-red-400`}>
          <TrashIcon className="h-3.5 w-3.5" />
        </button>
      )}
    </>
  );
}

const modBtn = "inline-flex h-[1.15em] w-[1.15em] items-center justify-center rounded align-[-0.18em] text-faint hover:bg-raised-2";

/** Chatterino-style inline mod buttons: ban, timeout (10m), delete. Each is optional (rights / reach). */
export const ModButtons = memo(function ModButtons({
  onBan,
  onTimeout,
  onDelete,
  timeoutTitle = "Timeout 10m",
}: {
  onBan?: (target: HTMLElement) => void;
  onTimeout?: () => void;
  onDelete?: () => void;
  timeoutTitle?: string;
}) {
  if (!onBan && !onTimeout && !onDelete) return null;
  return (
    <span className="mr-1 inline-flex items-center gap-px select-none">
      {onBan && (
        <button type="button" title="Ban" onClick={(e) => onBan(e.currentTarget)} className={`${modBtn} hover:text-red-400`}>
          <MinusCircleIcon className="h-3 w-3" />
        </button>
      )}
      {onTimeout && (
        <button type="button" title={timeoutTitle} onClick={onTimeout} className={`${modBtn} hover:text-amber-400`}>
          <ClockIcon className="h-3 w-3" />
        </button>
      )}
      {onDelete && (
        <button type="button" title="Delete message" onClick={onDelete} className={`${modBtn} hover:text-red-400`}>
          <TrashIcon className="h-3 w-3" />
        </button>
      )}
    </span>
  );
});
