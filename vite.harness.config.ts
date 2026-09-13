import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { appVersion } from "./scripts/app-version.mjs";

// Harness-only: root is THIS worktree so ./src is my branch, and cacheDir is
// private so optimizing deps here cannot invalidate the dev server another
// session runs out of the shared node_modules/.vite.
//
// tailwindcss() is not optional here. Naming a config file REPLACES
// vite.config.ts rather than merging with it — scripts/browser-regressions.mjs
// passes its plugins inline and so still inherits Tailwind, but this file did
// not, so every harness page it served rendered as unstyled stacked text. The
// DOM-assertion checks pass either way, which is exactly why it went
// unnoticed: looking at the page is the one thing this config is for.
export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  cacheDir: "/private/tmp/claude-501/gp-harness-vite-cache",
  define: { __APP_VERSION__: JSON.stringify(appVersion()) },
  server: { port: 5188, strictPort: true },
});
