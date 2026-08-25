import { describe, it, expect } from "vitest";
import { buildHitMap, hitBadgeClass } from "./fileCoverage";

describe("coverage file helpers", () => {
  it("maps covered lines onto line numbers", () => {
    const hits = buildHitMap([
      { line_no: 3, hits: 2 },
      { line_no: 7, hits: 0 },
    ]);
    expect(hits.get(3)).toBe(2);
    expect(hits.get(7)).toBe(0);
    expect(hits.size).toBe(2);
  });

  it("returns an empty map for empty line lists", () => {
    const hits = buildHitMap([]);
    expect(hits.size).toBe(0);
  });

  it("lets later duplicate lines win", () => {
    const hits = buildHitMap([
      { line_no: 1, hits: 1 },
      { line_no: 1, hits: 5 },
    ]);
    expect(hits.get(1)).toBe(5);
    expect(hits.size).toBe(1);
  });

  it("styles unknown lines transparent", () => {
    expect(hitBadgeClass(undefined)).toBe(
      "w-8 px-1 text-right text-[10px] tabular-nums shrink-0 text-transparent"
    );
  });

  it("styles hit lines emerald", () => {
    expect(hitBadgeClass(1)).toBe(
      "w-8 px-1 text-right text-[10px] tabular-nums shrink-0 text-emerald-400/80"
    );
  });

  it("styles missed lines red", () => {
    expect(hitBadgeClass(0)).toBe(
      "w-8 px-1 text-right text-[10px] tabular-nums shrink-0 text-red-400/80"
    );
  });

  it("fetches file coverage via invoke", async () => {
    const origWindow = (globalThis as Record<string, unknown>).window;
    (globalThis as Record<string, unknown>).window = {
      __TAURI_INTERNALS__: {
        invoke: async () => ({
          path: "src/file.ts",
          language: "TypeScript",
          color_hex: "#3178c6",
          lines: [],
          totals: { lines_found: 0, lines_hit: 0, percentage: 0 },
          truncated: false,
          lines_truncated: false,
        }),
      },
    };
    try {
      const { fetchFileCoverage } = await import("./fileCoverage");
      const result = await fetchFileCoverage("/repo", "src/file.ts");
      expect(result.path).toBe("src/file.ts");
    } finally {
      if (origWindow !== undefined) {
        (globalThis as Record<string, unknown>).window = origWindow;
      } else {
        delete (globalThis as Record<string, unknown>).window;
      }
    }
  });
});
