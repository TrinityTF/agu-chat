// Where the main window opens: its saved bounds, pulled back onto a screen when they are no longer on one.
// Pure (no Electron), so node --test can load it; windowState.js does the saving and loading.
const TITLE_BAR = 28; // the tab bar, which is also the drag area
const GRAB_W = 120; // this much of the title bar must be on a screen to count as reachable
const MIN_W = 280;
const MIN_H = 300;

const num = (v) => typeof v === "number" && Number.isFinite(v);

function overlap(a, b) {
  const w = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
  const h = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y);
  return { w: Math.max(0, w), h: Math.max(0, h) };
}

/**
 * Where to open, given the saved bounds and the screens' work areas (primary first).
 * Returns {x, y, width, height}, or null to let the window take its default centered spot.
 */
export function placeBounds(saved, workAreas) {
  if (!saved || ![saved.x, saved.y, saved.width, saved.height].every(num) || !workAreas.length) return null;
  const bar = { x: saved.x, y: saved.y, width: saved.width, height: TITLE_BAR };
  let best = null;
  for (const wa of workAreas) {
    const o = overlap(bar, wa);
    if (o.w >= Math.min(GRAB_W, saved.width) && o.h >= TITLE_BAR / 2 && (!best || o.w * o.h > best.area)) best = { wa, area: o.w * o.h };
  }
  const wa = best?.wa ?? workAreas[0];
  const width = Math.round(Math.max(MIN_W, Math.min(saved.width, wa.width)));
  const height = Math.round(Math.max(MIN_H, Math.min(saved.height, wa.height)));
  if (!best) {
    // Nowhere visible: same size, centered on the primary screen.
    return { x: Math.round(wa.x + (wa.width - width) / 2), y: Math.round(wa.y + (wa.height - height) / 2), width, height };
  }
  // Reachable, but keep the whole window on that screen where it fits.
  const x = Math.round(Math.max(wa.x, Math.min(saved.x, wa.x + wa.width - width)));
  const y = Math.round(Math.max(wa.y, Math.min(saved.y, wa.y + wa.height - height)));
  return { x, y, width, height };
}
