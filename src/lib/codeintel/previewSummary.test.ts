import { describe, expect, it } from "vitest";
import { normalizePreviewReport } from "./previewNormalize";
import {
  fileHonesty,
  isUnreliableParseStatus,
  markersByPath,
  summarizePreview,
} from "./previewSummary";
import type { DevmapPreviewFileResult } from "./types";

describe("previewNormalize", () => {
  it("maps CLI broken_callers resolution and walk_incomplete", () => {
    const report = normalizePreviewReport({
      file_path: "src/a.ts",
      parse_status: "Fallback",
      delta_available: false,
      file_is_indexed: true,
      compared_against: "disk",
      degraded_reason: "no grammar",
      symbols: [],
      bodies_not_compared: 2,
      ambiguous_callers: 1,
      broken_callers: {
        items: [{ source_symbol: "caller" }],
        shown: 1,
        hidden: 0,
        total: 1,
        truncated: false,
        walk_incomplete: "missing edges",
        resolution: { Available: null },
      },
    });
    expect(report).not.toBeNull();
    expect(report!.parse_status).toBe("Fallback");
    expect(report!.broken_callers.available).toBe(true);
    expect(report!.broken_callers.walk_incomplete).toBe("missing edges");
    expect(report!.broken_callers.items).toHaveLength(1);
  });

  it("marks Unavailable resolution as unavailable", () => {
    const report = normalizePreviewReport({
      file_path: "src/a.ts",
      parse_status: "Clean",
      delta_available: true,
      file_is_indexed: true,
      compared_against: "disk",
      symbols: [],
      bodies_not_compared: 0,
      ambiguous_callers: 0,
      broken_callers: {
        items: [],
        shown: 0,
        total: 0,
        truncated: false,
        resolution: { Unavailable: { reason: "not indexed" } },
      },
    });
    expect(report!.broken_callers.available).toBe(false);
    expect(report!.broken_callers.reason).toBe("not indexed");
  });
});

describe("previewSummary honesty", () => {
  it("treats Fallback as unreliable and never claim_clean", () => {
    expect(isUnreliableParseStatus("Fallback")).toBe(true);
    const file: DevmapPreviewFileResult = {
      file_path: "src/a.ts",
      available: true,
      reason: null,
      report: {
        file_path: "src/a.ts",
        parse_status: "Fallback",
        delta_available: false,
        file_is_indexed: true,
        compared_against: "disk",
        degraded_reason: null,
        symbols: [],
        bodies_not_compared: 0,
        ambiguous_callers: 0,
        broken_callers: {
          available: true,
          reason: null,
          items: [],
          total: 0,
          shown: 0,
          truncated: false,
        },
      },
    };
    const h = fileHonesty(file);
    expect(h.unreliable).toBe(true);
    expect(h.claim_clean).toBe(false);
    const summary = summarizePreview([file]);
    expect(summary.headline).toMatch(/unreliable/i);
    expect(summary.headline).toMatch(/do not treat as/);
    expect(summary.claimCleanCount).toBe(0);
    const marker = markersByPath([file]).get("src/a.ts");
    expect(marker?.kind).toBe("unreliable");
    expect(marker?.label).toBe("!");
  });

  it("surfaces broken callers when the parse is reliable", () => {
    const file: DevmapPreviewFileResult = {
      file_path: "src/b.ts",
      available: true,
      reason: null,
      report: {
        file_path: "src/b.ts",
        parse_status: "Clean",
        delta_available: true,
        file_is_indexed: true,
        compared_against: "disk",
        degraded_reason: null,
        symbols: [],
        bodies_not_compared: 0,
        ambiguous_callers: 0,
        broken_callers: {
          available: true,
          reason: null,
          items: [{ source_symbol: "x" }],
          total: 3,
          shown: 1,
          truncated: true,
          walk_incomplete: "partial",
        },
      },
    };
    const summary = summarizePreview([file]);
    expect(summary.brokenCallerTotal).toBe(3);
    expect(summary.headline).toContain("3 broken caller");
    expect(summary.files[0].broken_truncated).toBe(true);
    expect(summary.files[0].walk_incomplete).toBe("partial");
  });
});
