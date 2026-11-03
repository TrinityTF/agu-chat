// `npm run dev`: Vite dev server for the renderer, then Electron pointed at it.
import { createServer } from "vite";
import { spawn } from "node:child_process";
import electronPath from "electron";

const server = await createServer({ configFile: "vite.config.ts" });
await server.listen();
const url = server.resolvedUrls.local[0];
console.log(`[dev] renderer at ${url}`);

const child = spawn(electronPath, ["."], { stdio: "inherit", env: { ...process.env, AGU_DEV_URL: url } });
child.on("exit", async (code) => {
  await server.close();
  process.exit(code ?? 0);
});
