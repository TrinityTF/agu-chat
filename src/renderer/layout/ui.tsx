// Small shared widgets for the layout: modal, popup menu, buttons, toggles.
import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { XIcon } from "../icons";
import { hotkeyRecording } from "./hotkeys";

/** Centered modal. Esc and a click on the backdrop close it. */
export function Modal({
  title,
  onClose,
  children,
  width = 520,
  footer,
  fullOnSmall = false,
}: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  width?: number;
  footer?: ReactNode;
  /** Below 640px wide, fill the window under the title bar instead of floating. */
  fullOnSmall?: boolean;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !hotkeyRecording.active) {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  return createPortal(
    <div
      className={"fixed inset-0 z-[100] flex items-center justify-center bg-black/60 p-4" + (fullOnSmall ? " max-sm:p-0 max-sm:pt-7" : "")}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        role="dialog"
        aria-label={title}
        className={
          "flex max-h-full flex-col overflow-hidden rounded-lg border border-[var(--color-line)] bg-[var(--color-raised)] shadow-2xl" +
          (fullOnSmall ? " max-sm:h-full max-sm:!w-full max-sm:rounded-none max-sm:border-0" : "")
        }
        style={{ width, maxWidth: "100%" }}
      >
        <div className="flex items-center justify-between border-b border-[var(--color-line)] px-3 py-2">
          <div className="text-[13px] font-semibold">{title}</div>
          <button
            type="button"
            onClick={onClose}
            className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-raised-2)] hover:text-[var(--color-text)]"
            aria-label="Close"
          >
            <XIcon className="h-3.5 w-3.5" />
          </button>
        </div>
        <div className="min-h-0 flex-1 overflow-auto">{children}</div>
        {footer && <div className="flex items-center justify-end gap-2 border-t border-[var(--color-line)] px-3 py-2">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}

export type MenuItem =
  | { label: string; onClick: () => void; danger?: boolean; disabled?: boolean; hint?: string; icon?: ReactNode; active?: boolean }
  | { heading: string }
  | "sep";

/** A popup menu anchored at a screen rectangle (fixed positioning, so split borders never clip it). */
export function PopupMenu({
  anchor,
  items,
  onClose,
  align = "left",
}: {
  anchor: { x: number; y: number; w?: number; h?: number };
  items: MenuItem[];
  onClose: () => void;
  align?: "left" | "right";
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    let left = align === "right" ? anchor.x + (anchor.w ?? 0) - r.width : anchor.x;
    let top = anchor.y + (anchor.h ?? 0);
    left = Math.max(4, Math.min(left, window.innerWidth - r.width - 4));
    if (top + r.height > window.innerHeight - 4) top = Math.max(4, anchor.y - r.height);
    setPos({ left, top });
  }, [anchor, align]);

  useEffect(() => {
    const down = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    const key = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("mousedown", down, true);
    window.addEventListener("keydown", key, true);
    window.addEventListener("blur", onClose);
    window.addEventListener("resize", onClose);
    return () => {
      window.removeEventListener("mousedown", down, true);
      window.removeEventListener("keydown", key, true);
      window.removeEventListener("blur", onClose);
      window.removeEventListener("resize", onClose);
    };
  }, [onClose]);

  return createPortal(
    <div
      ref={ref}
      role="menu"
      className="fixed z-[90] max-h-[70vh] min-w-[170px] max-w-[calc(100vw-8px)] overflow-y-auto rounded border border-[var(--color-line)] bg-[var(--color-raised)] py-1 text-[12px] shadow-xl"
      style={{ left: pos?.left ?? -9999, top: pos?.top ?? -9999 }}
      onContextMenu={(e) => e.preventDefault()}
    >
      {items.map((it, i) =>
        it === "sep" ? (
          <div key={i} className="my-1 h-px bg-[var(--color-line)]" />
        ) : "heading" in it ? (
          <div key={i} className="truncate px-3 pb-0.5 pt-1 text-[10px] font-semibold uppercase tracking-wide text-[var(--color-faint)]">
            {it.heading}
          </div>
        ) : (
          <button
            key={i}
            type="button"
            role="menuitem"
            disabled={it.disabled}
            onClick={() => {
              onClose();
              it.onClick();
            }}
            className={
              "flex w-full items-center justify-between gap-4 px-3 py-1 text-left hover:bg-[var(--color-raised-2)] disabled:opacity-40 " +
              (it.danger ? "text-red-400" : it.active ? "font-semibold text-[var(--color-accent-soft)]" : "text-[var(--color-text)]")
            }
          >
            <span className="flex min-w-0 items-center gap-1.5">
              {it.icon}
              <span className="truncate">{it.label}</span>
            </span>
            {it.hint && <span className="text-[11px] text-[var(--color-faint)]">{it.hint}</span>}
          </button>
        ),
      )}
    </div>,
    document.body,
  );
}

/** Opens a PopupMenu below the element that was clicked. */
export function anchorOf(el: Element) {
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.top, w: r.width, h: r.height };
}

export function Btn({
  children,
  onClick,
  primary,
  danger,
  disabled,
  small,
  type = "button",
  title,
}: {
  children: ReactNode;
  onClick?: () => void;
  primary?: boolean;
  danger?: boolean;
  disabled?: boolean;
  small?: boolean;
  type?: "button" | "submit";
  title?: string;
}) {
  const tone = primary
    ? "bg-[var(--color-accent)] text-white hover:bg-[var(--color-accent-strong)] border-transparent"
    : danger
      ? "border-red-500/40 text-red-300 hover:bg-red-500/10"
      : "border-[var(--color-line)] text-[var(--color-text)] hover:bg-[var(--color-raised-2)]";
  return (
    <button
      type={type}
      title={title}
      disabled={disabled}
      onClick={onClick}
      className={`rounded border ${small ? "px-2 py-0.5 text-[11px]" : "px-3 py-1 text-[12px]"} font-medium transition-colors disabled:cursor-not-allowed disabled:opacity-40 ${tone}`}
    >
      {children}
    </button>
  );
}

export function Toggle({ checked, onChange, disabled, label }: { checked: boolean; onChange: (v: boolean) => void; disabled?: boolean; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={`relative h-4 w-7 shrink-0 rounded-full transition-colors disabled:opacity-40 ${checked ? "bg-[var(--color-accent)]" : "bg-[#2c2c36]"}`}
    >
      <span className={`absolute top-0.5 h-3 w-3 rounded-full bg-white transition-all ${checked ? "left-3.5" : "left-0.5"}`} />
    </button>
  );
}

export const inputCls =
  "w-full rounded border border-[var(--color-line)] bg-[var(--color-app)] px-2 py-1 text-[12px] text-[var(--color-text)] outline-none placeholder:text-[var(--color-faint)] focus:border-[var(--color-accent)]";

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}
