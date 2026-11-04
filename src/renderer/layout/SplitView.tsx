// Renders one tab's SplitNode tree.
//
// The tree is laid out to fractional rectangles and every leaf is drawn as a flat,
// absolutely positioned child in a stable order. That keeps each ChatPane mounted (and its
// scrollback intact) when a sibling is closed, a split is moved, or the tree reshapes.
import { useMemo, useRef, useState } from "react";
import type { ConnFrame, SplitNode } from "../protocol";
import { ChatPane, type Activity } from "../chat/ChatPane";
import { SplitHeader, type SourceDrag, type SplitActions } from "./SplitHeader";
import { Btn } from "./ui";
import { sourceKey, type Edge, type Leaf } from "./tree";

type Rect = { x: number; y: number; w: number; h: number };
type Divider = { key: string; containerId: string; index: number; axis: "row" | "col"; rect: Rect; container: Rect; sizes: number[] };

const MIN_W = 140;
const MIN_H = 70;

function computeLayout(root: SplitNode | null) {
  const leaves: { leaf: Leaf; rect: Rect }[] = [];
  const dividers: Divider[] = [];
  const walk = (n: SplitNode, r: Rect) => {
    if (n.kind === "leaf") {
      leaves.push({ leaf: n, rect: r });
      return;
    }
    let c = n.kind === "row" ? r.x : r.y;
    n.children.forEach((child, i) => {
      const f = n.sizes[i] ?? 1 / n.children.length;
      const cr: Rect = n.kind === "row" ? { x: c, y: r.y, w: r.w * f, h: r.h } : { x: r.x, y: c, w: r.w, h: r.h * f };
      walk(child, cr);
      c += n.kind === "row" ? cr.w : cr.h;
      if (i < n.children.length - 1) {
        const line: Rect = n.kind === "row" ? { x: c, y: r.y, w: 0, h: r.h } : { x: r.x, y: c, w: r.w, h: 0 };
        dividers.push({ key: `${n.id}:${i}`, containerId: n.id, index: i, axis: n.kind, rect: line, container: r, sizes: n.sizes });
      }
    });
  };
  if (root) walk(root, { x: 0, y: 0, w: 1, h: 1 });
  return { leaves, dividers };
}

const pct = (v: number) => `${(v * 100).toFixed(4)}%`;

/** A whole split being moved (its header), some of a split's channels (a header chip), or another tab. */
type Dragging = { kind: "split"; leafId: string } | ({ kind: "sources" } & SourceDrag) | { kind: "tab"; tabId: string };

export function SplitView({
  root,
  focusedId,
  tabActive,
  actions,
  onActivity,
  tabDrag = null,
}: {
  root: SplitNode | null;
  focusedId: string | null;
  tabActive: boolean;
  actions: SplitActions;
  /** Live chat arrived in one of this tab's splits (drives the tab's unread / highlight marker). */
  onActivity?: (kind: Activity) => void;
  /** Another tab being dragged from the tab bar: its splits can be dropped beside these. */
  tabDrag?: string | null;
}) {
  const hostRef = useRef<HTMLDivElement>(null);
  const [ownDrag, setDragging] = useState<Dragging | null>(null);
  const dragging: Dragging | null = ownDrag ?? (tabDrag ? { kind: "tab", tabId: tabDrag } : null);
  const { leaves, dividers } = useMemo(() => computeLayout(root), [root]);
  // Stable DOM order, so React never has to move a pane's element (moving resets its scroll offset).
  const ordered = useMemo(() => leaves.slice().sort((a, b) => (a.leaf.id < b.leaf.id ? -1 : 1)), [leaves]);

  if (!root) {
    return (
      <div
        className="relative flex h-full w-full items-center justify-center gap-2 bg-[var(--color-panel)]"
        onDragOver={(e) => {
          if (!tabDrag) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
        }}
        onDrop={(e) => {
          if (!tabDrag) return;
          e.preventDefault();
          actions.takeTab(tabDrag, null, "right");
        }}
      >
        <Btn primary onClick={actions.addFirst}>
          Add a channel
        </Btn>
        <Btn onClick={actions.openSettings}>Settings</Btn>
        {tabDrag && <div className="pointer-events-none absolute inset-2 rounded-sm border-2 border-dashed border-[var(--color-accent)]" />}
      </div>
    );
  }

  const startResize = (e: React.PointerEvent<HTMLDivElement>, d: Divider) => {
    const host = hostRef.current;
    if (!host) return;
    e.preventDefault();
    const handle = e.currentTarget;
    handle.setPointerCapture(e.pointerId);
    const hr = host.getBoundingClientRect();
    const horizontal = d.axis === "row";
    const px = (horizontal ? hr.width * d.container.w : hr.height * d.container.h) || 1;
    const start = horizontal ? e.clientX : e.clientY;
    const base = d.sizes.slice();
    const pair = base[d.index] + base[d.index + 1];
    const minFrac = Math.min((horizontal ? MIN_W : MIN_H) / px, pair / 2);
    const move = (ev: PointerEvent) => {
      const delta = ((horizontal ? ev.clientX : ev.clientY) - start) / px;
      const a = Math.max(minFrac, Math.min(pair - minFrac, base[d.index] + delta));
      const next = base.slice();
      next[d.index] = a;
      next[d.index + 1] = pair - a;
      actions.resize(d.containerId, next);
    };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
      document.body.style.cursor = "";
    };
    document.body.style.cursor = horizontal ? "col-resize" : "row-resize";
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  };

  const multi = leaves.length > 1;
  // The split whose header sits under the window's top-right corner gets the Settings button.
  const corner = leaves.filter((l) => l.rect.y < 1e-6).reduce((a, b) => (b.rect.x + b.rect.w > a.rect.x + a.rect.w ? b : a), leaves[0])?.leaf.id;

  return (
    <div ref={hostRef} className="relative h-full w-full overflow-hidden bg-[var(--color-panel)]">
      {ordered.map(({ leaf }) => {
        const r = leaves.find((l) => l.leaf.id === leaf.id)!.rect;
        return (
          <div
            key={leaf.id}
            className="absolute flex min-h-0 min-w-0 flex-col"
            style={{ left: pct(r.x), top: pct(r.y), width: pct(r.w), height: pct(r.h) }}
          >
            <LeafView
              leaf={leaf}
              focused={focusedId === leaf.id}
              outline={multi && focusedId === leaf.id}
              tabActive={tabActive}
              actions={actions}
              dragging={dragging}
              setDragging={setDragging}
              onActivity={onActivity}
              settingsButton={leaf.id === corner}
            />
          </div>
        );
      })}

      {dividers.map((d) => {
        const horizontal = d.axis === "row";
        return (
          <div
            key={d.key}
            data-divider={d.key}
            className="pointer-events-none absolute z-20 bg-[var(--color-line)]"
            style={
              horizontal
                ? { left: `calc(${pct(d.rect.x)} - 0.5px)`, top: pct(d.rect.y), width: 1, height: pct(d.rect.h) }
                : { top: `calc(${pct(d.rect.y)} - 0.5px)`, left: pct(d.rect.x), height: 1, width: pct(d.rect.w) }
            }
          >
            <div
              className="pointer-events-auto absolute hover:bg-[var(--color-accent)]/40"
              style={
                horizontal
                  ? { left: -3, width: 7, top: 0, bottom: 0, cursor: "col-resize" }
                  : { top: -3, height: 7, left: 0, right: 0, cursor: "row-resize" }
              }
              onPointerDown={(e) => startResize(e, d)}
              onDoubleClick={() => {
                const next = d.sizes.slice();
                const avg = (next[d.index] + next[d.index + 1]) / 2;
                next[d.index] = avg;
                next[d.index + 1] = avg;
                actions.resize(d.containerId, next);
              }}
              title="Drag to resize, double-click to even out"
            />
          </div>
        );
      })}
    </div>
  );
}

/** Where a drop lands: an edge makes a new split there, the middle merges into this split. */
type Zone = Edge | "center";

function zoneAt(e: React.DragEvent, el: HTMLElement, canMerge: boolean): Zone {
  const r = el.getBoundingClientRect();
  const x = (e.clientX - r.left) / r.width - 0.5;
  const y = (e.clientY - r.top) / r.height - 0.5;
  if (canMerge && Math.abs(x) < 0.25 && Math.abs(y) < 0.25) return "center";
  if (Math.abs(x) > Math.abs(y)) return x < 0 ? "left" : "right";
  return y < 0 ? "top" : "bottom";
}

const HIGHLIGHT: Record<Zone, React.CSSProperties> = {
  left: { left: 0, top: 0, width: "50%", height: "100%" },
  right: { right: 0, top: 0, width: "50%", height: "100%" },
  top: { left: 0, top: 0, width: "100%", height: "50%" },
  bottom: { left: 0, bottom: 0, width: "100%", height: "50%" },
  center: { left: "25%", top: "25%", width: "50%", height: "50%" },
};

function LeafView({
  leaf,
  focused,
  outline,
  tabActive,
  actions,
  dragging,
  setDragging,
  onActivity,
  settingsButton,
}: {
  leaf: Leaf;
  focused: boolean;
  outline: boolean;
  tabActive: boolean;
  actions: SplitActions;
  dragging: Dragging | null;
  setDragging: (d: Dragging | null) => void;
  onActivity?: (kind: Activity) => void;
  settingsButton: boolean;
}) {
  const [status, setStatus] = useState<Record<string, ConnFrame>>({});
  const [hover, setHover] = useState<Zone | null>(null);
  const sig = leaf.sources.map(sourceKey).join("|");
  // A split can't be dropped on itself; its channels can (beside it), unless they are all it has.
  const dropTarget =
    !!dragging &&
    (dragging.kind === "tab" ||
      (dragging.kind === "split" ? dragging.leafId !== leaf.id : dragging.leafId !== leaf.id || dragging.keys.length < leaf.sources.length));
  // Merging only means something into another split; a whole tab only goes beside one.
  const canMerge = !!dragging && dragging.kind !== "tab" && dragging.leafId !== leaf.id;

  return (
    <div
      className="relative flex min-h-0 min-w-0 flex-1 flex-col bg-[var(--color-panel)]"
      onMouseDownCapture={() => actions.focus(leaf.id)}
      data-leaf={leaf.id}
    >
      <SplitHeader
        leaf={leaf}
        status={status}
        focused={focused}
        actions={actions}
        onDragStart={() => setDragging({ kind: "split", leafId: leaf.id })}
        onDragSources={(d) => setDragging({ kind: "sources", ...d })}
        onDragEnd={() => setDragging(null)}
        settingsButton={settingsButton}
      />
      <div className="relative min-h-0 flex-1 overflow-hidden">
        <ChatPane key={sig} sources={leaf.sources} modMode={leaf.modMode} focused={focused && tabActive} onStatus={setStatus} onActivity={onActivity} />
      </div>

      {dragging && dropTarget && (
        <div
          className="absolute inset-0 z-30"
          onDragOver={(e) => {
            e.preventDefault();
            e.dataTransfer.dropEffect = "move";
            setHover(zoneAt(e, e.currentTarget, canMerge));
          }}
          onDragLeave={() => setHover(null)}
          onDrop={(e) => {
            e.preventDefault();
            const zone = zoneAt(e, e.currentTarget, canMerge);
            setHover(null);
            setDragging(null);
            if (dragging.kind === "tab") {
              if (zone !== "center") actions.takeTab(dragging.tabId, leaf.id, zone);
            } else if (zone === "center") actions.merge(dragging.leafId, dragging.kind === "split" ? null : dragging.keys, leaf.id);
            else if (dragging.kind === "split") actions.move(dragging.leafId, leaf.id, zone);
            else actions.splitOut(dragging.leafId, dragging.keys, leaf.id, zone);
          }}
        >
          {hover && (
            <div
              className="pointer-events-none absolute flex items-center justify-center rounded-sm border-2 border-[var(--color-accent)] bg-[var(--color-accent)]/25"
              style={HIGHLIGHT[hover]}
              data-drop-edge={hover}
            >
              {hover === "center" && <span className="rounded bg-[var(--color-accent)] px-2 py-0.5 text-[11px] font-semibold text-white">Merge into this split</span>}
            </div>
          )}
        </div>
      )}

      {outline && <div className="pointer-events-none absolute inset-0 z-10 border border-[var(--color-accent)]" />}
    </div>
  );
}
