// The ~26px header on every split: platforms + channel names, connection dots,
// live pill + stream title, mod-mode toggle and the "⋯" menu.
import { useState } from "react";
import type { ConnFrame } from "../protocol";
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
  addFirst(): void;
};

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
  onDragEnd,
}: {
  leaf: Leaf;
  status: Record<string, ConnFrame>;
  focused: boolean;
  actions: SplitActions;
  onDragStart: () => void;
  onDragEnd: () => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number; w: number; h: number } | null>(null);
  const closeKey = useHotkeyHint("closeSplit");

  const rows = leaf.sources.map((s) => ({ s, st: status[sourceKey(s)] as ConnFrame | undefined }));
  const live = rows.find((r) => r.st?.live);
  const title = (live?.st?.title || rows.find((r) => r.st?.title)?.st?.title || "").trim();

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
        "flex h-[26px] shrink-0 select-none items-center gap-2 border-b border-[var(--color-line)] bg-[var(--color-raised)] pl-2 pr-1 text-[12px] " +
        (focused ? "text-[var(--color-text)]" : "text-[var(--color-muted)]")
      }
      data-split-header={leaf.id}
    >
      <div className="flex min-w-0 shrink items-center gap-2 overflow-hidden">
        {rows.map(({ s }) => (
          <span key={sourceKey(s)} className="flex min-w-0 items-center gap-1" title={`${PLATFORM_LABEL[s.platform]}: ${s.channel}`}>
            <PlatformIcon platform={s.platform} className="h-3.5 w-3.5 shrink-0" />
            <span className="truncate font-medium">{shortName(s)}</span>
          </span>
        ))}
      </div>

      <div className="flex shrink-0 items-center gap-1">
        {rows.map(({ s, st }) => {
          const d = DOT[st?.state ?? "connecting"];
          const tip = `${PLATFORM_LABEL[s.platform]} ${shortName(s)}: ${d.label}${st?.detail ? " - " + st.detail : ""}`;
          return <span key={sourceKey(s)} title={tip} data-conn={st?.state ?? "connecting"} className={`h-2 w-2 rounded-full ${d.cls}`} />;
        })}
      </div>

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
      {menu && <PopupMenu anchor={menu} items={items} align="right" onClose={() => setMenu(null)} />}
    </div>
  );
}
