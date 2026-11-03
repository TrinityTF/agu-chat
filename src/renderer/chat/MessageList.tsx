// The scrolling message list. Auto-scroll logic ported from Chat() in modtools.agu.lol
// web/src/app/pages/Chat.tsx (commit 1b826c2: atBottomRef / handleScroll / resume, 40px threshold).
// Changes: a "More messages below" bar that shows how many arrived while paused; a ResizeObserver keeps the
// list pinned to the bottom when the split is resized or a GIF finishes loading; the store is told when the
// user is holding the list (so it does not trim rows from under them); rows are rendered by the caller.
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import type { Row } from "./types";

const BOTTOM_PX = 40;

export function MessageList({
  rows,
  renderRow,
  empty,
  onHold,
}: {
  rows: Row[];
  renderRow: (row: Row) => ReactNode;
  /** Shown when there are no rows. */
  empty: ReactNode;
  /** The user scrolled up (true) or came back to the bottom (false). */
  onHold: (held: boolean) => void;
}) {
  const listRef = useRef<HTMLDivElement | null>(null);
  const innerRef = useRef<HTMLDivElement | null>(null);
  const atBottomRef = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  // The newest row's arrival number at the moment the user scrolled up: everything after it is "new".
  const [pauseMark, setPauseMark] = useState(0);
  const lastOrdRef = useRef(0);
  const last = rows[rows.length - 1];
  lastOrdRef.current = last?.ord ?? 0;

  const toBottom = useCallback(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  // Stick to the bottom on new content, unless the user has scrolled up.
  useLayoutEffect(() => {
    if (atBottomRef.current) toBottom();
  }, [rows, toBottom]);

  useEffect(() => {
    const inner = innerRef.current;
    if (!inner || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (atBottomRef.current) toBottom();
    });
    ro.observe(inner);
    if (listRef.current) ro.observe(listRef.current);
    return () => ro.disconnect();
  }, [toBottom]);

  const onScroll = () => {
    const el = listRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const now = dist <= BOTTOM_PX;
    if (now === atBottomRef.current) return;
    atBottomRef.current = now;
    setAtBottom(now);
    if (!now) setPauseMark(lastOrdRef.current);
    onHold(!now);
  };

  const resume = () => {
    atBottomRef.current = true;
    setAtBottom(true);
    onHold(false);
    toBottom();
    // Once more after layout: rows trimmed by the store change the height.
    requestAnimationFrame(toBottom);
  };

  let newCount = 0;
  if (!atBottom) for (let i = rows.length - 1; i >= 0 && (rows[i].ord ?? 0) > pauseMark; i--) newCount++;

  return (
    <div className="relative min-h-0 flex-1">
      <div
        ref={listRef}
        onScroll={onScroll}
        className="h-full overflow-y-auto overflow-x-hidden"
        // While following the bottom we position the scroll ourselves; scrolled up, native anchoring keeps
        // the line being read still when rows are added below or trimmed above.
        style={{ overflowAnchor: atBottom ? "none" : "auto" }}
      >
        <div ref={innerRef} className="py-1">
          {rows.length === 0 ? empty : rows.map(renderRow)}
        </div>
      </div>
      {!atBottom && (
        <button
          type="button"
          onClick={resume}
          className="absolute bottom-2 left-1/2 z-10 -translate-x-1/2 rounded-full border border-accent/50 bg-raised px-3 py-1 text-xs font-semibold text-accent-soft shadow-xl shadow-black/50 hover:bg-raised-2"
        >
          More messages below{newCount > 0 ? ` (${newCount > 999 ? "999+" : newCount})` : ""}
        </button>
      )}
    </div>
  );
}
