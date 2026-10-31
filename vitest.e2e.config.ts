import { defineConfig } from "vitest/config";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  test: {
    environment: "node",
    globals: true,
    maxWorkers: 4,
    testTimeout: 30_000,
    hookTimeout: 30_000,
    include: ["tests/e2e/**/*.test.ts"],
    reporters: ["default"],
  },
  resolve: {
    alias: {
      "@": path.resolve(root, "src"),
    },
  },
});
