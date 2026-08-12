import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Tauri's devUrl in tauri.conf.json names this port, so a silent fallback to the next free
// port would leave the shell loading nothing.
const DEV_PORT = 1420;

export default defineConfig({
  plugins: [react()],
  // Vite's screen clearing wipes the Rust compiler output in a `cargo tauri dev` session.
  clearScreen: false,
  server: {
    port: DEV_PORT,
    strictPort: true,
    watch: {
      // The Rust crate has its own watcher inside `cargo tauri dev`.
      ignored: ["**/src-tauri/**"],
    },
  },
});
