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
