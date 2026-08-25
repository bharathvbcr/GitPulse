import { defineConfig } from "vitest/config";
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
  test: {
    include: ["src/**/*.{test,spec}.ts"],
    environment: "node",
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov", "html"],
      include: ["src/lib/**/*.ts"],
      exclude: [
        "src/**/*.test.ts",
        "src/**/*.spec.ts",
        "src/**/__tests__/**",
        "src/lib/stores/repoStore.ts",
        "src/lib/stores/graphStore.ts",
        "src/lib/stores/harnessStore.ts",
        "src/lib/desktop/nativeShell.ts",
        "**/*.svelte",
        "src/main.ts",
        "src/App.svelte",
      ],
      reportsDirectory: "./coverage",
    },
  },
});
