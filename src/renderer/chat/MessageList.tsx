// The scrolling message list. Auto-scroll logic ported from Chat() in modtools.agu.lol
// web/src/app/pages/Chat.tsx (commit 1b826c2: atBottomRef / handleScroll / resume, 40px threshold).
// Changes: a "More messages below" bar that shows how many arrived while paused; a ResizeObserver keeps the
// list pinned to the bottom when the split is resized or a GIF finishes loading; the store is told when the
// user is holding the list (so it does not trim rows from under them); rows are rendered by the caller;
// Ctrl+F in the focused split opens a search bar that filters the list (chat/search.ts), Esc closes it;
// Settings > Pause chat can also hold it still while the mouse is over it or while Alt is held.
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { SearchIcon, XIcon } from "../icons";
import { isEmptyQuery, matchRow, parseSearch } from "./search";
import type { Row } from "./types";

const BOTTOM_PX = 40;

export function MessageList({
  rows,
  renderRow,
  empty,
  onHold,
  focused = false,
  pauseOn = "scroll",
}: {
  rows: Row[];
  renderRow: (row: Row) => ReactNode;
  /** Shown when there are no rows. */
  empty: ReactNode;
  /** The user scrolled up or is searching (true), or is back on the live view (false). */
  onHold: (held: boolean) => void;
  /** This split has keyboard focus: Ctrl+F opens its search. */
  focused?: boolean;
  /** Besides scrolling up: "hover" pauses while the mouse is over the list, "alt" while Alt is held. */
  pauseOn?: "scroll" | "hover" | "alt";
}) {
  const rootRef = useRef<HTMLDivElement | null>(null);
  const listRef = useRef<HTMLDivElement | null>(null);
  const innerRef = useRef<HTMLDivElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);
  const atBottomRef = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  // The newest row's arrival number at the moment the user scrolled up: everything after it is "new".
  const [pauseMark, setPauseMark] = useState(0);
  const [searchOpen, setSearchOpen] = useState(false);
  const [query, setQuery] = useState("");

  const parsed = useMemo(() => parseSearch(query), [query]);
  const filtering = searchOpen && !isEmptyQuery(parsed);
  const shown = useMemo(() => (filtering ? rows.filter((r) => matchRow(r, parsed)) : rows), [rows, parsed, filtering]);

  const lastOrdRef = useRef(0);
  const last = shown[shown.length - 1];
  lastOrdRef.current = last?.ord ?? 0;

  const toBottom = useCallback(() => {
    const el = listRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);

  // Stick to the bottom on new content, unless the user has scrolled up.
  useLayoutEffect(() => {
    if (atBottomRef.current) toBottom();
  }, [shown, toBottom]);

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

  // While the search bar is open the store keeps its history (same as scrolling up), so matches don't
  // get trimmed away while you read them.
  const searchOpenRef = useRef(false);
  searchOpenRef.current = searchOpen;

  // A hover / Alt pause: held like a scroll-up while it lasts, then back to live unless the user scrolled up.
  const heldRef = useRef(false);
  const [held, setHeld] = useState(false);
  const hold = useCallback(() => {
    if (heldRef.current || !atBottomRef.current) return;
    heldRef.current = true;
    setHeld(true);
    atBottomRef.current = false;
    setAtBottom(false);
    setPauseMark(lastOrdRef.current);
    if (!searchOpenRef.current) onHold(true);
  }, [onHold]);
  const releaseRef = useRef(() => {});

  const onScroll = () => {
    const el = listRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const now = dist <= BOTTOM_PX;
    if (heldRef.current && now) return; // still at the bottom, still paused
    if (now === atBottomRef.current) return;
    atBottomRef.current = now;
    setAtBottom(now);
    if (!now) setPauseMark(lastOrdRef.current);
    if (!searchOpenRef.current) onHold(!now);
  };

  const resume = useCallback(() => {
    heldRef.current = false;
    setHeld(false);
    atBottomRef.current = true;
    setAtBottom(true);
    if (!searchOpenRef.current) onHold(false);
    toBottom();
    // Once more after layout: rows trimmed by the store change the height.
    requestAnimationFrame(toBottom);
  }, [onHold, toBottom]);

  releaseRef.current = () => {
    if (!heldRef.current) return;
    heldRef.current = false;
    setHeld(false);
    const el = listRef.current;
    if (!el || el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_PX) resume();
  };

  useEffect(() => {
    if (pauseOn !== "alt") return;
    const down = (e: KeyboardEvent) => {
      if (e.key === "Alt") hold();
    };
    const up = (e: KeyboardEvent) => {
      if (e.key === "Alt") releaseRef.current();
    };
    const blur = () => releaseRef.current();
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
      window.removeEventListener("blur", blur);
      releaseRef.current();
    };
  }, [pauseOn, hold]);
  useEffect(() => {
    if (pauseOn !== "hover") releaseRef.current();
  }, [pauseOn]);

  const openSearch = useCallback(() => {
    if (!searchOpenRef.current) {
      searchOpenRef.current = true;
      setSearchOpen(true);
      onHold(true);
    }
    requestAnimationFrame(() => {
      searchRef.current?.focus();
      searchRef.current?.select();
    });
  }, [onHold]);

  const closeSearch = useCallback(() => {
    searchOpenRef.current = false;
    setSearchOpen(false);
    setQuery("");
    resume();
  }, [resume]);

  // Ctrl+F: only the focused split's list reacts.
  const focusedRef = useRef(focused);
  focusedRef.current = focused;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;
      if ((e.ctrlKey || e.metaKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f") {
        if (!focusedRef.current || document.querySelector('[role="dialog"]')) return;
        e.preventDefault();
        openSearch();
      } else if (e.key === "Escape" && searchOpenRef.current) {
        // Esc from anywhere inside this list (the search box handles its own Esc).
        const root = rootRef.current;
        if (!root || !root.contains(document.activeElement)) return;
        e.preventDefault();
        closeSearch();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [openSearch, closeSearch]);

  // A new query shows the newest matches first.
  useLayoutEffect(() => {
    if (!searchOpen) return;
    atBottomRef.current = true;
    setAtBottom(true);
    toBottom();
  }, [query, searchOpen, toBottom]);

  let newCount = 0;
  if (!atBottom) for (let i = shown.length - 1; i >= 0 && (shown[i].ord ?? 0) > pauseMark; i--) newCount++;

  return (
    <div ref={rootRef} className="relative flex min-h-0 flex-1 flex-col">
      {searchOpen && (
        <div className="flex shrink-0 items-center gap-1.5 border-b border-line bg-raised px-1.5 py-1 text-xs">
          <SearchIcon className="h-3.5 w-3.5 shrink-0 text-faint" />
          <input
            ref={searchRef}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                closeSearch();
              }
            }}
            placeholder="Search messages, from:name"
            spellCheck={false}
            aria-label="Search messages"
            className="min-w-0 flex-1 bg-transparent text-text outline-none placeholder:text-faint"
          />
          <span className="shrink-0 tabular-nums text-faint" aria-live="polite">
            {filtering ? `${shown.length} ${shown.length === 1 ? "match" : "matches"}` : `${rows.length} messages`}
          </span>
          <button type="button" title="Close search (Esc)" onClick={closeSearch} className="shrink-0 rounded p-0.5 text-faint hover:bg-raised-2 hover:text-text">
            <XIcon className="h-3.5 w-3.5" />
          </button>
        </div>
      )}
      <div
        className="relative min-h-0 flex-1"
        onMouseEnter={pauseOn === "hover" ? hold : undefined}
        onMouseLeave={pauseOn === "hover" ? () => releaseRef.current() : undefined}
      >
        <div
          ref={listRef}
          onScroll={onScroll}
          className="h-full overflow-y-auto overflow-x-hidden"
          // While following the bottom we position the scroll ourselves; scrolled up, native anchoring keeps
          // the line being read still when rows are added below or trimmed above.
          style={{ overflowAnchor: atBottom ? "none" : "auto" }}
        >
          <div ref={innerRef} className="py-1">
            {shown.length === 0 ? (filtering ? <div className="px-4 py-10 text-center text-xs text-faint">No matching messages.</div> : empty) : shown.map(renderRow)}
          </div>
        </div>
        {!atBottom && (
          <button
            type="button"
            onClick={resume}
            className="absolute bottom-2 left-1/2 z-10 -translate-x-1/2 rounded-full border border-accent/50 bg-raised px-3 py-1 text-xs font-semibold text-accent-soft shadow-xl shadow-black/50 hover:bg-raised-2"
          >
            {searchOpen ? "Newest matches" : held ? "Chat paused" : "More messages below"}
            {newCount > 0 ? ` (${newCount > 999 ? "999+" : newCount})` : ""}
          </button>
        )}
      </div>
    </div>
  );
}
