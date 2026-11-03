// One log file in userData (agu-chat.log), also echoed to stdout in dev.
import { app } from "electron";
import fs from "node:fs";
import path from "node:path";

let stream = null;

export function log(...args) {
  const line = `${new Date().toISOString()} ${args
    .map((a) => (a instanceof Error ? a.stack || a.message : typeof a === "string" ? a : JSON.stringify(a)))
    .join(" ")}`;
  if (!app.isPackaged) console.log(line);
  try {
    if (!stream) stream = fs.createWriteStream(path.join(app.getPath("userData"), "agu-chat.log"), { flags: "a" });
    stream.write(`${line}\n`);
  } catch {}
}
