import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { appVersion } from "./scripts/app-version.mjs";

export default defineConfig({
  plugins: [svelte()],
  // Same definition as the production build, from the same source.
  define: {
    __APP_VERSION__: JSON.stringify(appVersion()),
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
        "**/*.svelte",
      ],
    },
  },
});
