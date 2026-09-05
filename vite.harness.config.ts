import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { appVersion } from "./scripts/app-version.mjs";

// Harness-only: root is THIS worktree so ./src is my branch, and cacheDir is
// private so optimizing deps here cannot invalidate the dev server another
// session runs out of the shared node_modules/.vite.
export default defineConfig({
  plugins: [svelte()],
  cacheDir: "/private/tmp/claude-501/gp-harness-vite-cache",
  define: { __APP_VERSION__: JSON.stringify(appVersion()) },
  server: { port: 5188, strictPort: true },
});
