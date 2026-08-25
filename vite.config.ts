import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { portFromEnv } from "./scripts/dev-port.mjs";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: portFromEnv(),
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
});
