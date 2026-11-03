// The notebook's tab bar, which is also the window's title bar (the window has no OS frame): select, add,
// close (x / middle-click), rename (double-click), drag to reorder, a list of every tab for when they overflow,
// always-on-top, settings and the window buttons. Empty space drags the window; double-click maximizes.
import { useEffect, useRef, useState } from "react";
import type { Tab } from "../protocol";
import { agu } from "../bridge";
import { ChevronIcon, GearIcon, PinIcon, WinMaxIcon, WinMinIcon, WinRestoreIcon, XIcon } from "../icons";
import { PopupMenu, anchorOf, type MenuItem } from "./ui";
import { autoTitle, tabTitle } from "./tree";
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
  openSettings(): void;
  togglePin(): void;
};

export function NotebookTabs({
  tabs,
  active,
  actions,
  unread = {},
  pinned = false,
}: {
  tabs: Tab[];
  active: string;
  actions: TabActions;
  /** Unselected tabs that got live chat: "message" = new messages, "highlight" = a highlight or mention. */
  unread?: Record<string, Activity>;
  /** The window stays above other apps. */
  pinned?: boolean;
}) {
  const closeTabKey = useHotkeyHint("closeTab");
  const newTabKey = useHotkeyHint("newTab");
  const settingsKey = useHotkeyHint("openSettings");
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [dragId, setDragId] = useState<string | null>(null);
  const [dropAt, setDropAt] = useState<number | null>(null); // insertion index while dragging
  const [menu, setMenu] = useState<{ anchor: { x: number; y: number }; tabId: string } | null>(null);
  const stripRef = useRef<HTMLDivElement>(null);
  const [overflow, setOverflow] = useState(false);
  const [list, setList] = useState<ReturnType<typeof anchorOf> | null>(null);
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const el = stripRef.current;
    if (!el) return;
    const check = () => setOverflow(el.scrollWidth > el.clientWidth + 1);
    check();
    const ro = new ResizeObserver(check);
    ro.observe(el);
    return () => ro.disconnect();
  }, [tabs]);

  useEffect(() => {
    agu.win
      .isMaximized()
      .then(setMaximized)
      .catch(() => {});
    return agu.win.onMaximized(setMaximized);
  }, []);

  useEffect(() => {
    const el = stripRef.current?.querySelector<HTMLElement>(`[data-tab-id="${active}"]`);
    el?.scrollIntoView({ inline: "nearest", block: "nearest" });
  }, [active, tabs.length]);

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
      { label: "Close tab", hint: closeTabKey, onClick: () => actions.close(id) },
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

  return (
    <div className="flex h-7 shrink-0 select-none items-stretch border-b border-[var(--color-line)] bg-[var(--color-raised)]" role="tablist">
      <div
        ref={stripRef}
        className="no-scrollbar flex min-w-0 items-stretch overflow-x-auto"
        onWheel={(e) => {
          if (e.deltaY && stripRef.current) stripRef.current.scrollLeft += e.deltaY;
        }}
        onDragOver={(e) => {
          // Dropping on the empty strip area sends the tab to the end.
          if (dragId && e.target === e.currentTarget) {
            e.preventDefault();
            setDropAt(tabs.length);
          }
        }}
        onDrop={(e) => {
          if (dragId && e.target === e.currentTarget) {
            e.preventDefault();
            actions.reorder(dragId, tabs.length);
            setDragId(null);
            setDropAt(null);
          }
        }}
      >
        {tabs.map((t, i) => {
          const selected = t.id === active;
          const isEditing = editing === t.id;
          const mark = selected ? undefined : unread[t.id];
          return (
            <div
              key={t.id}
              role="tab"
              aria-selected={selected}
              data-tab-id={t.id}
              draggable={!isEditing}
              onClick={() => actions.select(t.id)}
              onMouseDown={(e) => {
                if (e.button === 1) e.preventDefault();
              }}
              onAuxClick={(e) => {
                if (e.button === 1) {
                  e.preventDefault();
                  actions.close(t.id);
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
              }}
              onDragEnd={() => {
                setDragId(null);
                setDropAt(null);
              }}
              onDragOver={(e) => {
                if (!dragId) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                const r = e.currentTarget.getBoundingClientRect();
                setDropAt(e.clientX < r.left + r.width / 2 ? i : i + 1);
              }}
              onDrop={(e) => {
                if (!dragId) return;
                e.preventDefault();
                const r = e.currentTarget.getBoundingClientRect();
                actions.reorder(dragId, e.clientX < r.left + r.width / 2 ? i : i + 1);
                setDragId(null);
                setDropAt(null);
              }}
              className={
                "group relative flex max-w-[220px] shrink-0 cursor-default items-center gap-1 border-r border-[var(--color-line)] px-2.5 text-[12px] " +
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
              {!isEditing && (
                <button
                  type="button"
                  aria-label="Close tab"
                  title="Close tab"
                  onClick={(e) => {
                    e.stopPropagation();
                    actions.close(t.id);
                  }}
                  onMouseDown={(e) => e.stopPropagation()}
                  className={
                    "flex h-4 w-4 shrink-0 items-center justify-center rounded text-[var(--color-faint)] hover:bg-[var(--color-line)] hover:text-[var(--color-text)] " +
                    (selected ? "" : "invisible group-hover:visible")
                  }
                >
                  <XIcon className="h-3 w-3" />
                </button>
              )}
              {selected && <span className="absolute inset-x-0 bottom-0 h-[2px] bg-[var(--color-accent)]" />}
              {mark === "highlight" && <span className="absolute inset-x-0 top-0 h-[2px] bg-[var(--color-accent-soft)]" />}
              {dragId && dropAt === i && <span className="absolute inset-y-0 left-0 w-[2px] bg-[var(--color-accent)]" />}
              {dragId && dropAt === i + 1 && i === tabs.length - 1 && <span className="absolute inset-y-0 right-0 w-[2px] bg-[var(--color-accent)]" />}
            </div>
          );
        })}
      </div>
      <button
        type="button"
        onClick={actions.add}
        title={newTabKey ? `New tab (${newTabKey})` : "New tab"}
        aria-label="New tab"
        className="flex w-7 shrink-0 items-center justify-center text-[16px] leading-none text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
      >
        +
      </button>
      {overflow && (
        <button
          type="button"
          onClick={(e) => {
            const el = e.currentTarget;
            setList((open) => (open ? null : anchorOf(el)));
          }}
          title="All tabs"
          aria-label="All tabs"
          className="flex w-6 shrink-0 items-center justify-center text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
        >
          <ChevronIcon className="h-3.5 w-3.5" />
        </button>
      )}
      <div className="app-drag min-w-[28px] flex-1" />
      <button
        type="button"
        onClick={actions.togglePin}
        aria-pressed={pinned}
        title={pinned ? "Always on top: on" : "Always on top: off"}
        aria-label="Always on top"
        className={
          "flex w-7 shrink-0 items-center justify-center hover:bg-[var(--color-raised-2)] " +
          (pinned ? "text-[var(--color-accent-soft)]" : "text-[var(--color-faint)] hover:text-[var(--color-text)]")
        }
      >
        <PinIcon className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={(e) => {
          e.currentTarget.blur();
          actions.openSettings();
        }}
        title={settingsKey ? `Settings (${settingsKey})` : "Settings"}
        aria-label="Settings"
        className="flex w-8 shrink-0 items-center justify-center border-l border-[var(--color-line)] text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
      >
        <GearIcon className="h-4 w-4" />
      </button>
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
      {list && <PopupMenu anchor={list} items={listItems} align="right" onClose={() => setList(null)} />}
      {menu && <PopupMenu anchor={menu.anchor} items={menuItems(menu.tabId)} onClose={() => setMenu(null)} />}
    </div>
  );
}
