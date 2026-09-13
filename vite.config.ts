import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    // A dedicated port, not the Tauri default 1420.
    //
    // 1420 is the first port any Tauri project reaches for, and another local
    // dev server already holds it, so this app's vite exited on startup
    // (strictPort) while the desktop window kept loading `devUrl` — which then
    // showed whatever else had taken the port. A port nobody shares is worth
    // more than the conventional number.
    port: 1437,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1438,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
