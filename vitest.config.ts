import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: "node",
    globals: true,
    include: ["src/**/*.{test,spec}.{js,ts}", "scripts/**/*.test.ts"],
    coverage: {
      provider: "v8",
      reporter: ["text", "lcov", "json-summary"],
      reportsDirectory: "coverage",
      include: ["src/**/*.{ts,js,svelte}"],
      exclude: ["src/**/*.{test,spec}.ts", "src/**/__tests__/**"],
    },
  },
});
