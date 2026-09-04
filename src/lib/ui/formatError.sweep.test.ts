import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));

/**
 * Files covered by the formatError sweep. Every error-stringification site
 * in these files must go through formatError so object-shaped IPC
 * rejections render as readable JSON instead of "[object Object]".
 */
const SWEPT_FILES = [
  "../stores/repoStore.ts",
  "../stores/graphStore.ts",
  "../stores/harnessStore.ts",
  "../components/WorktreesPanel.svelte",
  "../components/ManviOpsPanel.svelte",
  "../components/ManviHarnessPane.svelte",
  "../components/ConflictEditor.svelte",
  "../components/CodeStackViewer.svelte",
  "../components/CoverageViewer.svelte",
  "../components/BlameViewer.svelte",
  "../components/FileViewer.svelte",
  "../components/files/FileTreePanel.svelte",
  "../components/files/CodeViewer.svelte",
  "../components/CommitDetails.svelte",
  "../components/CommitComposer.svelte",
  "../components/ReflogViewer.svelte",
  "../components/BranchList.svelte",
  "../desktop/nativeShell.ts",
  "../diff/conflictSave.ts",
];

// Matches String(err), String(reason), String(error), String(e) — the
// rejection-stringifying shapes. Legitimate uses like String(id) do not
// start with an error identifier.
const RAW_ERROR_STRINGIFY = /\bString\(\s*(err|reason|error|e)\b/;

// Panels migrated to the diagnostics seam report through `reportPanelError`,
// while stores that need the same redaction-safe text use
// `formatDiagnosticFailure` directly. Each path delegates to formatError;
// raw String(err) satisfies none of them.
const FORMATTER_SEAM = [
  { marker: "formatError(", importNeedle: "ui/formatError" },
  { marker: "reportPanelError(", importNeedle: "diagnostics/report" },
  { marker: "formatDiagnosticFailure(", importNeedle: "diagnostics/diagnostics" },
] as const;

function routesThroughFormatter(source: string): boolean {
  return FORMATTER_SEAM.some(
    ({ marker, importNeedle }) => source.includes(marker) && source.includes(importNeedle),
  );
}

describe("formatError sweep", () => {
  it("leaves no raw rejection stringification behind", () => {
    const offenders: string[] = [];
    for (const rel of SWEPT_FILES) {
      const source = readFileSync(join(here, rel), "utf8");
      if (RAW_ERROR_STRINGIFY.test(source)) offenders.push(rel);
    }
    expect(offenders).toEqual([]);
  });

  it("routes every swept file's rejections through formatError or the diagnostics seam", () => {
    const offenders: string[] = [];
    for (const rel of SWEPT_FILES) {
      const source = readFileSync(join(here, rel), "utf8");
      if (!routesThroughFormatter(source)) offenders.push(rel);
    }
    expect(offenders).toEqual([]);
  });
});
