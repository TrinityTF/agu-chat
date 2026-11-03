// Renders build/icon.svg to build/icon.png (512x512); electron-builder turns the PNG into the .exe/.ico icon.
// Run with: npm run icon   (plain node is fine: it starts Electron itself to rasterize the SVG)
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const SIZE = 512;
const here = path.dirname(fileURLToPath(import.meta.url));
const svgPath = path.join(here, "..", "build", "icon.svg");
const pngPath = path.join(here, "..", "build", "icon.png");

if (!process.versions.electron) {
  // Running under plain node: re-run this same file under Electron.
  const electronPath = createRequire(import.meta.url)("electron");
  const res = spawnSync(electronPath, [fileURLToPath(import.meta.url)], { stdio: "inherit" });
  process.exit(res.status ?? 1);
} else {
  const { app, BrowserWindow } = await import("electron");
  const svg = fs.readFileSync(svgPath, "utf8").replace("<svg ", `<svg width="${SIZE}" height="${SIZE}" `);
  const html = `<html><body style="margin:0;background:transparent">${svg}</body></html>`;

  app.disableHardwareAcceleration();
  // No top-level await on whenReady: Electron only emits "ready" once the main module has finished loading.
  app.whenReady().then(async () => {
    const win = new BrowserWindow({
      width: SIZE,
      height: SIZE,
      show: false,
      frame: false,
      transparent: true,
      useContentSize: true,
      webPreferences: { offscreen: true },
    });
    await win.loadURL(`data:text/html;charset=utf-8,${encodeURIComponent(html)}`);
    await new Promise((resolve) => setTimeout(resolve, 500));
    const image = await win.webContents.capturePage({ x: 0, y: 0, width: SIZE, height: SIZE });
    fs.writeFileSync(pngPath, image.resize({ width: SIZE, height: SIZE }).toPNG());
    console.log(`Wrote ${pngPath}`);
    app.quit();
  });
}
