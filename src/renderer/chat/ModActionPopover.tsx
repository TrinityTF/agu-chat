// Ported from modtools.agu.lol web/src/app/pages/Chat.tsx (commit 1b826c2): ModActionPopover, the
// lightweight confirm anchored at a row's mod button (ban / timeout / warn / delete). Changes: restyled on
// the theme tokens; recent lines come from this split's rows; Escape / click-outside closes.
import { useEffect, useRef, useState } from "react";
import type { SourceRef } from "../protocol";
import { fetchChatRules, TIMEOUT_LADDER } from "./modCaps";
import { twitchAction } from "./modActions";
import type { Toast } from "./toast";
import type { ChatMsg, Row } from "./types";
import { btnDanger, btnGhost, inputCls } from "./ui";

export type ModPopoverState = { sk: string; msg: ChatMsg; kind: "ban" | "timeout" | "warn" | "delete"; left: number; top: number; above: boolean };

export function ModActionPopover({
  pop,
  source,
  rows,
  toast,
  onClose,
  onDeleted,
}: {
  pop: ModPopoverState;
  source: SourceRef;
  rows: Row[];
  toast: Toast;
  onClose: () => void;
  onDeleted: (id: string) => void;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState("");
  const [rules, setRules] = useState<string[]>([]);
  const [selectedRules, setSelectedRules] = useState<Set<string>>(new Set());

  useEffect(() => {
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  useEffect(() => {
    if (pop.kind !== "warn") return;
    let dead = false;
    fetchChatRules(source).then((r) => {
      if (!dead) setRules(r);
    });
    return () => {
      dead = true;
    };
  }, [pop.kind, source]);

  const recent = rows.filter((r): r is Extract<Row, { k: "msg" }> => r.k === "msg" && r.sk === pop.sk && r.m.user.id === pop.msg.user.id).slice(-5);

  const run = async (body: Record<string, unknown>, successMsg: string) => {
    setBusy(true);
    try {
      if (await twitchAction(source, body, successMsg, toast)) {
        if (body.action === "delete") onDeleted(pop.msg.id);
        onClose();
      }
    } finally {
      setBusy(false);
    }
  };

  const title =
    pop.kind === "ban" ? `Ban ${pop.msg.user.name}?` : pop.kind === "timeout" ? `Timeout ${pop.msg.user.name}` : pop.kind === "warn" ? `Warn ${pop.msg.user.name}` : "Delete this message?";

  return (
    <div
      ref={ref}
      className="absolute z-40 w-64 space-y-2 rounded-md border border-line bg-raised p-2.5 text-xs shadow-2xl shadow-black/60"
      style={{ left: pop.left, top: pop.top, transform: pop.above ? "translateY(-100%)" : undefined }}
    >
      <div className="font-semibold text-text">{title}</div>

      {(pop.kind === "ban" || pop.kind === "timeout") && recent.length > 0 && (
        <div className="max-h-20 space-y-0.5 overflow-y-auto text-faint">
          {recent.map((r) => (
            <div key={r.key} className="truncate">
              {r.m.text}
            </div>
          ))}
        </div>
      )}

      {pop.kind === "timeout" && (
        <div className="flex flex-wrap gap-1">
          {TIMEOUT_LADDER.map((t) => (
            <button
              key={t.sec}
              type="button"
              disabled={busy}
              onClick={() => run({ action: "timeout", userId: pop.msg.user.id, duration: t.sec }, `timed out ${t.label}`)}
              className="rounded border border-line px-1.5 py-0.5 text-[11px] text-muted hover:border-amber-500/50 hover:text-amber-300 disabled:opacity-40"
            >
              {t.label}
            </button>
          ))}
        </div>
      )}

      {pop.kind === "warn" && (
        <div className="space-y-2">
          {rules.length > 0 && (
            <div className="max-h-24 space-y-1 overflow-y-auto">
              {rules.map((rule) => (
                <label key={rule} className="flex items-start gap-1.5 text-muted">
                  <input
                    type="checkbox"
                    className="mt-0.5"
                    checked={selectedRules.has(rule)}
                    onChange={(e) =>
                      setSelectedRules((prev) => {
                        const next = new Set(prev);
                        if (e.target.checked) next.add(rule);
                        else next.delete(rule);
                        return next;
                      })
                    }
                  />
                  <span>{rule}</span>
                </label>
              ))}
            </div>
          )}
          <input className={`${inputCls} w-full`} placeholder="Custom reason" value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} />
        </div>
      )}

      {pop.kind === "ban" && <input autoFocus className={`${inputCls} w-full`} placeholder="Reason (optional)" value={reason} onChange={(e) => setReason(e.target.value)} maxLength={500} />}

      <div className="flex gap-1.5">
        {pop.kind !== "timeout" && (
          <button
            type="button"
            className={btnDanger}
            disabled={busy || (pop.kind === "warn" && selectedRules.size === 0 && !reason.trim())}
            onClick={() => {
              if (pop.kind === "ban") run({ action: "ban", userId: pop.msg.user.id, reason: reason.trim() || undefined }, "banned");
              else if (pop.kind === "warn") {
                const reasonText = [...Array.from(selectedRules), reason.trim()].filter(Boolean).join("; ").slice(0, 500);
                run({ action: "warn", userId: pop.msg.user.id, reason: reasonText }, "warned");
              } else if (pop.kind === "delete") run({ action: "delete", messageId: pop.msg.id }, "deleted");
            }}
          >
            Confirm
          </button>
        )}
        <button type="button" className={btnGhost} onClick={onClose}>
          Cancel
        </button>
      </div>
    </div>
  );
}
