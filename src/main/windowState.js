// The main window's position, size and maximized state, saved in userData/window.json and restored on
// start. A saved spot that is no longer on any screen (a monitor unplugged, resolution changed) is pulled
// back: the window always opens with its title bar on a screen, where it can be grabbed.
import { readJson, writeJson } from "./store.js";

export { placeBounds } from "./windowPlace.js";

const FILE = "window.json";
export function loadWindowState() {
  const s = readJson(FILE, null);
  return s && typeof s === "object" ? s : null;
}

/** Saves the window's normal (un-maximized) bounds plus whether it was maximized. */
export function trackWindowState(win) {
  let timer = null;
  const save = () => {
    timer = null;
    if (win.isDestroyed() || win.isMinimized()) return;
    const b = win.getNormalBounds();
    try {
      writeJson(FILE, { x: b.x, y: b.y, width: b.width, height: b.height, maximized: win.isMaximized() });
    } catch {
      /* not worth failing over */
    }
  };
  const soon = () => {
    if (timer) clearTimeout(timer);
    timer = setTimeout(save, 500);
  };
  for (const ev of ["resize", "move", "maximize", "unmaximize"]) win.on(ev, soon);
  win.on("close", () => {
    if (timer) clearTimeout(timer);
    save();
  });
}
