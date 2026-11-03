// A tiny per-pane toast (the site used the dashboard's global toast from store.tsx, which has no
// equivalent here). Lines stack at the top-right of the split and fade after a few seconds.
import { useCallback, useRef, useState } from "react";

export type Toast = (text: string, level?: "ok" | "warn" | string) => void;

type Item = { id: number; text: string; warn: boolean };

export function useToasts(): { toast: Toast; node: React.ReactNode } {
  const [items, setItems] = useState<Item[]>([]);
  const seq = useRef(0);
  const toast = useCallback<Toast>((text, level) => {
    const id = ++seq.current;
    setItems((prev) => [...prev.slice(-3), { id, text, warn: level === "warn" }]);
    setTimeout(() => setItems((prev) => prev.filter((i) => i.id !== id)), level === "warn" ? 6000 : 2800);
  }, []);
  const node = items.length ? (
    <div className="pointer-events-none absolute right-2 top-2 z-50 flex max-w-[80%] flex-col items-end gap-1">
      {items.map((i) => (
        <div
          key={i.id}
          className={`rounded border px-2 py-1 text-xs shadow-lg ${
            i.warn ? "border-amber-500/40 bg-raised text-amber-200" : "border-line bg-raised text-text"
          }`}
        >
          {i.text}
        </div>
      ))}
    </div>
  ) : null;
  return { toast, node };
}
