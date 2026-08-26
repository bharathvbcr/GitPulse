import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { normalizeDiffPayload } from "../stores/graphStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "DiffViewer.svelte"),
  "utf8"
);

describe("DiffViewer truncation banner", () => {
  it("renders the banner only when the payload says it was truncated", () => {
    expect(source).toMatch(/\{#if commitTruncated && !bannerDismissed\}/);
  });

  it("announces shown vs total files and shown vs total line counts", () => {
    expect(source).toContain("Large commit: showing");
    expect(source).toContain("lines shown of");
    expect(source).toContain("total)");
  });

  it("caps the skipped-file list and reports the overflow", () => {
    expect(source).toContain(".slice(0, MAX_SKIPPED_SHOWN)");
    expect(source).toContain("const MAX_SKIPPED_SHOWN = 5;");
    expect(source).toContain("more files not shown");
  });

  it("is dismissible and re-arms on selection change", () => {
    expect(source).toContain('aria-label="Dismiss truncation notice"');
    expect(source).toContain("bannerDismissed = true");
    expect(source).toMatch(/bannerDismissed = false;/);
  });

  it("still renders through the normalizer so legacy strings keep working", () => {
    expect(source).toContain("normalizeDiffPayload($repoStore.selectedDiff)");
    expect(source).toContain("parseUnifiedDiff(diffPayload.content)");
  });
});

describe("normalizeDiffPayload", () => {
  const full = {
    content: "diff --git a/a.txt b/a.txt\n--- a/a.txt",
    truncated: true,
    included_files: 3,
    skipped_files: [{ path: "big.log", additions: 900, deletions: 12 }],
    total_files: 4,
    total_additions: 1000,
    total_deletions: 50,
  };

  it("passes a well-formed payload through unchanged", () => {
    expect(normalizeDiffPayload(full)).toEqual(full);
  });

  it("treats a legacy bare string as untruncated content", () => {
    expect(normalizeDiffPayload("some raw unified diff")).toEqual({
      content: "some raw unified diff",
      truncated: false,
      included_files: 0,
      skipped_files: [],
      total_files: 0,
      total_additions: 0,
      total_deletions: 0,
    });
  });

  it("returns safe defaults for null, undefined and non-object roots", () => {
    const expected = {
      content: "",
      truncated: false,
      included_files: 0,
      skipped_files: [],
      total_files: 0,
      total_additions: 0,
      total_deletions: 0,
    };
    expect(normalizeDiffPayload(null)).toEqual(expected);
    expect(normalizeDiffPayload(undefined)).toEqual(expected);
    expect(normalizeDiffPayload(42)).toEqual(expected);
    expect(normalizeDiffPayload(["not", "an", "object"])).toEqual(expected);
    expect(normalizeDiffPayload(true)).toEqual(expected);
  });

  it("fills missing fields on partial objects with safe defaults", () => {
    expect(normalizeDiffPayload({ content: "only content" })).toEqual({
      ...full,
      content: "only content",
      truncated: false,
      included_files: 0,
      skipped_files: [],
      total_files: 0,
      total_additions: 0,
      total_deletions: 0,
    });
    expect(normalizeDiffPayload({ truncated: true }).content).toBe("");
  });

  it("coerces wrong-typed scalar fields instead of trusting them", () => {
    const payload = normalizeDiffPayload({
      content: 1234,
      truncated: "yes",
      included_files: "3",
      total_files: Number.NaN,
      total_additions: Number.POSITIVE_INFINITY,
      total_deletions: -7,
      skipped_files: "nope",
    });
    expect(payload.content).toBe("");
    expect(payload.truncated).toBe(false);
    expect(payload.included_files).toBe(0);
    expect(payload.total_files).toBe(0);
    expect(payload.total_additions).toBe(0);
    expect(payload.total_deletions).toBe(0);
    expect(payload.skipped_files).toEqual([]);
  });

  it("drops malformed skipped-file entries and coerces surviving stats", () => {
    expect(
      normalizeDiffPayload({
        ...full,
        skipped_files: [
          null,
          7,
          "path-only",
          { additions: 5 },
          { path: "kept.txt", additions: "x", deletions: -1 },
          { path: "real.bin", additions: 4, deletions: 1 },
        ],
      }).skipped_files
    ).toEqual([
      { path: "kept.txt", additions: 0, deletions: 0 },
      { path: "real.bin", additions: 4, deletions: 1 },
    ]);
    expect(normalizeDiffPayload({ ...full, skipped_files: [] }).skipped_files).toEqual([]);
  });
});
