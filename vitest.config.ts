import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { appVersion } from "./scripts/app-version.mjs";
import { gitpulseIgnoreNestedWorktrees } from "./scripts/vite-watch.mjs";

// A coverage run instruments every function; wall-clock limits in tests scale
// for it through `src/lib/testing/wallClock.ts`, which reads this flag.
const coverageRun = process.argv.includes("--coverage") || process.env.npm_lifecycle_event === "coverage";

export default defineConfig({
  // `vitest` watch mode runs on a vite dev server and its file watcher.
  plugins: [tailwindcss(), svelte(), gitpulseIgnoreNestedWorktrees()],
  // Same definition as the production build, from the same source.
  define: {
    __APP_VERSION__: JSON.stringify(appVersion()),
    __APP_BUILD_ID__: JSON.stringify("vitest"),
  },
  test: {
    environment: "node",
    env: { GITPULSE_COVERAGE_RUN: coverageRun ? "1" : "" },
    globals: true,
    // Coverage instrumentation plus 18 CPU-bound file workers starved the
    // stress tests past their 5s safety budgets on an 18-core host. Four
    // concurrent files keep the suite parallel while the same hostile cases
    // complete in 1.5-2.6s, so the budgets stay strict instead of being raised.
    // Tests whose cost is serial process spawns rather than CPU derive their
    // own budget from case count (src/lib/commit/agentCommit.stress.test.ts).
    maxWorkers: 4,
    include: ["src/**/*.{test,spec}.{js,ts}", "scripts/**/*.test.ts", "harness/**/*.test.ts"],
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
      // Every entry below the test globs is a hole in the floor, so each one
      // names who owns closing it and the date it must be looked at again
      // (`scripts/coverage-exclusions-contract.test.ts` fails an entry without
      // both, and one whose date has passed). Moving the date forward is a
      // decision somebody makes on purpose; it is not an automatic renewal.
      // `repoMetrics.ts` and `graphStore.ts` used to be here and now count.
      exclude: [
        "src/**/*.test.ts",
        "src/**/*.spec.ts",
        "src/**/__tests__/**",
        "src/lib/stores/repoStore.ts", // owner: bharath; review by 2027-01-31; 4,043 lines, exercised through repoStore.*.test.ts but not attributable per line
        "src/lib/stores/harnessStore.ts", // owner: bharath; review by 2027-01-31; 964 lines, exercised through harnessStore.test.ts
        "src/lib/desktop/nativeShell.ts", // owner: bharath; review by 2027-01-31; Tauri window glue, exercised by nativeShell.test.ts
        "src/lib/codeintel/previewStore.ts", // owner: bharath; review by 2027-01-31; new surface still gaining unit tests
        "src/lib/docs/liveVault.ts", // owner: bharath; review by 2027-01-31; 29 lines, thin wrapper over the vault IPC
        "src/lib/syntax/**", // owner: bharath; review by 2027-01-31; 20-line IPC client
        "src/lib/insights/client.ts", // owner: bharath; review by 2027-01-31; 14-line IPC client
        "**/*.svelte", // owner: bharath; review by 2027-01-31; components run in the browser harnesses (bun run test:browser), not under v8 coverage
      ],
    },
  },
});
