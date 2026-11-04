// The notebook's tab bar, which is also the window's title bar (the window has no OS frame): select, add,
// close the selected tab (x / middle-click), rename (double-click), drag to reorder and the window buttons.
// The bar never scrolls: when the tabs don't all fit it shows a run of them that always holds the selected one
// (tabWindow.ts), and the arrow at the left edge lists every tab. Empty space drags the window; double-click
// maximizes.
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Tab } from "../protocol";
import { agu } from "../bridge";
import { ChevronIcon, WinMaxIcon, WinMinIcon, WinRestoreIcon, XIcon } from "../icons";
import { PopupMenu, anchorOf, type MenuItem } from "./ui";
import { autoTitle, tabTitle } from "./tree";
import { tabWindow } from "./tabWindow";
import { useHotkeyHint } from "./hotkeyContext";
import type { Activity } from "../chat/ChatPane";

export type TabActions = {
  select(id: string): void;
  add(): void;
  close(id: string): void;
  closeOthers(id: string): void;
  rename(id: string, title: string): void;
  /** Move tab `id` to sit before the tab currently at `beforeIndex` (tabs.length = the end). */
  reorder(id: string, beforeIndex: number): void;
  /** A split of the selected tab (or some of its channels, `keys`) dropped on the bar: a new tab at `beforeIndex`. */
  fromSplit(leafId: string, keys: string[] | null, beforeIndex: number): void;
};

// What a split header (a whole split) or a header chip (some of its channels) puts on the drag.
const SPLIT_TYPE = "application/x-agu-split";
const SOURCES_TYPE = "application/x-agu-sources";
const splitDrag = (e: React.DragEvent) => e.dataTransfer.types.includes(SPLIT_TYPE) || e.dataTransfer.types.includes(SOURCES_TYPE);

/** The split or channels a drop carries, or null for anything else. */
function droppedSplit(e: React.DragEvent): { leafId: string; keys: string[] | null } | null {
  const leafId = e.dataTransfer.getData(SPLIT_TYPE);
  if (leafId) return { leafId, keys: null };
  try {
    const d = JSON.parse(e.dataTransfer.getData(SOURCES_TYPE) || "null");
    return d && typeof d.leafId === "string" && Array.isArray(d.keys) ? { leafId: d.leafId, keys: d.keys } : null;
  } catch {
    return null;
  }
}

const ALL_W = 25; // the All tabs button, border included

export function NotebookTabs({
  tabs,
  active,
  actions,
  unread = {},
  onDragTab,
}: {
  tabs: Tab[];
  active: string;
  actions: TabActions;
  /** Unselected tabs that got live chat: "message" = new messages, "highlight" = a highlight or mention. */
  unread?: Record<string, Activity>;
  /** A tab drag started (its id) or ended (null): the splits below show where it can be dropped. */
  onDragTab?: (id: string | null) => void;
}) {
  const closeTabKey = useHotkeyHint("closeTab");
  const newTabKey = useHotkeyHint("newTab");
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null); // insertion index while dragging
  const [splitOver, setSplitOver] = useState(false); // a split or channel is being dragged over the bar
  const [menu, setMenu] = useState<{ anchor: { x: number; y: number }; tabId: string } | null>(null);
  const [list, setList] = useState<ReturnType<typeof anchorOf> | null>(null);
  const [maximized, setMaximized] = useState(false);

  // ---- which tabs fit ----------------------------------------------------------------
  const zoneRef = useRef<HTMLDivElement>(null);
  const plusRef = useRef<HTMLButtonElement>(null);
  const tabEls = useRef(new Map<string, HTMLElement>()).current;
  const startRef = useRef(0);
  const [view, setView] = useState({ start: 0, end: Number.MAX_SAFE_INTEGER });
  const fitRef = useRef(() => {});
  fitRef.current = () => {
    const zone = zoneRef.current;
    if (!zone) return;
    const widths = tabs.map((t) => tabEls.get(t.id)?.offsetWidth ?? 120);
    const full = zone.clientWidth - (plusRef.current?.offsetWidth ?? 28);
    const total = widths.reduce((a, b) => a + b, 0);
    const activeIdx = Math.max(0, tabs.findIndex((t) => t.id === active));
    const next = total <= full ? { start: 0, end: tabs.length - 1 } : tabWindow(widths, full - ALL_W, activeIdx, startRef.current);
    startRef.current = next.start;
    setView((v) => (v.start === next.start && v.end === next.end ? v : next));
  };
  // After every render (a tab added, renamed, marked unread...) and on every resize.
  useLayoutEffect(() => fitRef.current());
  useEffect(() => {
    const el = zoneRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => fitRef.current());
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  const shown = (i: number) => i >= view.start && i <= view.end;
  const hidden = tabs.filter((_, i) => !shown(i));
  const hiddenMark = hidden.some((t) => unread[t.id] === "highlight") ? "highlight" : hidden.some((t) => unread[t.id]) ? "message" : undefined;

  // However a split drag ends (dropped here, elsewhere, or cancelled), the insertion mark goes.
  useEffect(() => {
    if (!splitOver) return;
    const end = () => {
      setSplitOver(false);
      setDropAt(null);
    };
    window.addEventListener("dragend", end);
    window.addEventListener("drop", end);
    return () => {
      window.removeEventListener("dragend", end);
      window.removeEventListener("drop", end);
    };
  }, [splitOver]);
  const dropHere = (e: React.DragEvent, at: number) => {
    e.preventDefault();
    setDropAt(null);
    setSplitOver(false);
    if (dragId) {
      actions.reorder(dragId, at);
      setDragId(null);
      return;
    }
    const d = droppedSplit(e);
    if (d) actions.fromSplit(d.leafId, d.keys, at);
  };
  const dragging = !!dragId || splitOver;

  useEffect(() => {
    agu.win
      .isMaximized()
      .then(setMaximized)
      .catch(() => {});
    return agu.win.onMaximized(setMaximized);
  }, []);

  const startRename = (t: Tab) => {
    setDraft(t.title);
    setEditing(t.id);
  };
  const commit = () => {
    if (editing) actions.rename(editing, draft.trim());
    setEditing(null);
  };

  const menuItems = (id: string): MenuItem[] => {
    const t = tabs.find((x) => x.id === id);
    return [
      { label: "Rename tab", onClick: () => t && startRename(t) },
      // Only the selected tab can be closed, so a stray click never closes one you aren't looking at.
      { label: "Close tab", hint: closeTabKey, disabled: id !== active, onClick: () => actions.close(id) },
      { label: "Close other tabs", disabled: tabs.length < 2, onClick: () => actions.closeOthers(id) },
    ];
  };

  const listItems: MenuItem[] = tabs.map((t) => ({
    label: tabTitle(t, true),
    active: t.id === active,
    hint: t.id !== active && unread[t.id] ? (unread[t.id] === "highlight" ? "highlight" : "new") : undefined,
    onClick: () => actions.select(t.id),
  }));
  const winBtn = "app-no-drag flex w-9 shrink-0 items-center justify-center text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]";
  const dropEnd = Math.min(view.end, tabs.length - 1) + 1; // dropping on the empty bar: after the last shown tab

  return (
    <div
      className="flex h-7 shrink-0 select-none items-stretch border-b border-[var(--color-line)] bg-[var(--color-raised)]"
      role="tablist"
      onDragLeave={(e) => {
        if (splitOver && !e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setSplitOver(false);
          setDropAt(null);
        }
      }}
    >
      <div ref={zoneRef} className="flex min-w-0 flex-1 items-stretch">
        {hidden.length > 0 && (
          <button
            type="button"
            onClick={(e) => {
              const el = e.currentTarget;
              setList((open) => (open ? null : anchorOf(el)));
            }}
            title={`All tabs (${hidden.length} more)`}
            aria-label="All tabs"
            className="relative flex w-6 shrink-0 items-center justify-center border-r border-[var(--color-line)] text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
          >
            <ChevronIcon className="h-3.5 w-3.5" />
            {hiddenMark && (
              <span className={"absolute right-1 top-1 h-1.5 w-1.5 rounded-full " + (hiddenMark === "highlight" ? "bg-[var(--color-accent-soft)]" : "bg-[var(--color-faint)]")} />
            )}
          </button>
        )}
        <div className="relative flex min-w-0 items-stretch overflow-hidden">
          {tabs.map((t, i) => {
            const selected = t.id === active;
            const isEditing = editing === t.id;
            const mark = selected ? undefined : unread[t.id];
            const visible = shown(i);
            return (
              <div
                key={t.id}
                ref={(el) => {
                  if (el) tabEls.set(t.id, el);
                  else tabEls.delete(t.id);
                }}
                // Tabs that don't fit stay laid out off to the side, invisible, so their widths can be measured.
                role={visible ? "tab" : undefined}
                aria-hidden={visible ? undefined : true}
                aria-selected={visible ? selected : undefined}
                data-tab-id={visible ? t.id : undefined}
                draggable={visible && !isEditing}
                onClick={() => actions.select(t.id)}
                onMouseDown={(e) => {
                  if (e.button === 1) e.preventDefault();
                }}
                onAuxClick={(e) => {
                  if (e.button === 1) {
                    e.preventDefault();
                    if (selected) actions.close(t.id);
                  }
                }}
                onDoubleClick={() => startRename(t)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  setMenu({ anchor: { x: e.clientX, y: e.clientY }, tabId: t.id });
                }}
                onDragStart={(e) => {
                  e.dataTransfer.setData("application/x-agu-tab", t.id);
                  e.dataTransfer.effectAllowed = "move";
                  setDragId(t.id);
                  // Changing the DOM inside dragstart can cancel the drag, so the drop targets show a tick later.
                  setTimeout(() => onDragTab?.(t.id), 0);
                }}
                onDragEnd={() => {
                  setDragId(null);
                  setDropAt(null);
                  onDragTab?.(null);
                }}
                onDragOver={(e) => {
                  if (!dragId && !splitDrag(e)) return;
                  e.preventDefault();
                  e.dataTransfer.dropEffect = "move";
                  if (!dragId) setSplitOver(true);
                  const r = e.currentTarget.getBoundingClientRect();
                  setDropAt(e.clientX < r.left + r.width / 2 ? i : i + 1);
                }}
                onDrop={(e) => {
                  if (!dragId && !splitDrag(e)) return;
                  const r = e.currentTarget.getBoundingClientRect();
                  dropHere(e, e.clientX < r.left + r.width / 2 ? i : i + 1);
                }}
                className={
                  "flex max-w-[220px] shrink-0 cursor-default items-center gap-1 border-r border-[var(--color-line)] px-2.5 text-[12px] " +
                  (visible ? "relative " : "pointer-events-none invisible absolute left-0 top-0 h-full ") +
                  (selected
                    ? "bg-[var(--color-panel)] text-[var(--color-text)]"
                    : mark === "highlight"
                      ? "bg-[var(--color-mention)] text-[var(--color-text)] hover:bg-[var(--color-raised-2)]"
                      : mark === "message"
                        ? "text-[var(--color-text)] hover:bg-[var(--color-raised-2)]"
                        : "text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]") +
                  (dragId === t.id ? " opacity-50" : "")
                }
              >
                {isEditing ? (
                  <input
                    autoFocus
                    value={draft}
                    placeholder={autoTitle(t)}
                    onChange={(e) => setDraft(e.target.value)}
                    onFocus={(e) => e.currentTarget.select()}
                    onBlur={commit}
                    onKeyDown={(e) => {
                      e.stopPropagation();
                      if (e.key === "Enter") commit();
                      else if (e.key === "Escape") setEditing(null);
                    }}
                    onClick={(e) => e.stopPropagation()}
                    onDoubleClick={(e) => e.stopPropagation()}
                    className="w-32 rounded-sm border border-[var(--color-accent)] bg-[var(--color-app)] px-1 text-[12px] text-[var(--color-text)] outline-none"
                    aria-label="Tab name"
                  />
                ) : (
                  <>
                    {mark && (
                      <span
                        aria-label={mark === "highlight" ? "Highlighted messages" : "New messages"}
                        className={"h-1.5 w-1.5 shrink-0 rounded-full " + (mark === "highlight" ? "bg-[var(--color-accent-soft)]" : "bg-[var(--color-faint)]")}
                      />
                    )}
                    <span className="truncate" title={tabTitle(t, true)}>
                      {tabTitle(t)}
                    </span>
                  </>
                )}
                {selected && !isEditing && (
                  <button
                    type="button"
                    aria-label="Close tab"
                    title="Close tab"
                    onClick={(e) => {
                      e.stopPropagation();
                      actions.close(t.id);
                    }}
                    onMouseDown={(e) => e.stopPropagation()}
                    className="flex h-4 w-4 shrink-0 items-center justify-center rounded text-[var(--color-faint)] hover:bg-[var(--color-line)] hover:text-[var(--color-text)]"
                  >
                    <XIcon className="h-3 w-3" />
                  </button>
                )}
                {selected && <span className="absolute inset-x-0 bottom-0 h-[2px] bg-[var(--color-accent)]" />}
                {mark === "highlight" && <span className="absolute inset-x-0 top-0 h-[2px] bg-[var(--color-accent-soft)]" />}
                {dragging && dropAt === i && <span className="absolute inset-y-0 left-0 w-[2px] bg-[var(--color-accent)]" />}
                {dragging && dropAt === i + 1 && i + 1 === dropEnd && <span className="absolute inset-y-0 right-0 w-[2px] bg-[var(--color-accent)]" />}
              </div>
            );
          })}
        </div>
        <button
          ref={plusRef}
          type="button"
          onClick={actions.add}
          title={newTabKey ? `New tab (${newTabKey})` : "New tab"}
          aria-label="New tab"
          className="flex w-7 shrink-0 items-center justify-center text-[16px] leading-none text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
        >
          +
        </button>
        <div
          className="app-drag min-w-0 flex-1"
          onDragOver={(e) => {
            if (!dragId && !splitDrag(e)) return;
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            if (!dragId) setSplitOver(true);
            setDropAt(dropEnd);
          }}
          onDrop={(e) => {
            if (!dragId && !splitDrag(e)) return;
            dropHere(e, dropEnd);
          }}
        />
      </div>
      {/* Always some bare title bar to grab the window by, however many tabs there are. */}
      <div className="app-drag w-7 shrink-0" />
      <button type="button" onClick={() => agu.win.minimize().catch(() => {})} title="Minimize" aria-label="Minimize" className={winBtn}>
        <WinMinIcon className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={() => agu.win.toggleMaximize().catch(() => {})}
        title={maximized ? "Restore" : "Maximize"}
        aria-label={maximized ? "Restore" : "Maximize"}
        className={winBtn}
      >
        {maximized ? <WinRestoreIcon className="h-3.5 w-3.5" /> : <WinMaxIcon className="h-3 w-3" />}
      </button>
      <button type="button" onClick={() => agu.win.close().catch(() => {})} title="Close" aria-label="Close" className={winBtn + " hover:!bg-red-600 hover:!text-white"}>
        <XIcon className="h-3.5 w-3.5" />
      </button>
      {list && <PopupMenu anchor={list} items={listItems} onClose={() => setList(null)} />}
      {menu && <PopupMenu anchor={menu.anchor} items={menuItems(menu.tabId)} onClose={() => setMenu(null)} />}
    </div>
  );
}
