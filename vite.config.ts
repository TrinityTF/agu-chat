import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  root: "src/renderer",
  base: "./",
  plugins: [react(), tailwindcss()],
  server: { port: 5199, strictPort: true },
  build: { outDir: "../../out/renderer", emptyOutDir: true },
});
