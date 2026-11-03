// Ported from modtools.agu.lol web/src/app/emoteTip.tsx (commit 1b826c2). Changes: a module-level store
// instead of a React context, so rows (memoized) can call show/hide without a context value that changes
// with every hover, and one <EmoteTipLayer/> per pane renders it.
import { useSyncExternalStore } from "react";

export type EmoteTipContent = { img: string; name: string; label: string };
export type EmoteTipInfo = EmoteTipContent & { x: number; y: number };

let info: EmoteTipInfo | null = null;
const listeners = new Set<() => void>();
const emit = () => listeners.forEach((l) => l());

export const emoteTip = {
  show(x: number, y: number, content: EmoteTipContent) {
    info = { x, y, ...content };
    emit();
  },
  hide() {
    if (!info) return;
    info = null;
    emit();
  },
};

// Rendered once per pane, positioned from the cursor. `pointer-events-none` matters: without it the
// tooltip sits under the pointer and the emote it describes immediately fires mouseleave, flickering.
export function EmoteTipLayer() {
  const cur = useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => info,
  );
  if (!cur) return null;
  const left = Math.min(cur.x + 14, window.innerWidth - 140);
  const top = Math.min(cur.y + 14, window.innerHeight - 130);
  return (
    <div
      className="pointer-events-none fixed z-[60] flex flex-col items-center gap-1 rounded-md border border-line bg-raised p-2 text-xs shadow-2xl"
      style={{ left, top }}
    >
      {cur.img && <img src={cur.img} alt="" decoding="async" className="h-16 w-16 object-contain" />}
      <div className="font-semibold text-text">{cur.name}</div>
      <div className="text-faint">{cur.label}</div>
    </div>
  );
}
