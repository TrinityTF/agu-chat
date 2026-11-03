// Crops an icon PNG to its visible artwork (dropping the transparent border), centres it on a square with a
// small margin and writes build/icon.png at 1024x1024. Windows draws taskbar/desktop icons at a fixed box, so
// a padded source shows up visibly smaller than its neighbours.
// Usage: node scripts/fit-icon.mjs <source.png> [margin fraction, default 0.02]
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.join(here, "..", "build", "icon.png");
const SIZE = 1024;
const ALPHA_MIN = 24; // ignore the faint outer glow when measuring the artwork

if (!process.versions.electron) {
  const electronPath = createRequire(import.meta.url)("electron");
  const res = spawnSync(electronPath, [fileURLToPath(import.meta.url), ...process.argv.slice(2)], { stdio: "inherit" });
  process.exit(res.status ?? 1);
} else {
  const { app, nativeImage } = await import("electron");
  const args = process.argv.slice(process.argv.findIndex((a) => a.endsWith("fit-icon.mjs")) + 1);
  const [src, marginArg] = args;
  const margin = Number(marginArg ?? 0.02);
  app.whenReady().then(() => {
    const img = nativeImage.createFromPath(path.resolve(src));
    const { width, height } = img.getSize();
    const px = img.toBitmap(); // BGRA
    let x0 = width, y0 = height, x1 = -1, y1 = -1;
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        if (px[(y * width + x) * 4 + 3] < ALPHA_MIN) continue;
        if (x < x0) x0 = x;
        if (x > x1) x1 = x;
        if (y < y0) y0 = y;
        if (y > y1) y1 = y;
      }
    }
    const w = x1 - x0 + 1, h = y1 - y0 + 1;
    const side = Math.round(Math.max(w, h) * (1 + 2 * margin));
    // Square canvas, artwork centred.
    const canvas = Buffer.alloc(side * side * 4);
    const ox = Math.floor((side - w) / 2), oy = Math.floor((side - h) / 2);
    for (let y = 0; y < h; y++) {
      px.copy(canvas, ((oy + y) * side + ox) * 4, ((y0 + y) * width + x0) * 4, ((y0 + y) * width + x0 + w) * 4);
    }
    const squared = nativeImage.createFromBitmap(canvas, { width: side, height: side });
    fs.writeFileSync(out, squared.resize({ width: SIZE, height: SIZE, quality: "best" }).toPNG());
    console.log(`artwork ${w}x${h} at (${x0},${y0}) of ${width}x${height} -> ${out}`);
    app.quit();
  });
}
