// Custom alert sounds. A file the user picks is copied into userData/sounds, so it keeps working if the
// original moves, and the renderer only ever names a file inside that folder (never a path).
import { app, dialog } from "electron";
import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { isSoundFileName } from "./highlightRules.js";

const EXTENSIONS = ["mp3", "wav", "ogg", "oga", "m4a", "aac", "flac", "webm", "opus"];
const MAX_BYTES = 5 * 1024 * 1024;

const soundsDir = () => path.join(app.getPath("userData"), "sounds");

/** Ask for an audio file and copy it into the sounds folder. Resolves { file } or null when cancelled. */
export async function pickSound(win) {
  const opts = {
    title: "Choose an alert sound",
    properties: ["openFile"],
    filters: [{ name: "Audio", extensions: EXTENSIONS }],
  };
  const res = await (win ? dialog.showOpenDialog(win, opts) : dialog.showOpenDialog(opts));
  if (res.canceled || !res.filePaths.length) return null;
  const src = res.filePaths[0];
  const ext = path.extname(src).slice(1).toLowerCase();
  if (!EXTENSIONS.includes(ext)) throw new Error("That file type is not supported. Choose an mp3, wav, ogg, m4a or flac file.");
  const stat = fs.statSync(src);
  if (stat.size > MAX_BYTES) throw new Error("That file is too big for an alert sound (5 MB at most).");
  const data = fs.readFileSync(src);
  // Readable name plus a short content hash: the same file picked twice lands on the same name.
  const base =
    path
      .basename(src, path.extname(src))
      .replace(/[^A-Za-z0-9 _-]+/g, "")
      .trim()
      .slice(0, 60) || "sound";
  const hash = crypto.createHash("sha1").update(data).digest("hex").slice(0, 8);
  const file = `${base}-${hash}.${ext}`;
  fs.mkdirSync(soundsDir(), { recursive: true });
  fs.writeFileSync(path.join(soundsDir(), file), data);
  return { file };
}

/** The bytes of a file in the sounds folder. */
export function readSound(file) {
  if (!isSoundFileName(file)) throw new Error("Not a sound file.");
  const ext = path.extname(file).slice(1).toLowerCase();
  if (!EXTENSIONS.includes(ext)) throw new Error("Not a sound file.");
  const full = path.join(soundsDir(), file);
  if (path.dirname(full) !== soundsDir()) throw new Error("Not a sound file.");
  const stat = fs.statSync(full);
  if (stat.size > MAX_BYTES) throw new Error("Sound file is too big.");
  return new Uint8Array(fs.readFileSync(full));
}
