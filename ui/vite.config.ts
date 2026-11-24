import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host = process.env.TAURI_ENV_PLATFORM === "windows" ? "127.0.0.1" : "::1";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host,
    hmr: host === "::1" ? { protocol: "ws", host: "::1", port: 15274 } : undefined,
  },
  build: {
    target: "es2022",
    outDir: "dist",
  },
});
