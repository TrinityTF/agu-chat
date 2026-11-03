// The ~26px header on every split: platforms + channel names, connection dots,
// live pill + stream title, mod-mode toggle and the "⋯" menu.
// A merged split shows one chip per platform (icon, "2x" when it holds several channels of it, a status dot);
// names appear when there is room. Clicking a chip splits its channels out; dragging one drops it on any split edge.
import { useState } from "react";
import type { ConnFrame, Platform } from "../protocol";
import { PLATFORM_LABEL, PlatformIcon } from "../platformIcons";
import { DotsIcon, SwordIcon } from "../icons";
import { PopupMenu, anchorOf, type MenuItem } from "./ui";
import { shortName, sourceKey, type Leaf } from "./tree";
import { useHotkeyHint } from "./hotkeyContext";

export type SplitActions = {
  focus(leafId: string): void;
  toggleMod(leafId: string): void;
  change(leafId: string): void;
  split(leafId: string, dir: "right" | "below"): void;
  close(leafId: string): void;
  resize(containerId: string, sizes: number[]): void;
  move(srcId: string, targetId: string, edge: "left" | "right" | "top" | "bottom"): void;
  /** Take these sources (sourceKey) out of `leafId` into a new split at `edge` of `targetId`. */
  splitOut(leafId: string, keys: string[], targetId: string, edge: "left" | "right" | "top" | "bottom"): void;
  removeSource(leafId: string, key: string): void;
  addFirst(): void;
};

/** What a header chip carries while dragged (dataTransfer "application/x-agu-sources"). */
export type SourceDrag = { leafId: string; keys: string[] };

// The dot a chip shows for several channels: the one most worth noticing.
const WORST: ConnFrame["state"][] = ["error", "unavailable", "connecting", "offline", "open"];

const DOT: Record<ConnFrame["state"], { cls: string; label: string }> = {
  connecting: { cls: "bg-amber-400 animate-pulse", label: "Connecting" },
  open: { cls: "bg-emerald-500", label: "Connected" },
  offline: { cls: "bg-zinc-500", label: "Offline" },
  error: { cls: "bg-red-500", label: "Error" },
  unavailable: { cls: "bg-orange-500", label: "Unavailable" },
};

export function SplitHeader({
  leaf,
  status,
  focused,
  actions,
  onDragStart,
  onDragSources,
  onDragEnd,
}: {
  leaf: Leaf;
  status: Record<string, ConnFrame>;
  focused: boolean;
  actions: SplitActions;
  onDragStart: () => void;
  onDragSources: (d: SourceDrag) => void;
  onDragEnd: () => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
  const [chipMenu, setChipMenu] = useState<{ anchor: { x: number; y: number; w: number; h: number }; platform: Platform } | null>(null);
  const closeKey = useHotkeyHint("closeSplit");

  const rows = leaf.sources.map((s) => ({ s, st: status[sourceKey(s)] as ConnFrame | undefined }));
  const live = rows.find((r) => r.st?.live);
  const title = (live?.st?.title || rows.find((r) => r.st?.title)?.st?.title || "").trim();

  const merged = rows.length > 1;
  const groups: { platform: Platform; rows: typeof rows }[] = [];
  for (const r of rows) {
    const g = groups.find((x) => x.platform === r.s.platform);
    if (g) g.rows.push(r);
    else groups.push({ platform: r.s.platform, rows: [r] });
  }
  const dotTip = (r: (typeof rows)[number]) =>
    `${PLATFORM_LABEL[r.s.platform]} ${shortName(r.s)}: ${DOT[r.st?.state ?? "connecting"].label}${r.st?.detail ? " - " + r.st.detail : ""}`;

  const chipItems = (platform: Platform): MenuItem[] => {
    const g = groups.find((x) => x.platform === platform);
    if (!g) return [];
    const out: MenuItem[] = [];
    for (const { s } of g.rows) {
      const key = sourceKey(s);
      out.push(
        { heading: `${PLATFORM_LABEL[s.platform]}: ${shortName(s)}` },
        { label: "Split out to the right", onClick: () => actions.splitOut(leaf.id, [key], leaf.id, "right") },
        { label: "Split out below", onClick: () => actions.splitOut(leaf.id, [key], leaf.id, "bottom") },
        { label: "Remove from this split", danger: true, onClick: () => actions.removeSource(leaf.id, key) },
      );
    }
    if (g.rows.length > 1) {
      const keys = g.rows.map((r) => sourceKey(r.s));
      out.push("sep", {
        label: `Split all ${g.rows.length} ${PLATFORM_LABEL[platform]} out to the right`,
        onClick: () => actions.splitOut(leaf.id, keys, leaf.id, "right"),
      });
    }
    return out;
  };

  const items: MenuItem[] = [
    { label: "Change channel(s)…", onClick: () => actions.change(leaf.id) },
    { label: "Add split right", onClick: () => actions.split(leaf.id, "right") },
    { label: "Add split below", onClick: () => actions.split(leaf.id, "below") },
    "sep",
    { label: "Close split", hint: closeKey, danger: true, onClick: () => actions.close(leaf.id) },
  ];

  return (
    <div
      draggable
      onDragStart={(e) => {
        e.dataTransfer.setData("application/x-agu-split", leaf.id);
        e.dataTransfer.effectAllowed = "move";
        // Changing the DOM inside dragstart can cancel the drag, so show the drop targets a tick later.
        setTimeout(onDragStart, 0);
      }}
      onDragEnd={onDragEnd}
      className={
        "@container flex h-[26px] shrink-0 select-none items-center gap-2 border-b border-[var(--color-line)] bg-[var(--color-raised)] pl-2 pr-1 text-[12px] " +
        (focused ? "text-[var(--color-text)]" : "text-[var(--color-muted)]")
      }
      data-split-header={leaf.id}
    >
      {merged ? (
        <div className="flex min-w-0 shrink items-center gap-1.5 overflow-hidden">
          {groups.map((g) => {
            const states = g.rows.map((r) => r.st?.state ?? "connecting");
            const worst = WORST.find((w) => states.includes(w)) ?? "connecting";
            const keys = g.rows.map((r) => sourceKey(r.s));
            return (
              <button
                key={g.platform}
                type="button"
                draggable
                onDragStart={(e) => {
                  e.stopPropagation(); // not the whole-split drag
                  const d: SourceDrag = { leafId: leaf.id, keys };
                  e.dataTransfer.setData("application/x-agu-sources", JSON.stringify(d));
                  e.dataTransfer.effectAllowed = "move";
                  setTimeout(() => onDragSources(d), 0);
                }}
                onDragEnd={(e) => {
                  e.stopPropagation();
                  onDragEnd();
                }}
                onClick={(e) => {
                  const r = e.currentTarget.getBoundingClientRect();
                  setChipMenu({ anchor: { x: r.left, y: r.top, w: r.width, h: r.height }, platform: g.platform });
                }}
                title={[...g.rows.map(dotTip), "Click to split out, or drag onto a split edge"].join("\n")}
                data-chip={g.platform}
                className="flex min-w-0 shrink-0 items-center gap-1 rounded px-1 py-0.5 hover:bg-[var(--color-raised-2)]"
              >
                <span className="relative shrink-0">
                  <PlatformIcon platform={g.platform} className="h-3.5 w-3.5" />
                  <span data-conn={worst} className={`absolute -right-0.5 -top-0.5 h-1.5 w-1.5 rounded-full ring-1 ring-[var(--color-raised)] ${DOT[worst].cls}`} />
                  {g.rows.length > 1 && (
                    <span className="absolute -bottom-1 -right-2 rounded-sm bg-[var(--color-raised-2)] px-[2px] text-[8px] font-bold leading-[10px] text-[var(--color-text)]">
                      {g.rows.length}x
                    </span>
                  )}
                </span>
                <span className={"hidden max-w-[110px] truncate font-medium @2xl:inline " + (g.rows.length > 1 ? "pl-1.5" : "")}>
                  {g.rows.map((r) => shortName(r.s)).join(", ")}
                </span>
              </button>
            );
          })}
        </div>
      ) : (
        rows.map((r) => (
          <span key={sourceKey(r.s)} className="flex min-w-0 shrink items-center gap-1.5" title={dotTip(r)}>
            <PlatformIcon platform={r.s.platform} className="h-3.5 w-3.5 shrink-0" />
            <span className="truncate font-medium">{shortName(r.s)}</span>
            <span data-conn={r.st?.state ?? "connecting"} className={`h-2 w-2 shrink-0 rounded-full ${DOT[r.st?.state ?? "connecting"].cls}`} />
          </span>
        ))
      )}

      {live && <span className="shrink-0 rounded-sm bg-red-600 px-1 text-[9px] font-bold leading-[14px] tracking-wide text-white">LIVE</span>}
      {title ? (
        <span className="min-w-0 flex-1 truncate text-[11px] text-[var(--color-faint)]" title={title}>
          {title}
        </span>
      ) : (
        <span className="flex-1" />
      )}

      <button
        type="button"
        onClick={() => actions.toggleMod(leaf.id)}
        aria-pressed={leaf.modMode}
        title={leaf.modMode ? "Mod mode on" : "Mod mode off"}
        className={
          "flex h-5 w-5 shrink-0 items-center justify-center rounded " +
          (leaf.modMode
            ? "bg-[var(--color-accent)] text-white"
            : "text-[var(--color-faint)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]")
        }
      >
        <SwordIcon className="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        onClick={(e) => setMenu(anchorOf(e.currentTarget))}
        title="Split menu"
        aria-label="Split menu"
        className="flex h-5 w-5 shrink-0 items-center justify-center rounded text-[var(--color-faint)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
      >
        <DotsIcon className="h-3.5 w-3.5 rotate-90" />
      </button>
      {chipMenu && <PopupMenu anchor={chipMenu.anchor} items={chipItems(chipMenu.platform)} onClose={() => setChipMenu(null)} />}
      {menu && <PopupMenu anchor={menu} items={items} align="right" onClose={() => setMenu(null)} />}
    </div>
  );
}
