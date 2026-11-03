// Draws its children into a separate OS window, so a user card can be dragged anywhere on screen, outside the app.
// main.js lets about:blank windows named "agu-popout:<id>" open (frameless, owned by the main window). The window
// runs this renderer's JS, so React state, handlers and stores work as if it were inline; the styles are copied in.
import { createContext, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { agu } from "../bridge";

const open = new Map<string, Window>();

/** True once the user resized the pop-out by hand: its content should fill the window instead of sizing it. */
export const PopoutFill = createContext(false);

/** Bring an already open pop-out to the front. False when there is none. */
export function focusPopout(id: string): boolean {
  const w = open.get(id);
  if (!w || w.closed) return false;
  w.focus();
  return true;
}

function copyStyles(from: Document, to: Document) {
  for (const sheet of Array.from(from.styleSheets)) {
    try {
      const el = to.createElement("style");
      el.textContent = Array.from(sheet.cssRules)
        .map((r) => r.cssText)
        .join("\n");
      to.head.appendChild(el);
    } catch {
      if (!sheet.href) continue;
      const link = to.createElement("link");
      link.rel = "stylesheet";
      link.href = sheet.href;
      to.head.appendChild(link);
    }
  }
}

export function PopoutWindow({
  id,
  x,
  y,
  width,
  title,
  onClose,
  children,
}: {
  id: string;
  /** Screen position of the window's top-left corner. */
  x: number;
  y: number;
  width: number;
  title: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const [host, setHost] = useState<HTMLElement | null>(null);
  const [fill, setFill] = useState(false);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    const scr = window.screen as Screen & { availLeft?: number; availTop?: number };
    const minX = scr.availLeft ?? 0;
    const minY = scr.availTop ?? 0;
    const left = Math.round(Math.max(minX, Math.min(x, minX + scr.availWidth - width)));
    const top = Math.round(Math.max(minY, Math.min(y, minY + scr.availHeight - 200)));
    const w = window.open("about:blank", `agu-popout:${id}`, `left=${left},top=${top},width=${width},height=200`);
    if (!w) {
      closeRef.current();
      return;
    }
    open.set(id, w);
    const d = w.document;
    d.title = title;
    copyStyles(document, d);
    d.documentElement.className = document.documentElement.className;
    d.body.className = document.body.className;
    const root = d.createElement("div");
    d.body.appendChild(root);

    // Fit the window's height to its content (the card grows when its info loads or a panel opens).
    const RO = (w as unknown as { ResizeObserver: typeof ResizeObserver }).ResizeObserver;
    let lastH = 0;
    const ro = new RO(() => {
      const h = Math.ceil(root.getBoundingClientRect().height);
      if (h <= 0 || h === lastH) return;
      lastH = h;
      agu.win.fitPopout(`agu-popout:${id}`, h).catch(() => {});
    });
    ro.observe(root);

    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeRef.current();
    };
    const gone = () => closeRef.current(); // Alt+F4, or the main window closed it
    w.addEventListener("keydown", onKey);
    w.addEventListener("pagehide", gone);
    setHost(root);
    w.focus();
    return () => {
      ro.disconnect();
      w.removeEventListener("pagehide", gone);
      if (open.get(id) === w) open.delete(id);
      w.close();
    };
    // Opened once per id; later position changes don't move a window the user may have dragged.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id]);

  useEffect(() => {
    const w = open.get(id);
    if (w && !w.closed) w.document.title = title;
  }, [id, title, host]);

  useEffect(() => {
    setFill(false);
    return agu.win.onPopoutUserSized((name) => name === `agu-popout:${id}` && setFill(true));
  }, [id]);

  useEffect(() => {
    if (!host) return;
    const d = host.ownerDocument;
    for (const el of [d.documentElement, d.body, host]) el.style.height = fill ? "100%" : "";
  }, [host, fill]);

  return host ? createPortal(<PopoutFill.Provider value={fill}>{children}</PopoutFill.Provider>, host) : null;
}
