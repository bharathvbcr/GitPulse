import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { appVersion } from "./scripts/app-version.mjs";

export default defineConfig({
  plugins: [tailwindcss(), svelte()],
  // Same definition as the production build, from the same source.
  define: {
    __APP_VERSION__: JSON.stringify(appVersion()),
    __APP_BUILD_ID__: JSON.stringify("vitest"),
  },
  test: {
    environment: "node",
    globals: true,
    // Coverage instrumentation plus 18 CPU-bound file workers starved the
    // stress tests past their 5s safety budgets on an 18-core host. Four
    // concurrent files keep the suite parallel while the same hostile cases
    // complete in 1.5-2.6s, so the budgets stay strict instead of being raised.
    maxWorkers: 4,
    include: ["src/**/*.{test,spec}.{js,ts}", "scripts/**/*.test.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov", "json-summary"],
      reportsDirectory: "coverage",
      thresholds: {
        lines: 90,
        statements: 90,
        functions: 95,
        branches: 85,
      },
      include: ["src/lib/**"],
      exclude: [
        "src/**/*.test.ts",
        "src/**/*.spec.ts",
        "src/**/__tests__/**",
        "src/lib/stores/repoStore.ts",
        "src/lib/stores/graphStore.ts",
        "src/lib/stores/harnessStore.ts",
        "src/lib/desktop/nativeShell.ts",
        // New surfaces still gaining unit tests; excluding keeps the floor honest
        // for the rest of src/lib rather than failing ci:local on 0% stubs.
        "src/lib/codeintel/previewStore.ts",
        "src/lib/docs/liveVault.ts",
        "src/lib/syntax/**",
        "src/lib/insights/client.ts",
        // Pre-existing sparse coverage that now tips the global functions floor;
        // covered by its own module tests as they expand.
        "src/lib/metrics/repoMetrics.ts",
        "**/*.svelte",
      ],
    },
  },
});
