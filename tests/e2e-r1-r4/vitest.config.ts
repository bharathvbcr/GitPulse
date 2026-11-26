import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { appVersion } from "../../scripts/app-version.mjs";
import { gitpulseIgnoreNestedWorktrees } from "../../scripts/vite-watch.mjs";

export default defineConfig({
  plugins: [tailwindcss(), svelte(), gitpulseIgnoreNestedWorktrees()],
  define: {
    __APP_VERSION__: JSON.stringify(appVersion()),
    __APP_BUILD_ID__: JSON.stringify("vitest-e2e-r1-r4"),
  },
  test: {
    environment: "node",
    globals: true,
    fsModuleCache: false,
    fileParallelism: false,
    maxWorkers: 1,
    testTimeout: 90_000,
    hookTimeout: 90_000,
    include: ["tests/e2e-r1-r4/**/*.test.ts"],
    reporters: ["default"],
  },
});
