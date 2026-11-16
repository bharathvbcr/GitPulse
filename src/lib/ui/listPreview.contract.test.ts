import { describe, expect, it } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { TIMELINE_PREVIEW_COUNT } from "../delivery/timeline";
import { RUN_PREVIEW_COUNT, WORKFLOW_PREVIEW_COUNT } from "../github/runActions";
import { LIST_PREVIEW_ROWS } from "./previewList";

/**
 * One 5-row preview on every long list, and one control to reveal the rest.
 *
 * Each page used to keep its own count (Health 8, run cards 3, workflows 5,
 * the delivery timeline 3) and its own "Show all" button, so the same gesture
 * meant different things on neighbouring pages. These checks read the source
 * because the failure they guard is a file quietly keeping its own copy.
 */
const lib = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (relative: string): string => readFileSync(join(lib, relative), "utf8");

/**
 * Every component whose long list is previewed, with the markup that proves it
 * draws the shared control. Each also imports the shared slice.
 */
const CONVERTED: readonly { file: string; slices: boolean }[] = [
  { file: "components/HealthPanel.svelte", slices: true },
  { file: "components/health/SecretsSection.svelte", slices: true },
  // The alert tables are sliced by the panel (it owns the rows); the section
  // owns the control beneath them.
  { file: "components/health/GithubAlertsSection.svelte", slices: false },
  { file: "components/health/HostLoadSection.svelte", slices: true },
  { file: "components/health/TerminalIncidentsSection.svelte", slices: true },
  { file: "components/GitHubPanel.svelte", slices: true },
  { file: "components/DeliveryTimeline.svelte", slices: true },
  { file: "components/StoragePanel.svelte", slices: true },
  { file: "components/HygienePanel.svelte", slices: true },
  { file: "components/pulse/PulseKnowledgeMap.svelte", slices: true },
  { file: "components/FleetPulse.svelte", slices: true },
  { file: "components/CoverageViewer.svelte", slices: true },
];

describe("list preview contract", () => {
  it("keeps a navigator fully listed: every worktree in the sidebar stays one click away", () => {
    // The sidebar's worktree list is how you switch worktrees, not a report.
    // Collapsing it to five would hide the sixth, perhaps the current one.
    const source = read("components/WorktreesPanel.svelte");
    expect(source).toContain("{#each worktrees as wt (wt.path)}");
    expect(source).not.toContain("ListPreviewToggle");
  });

  it("is five rows, and every named preview count is that one constant", () => {
    expect(LIST_PREVIEW_ROWS).toBe(5);
    expect(WORKFLOW_PREVIEW_COUNT).toBe(LIST_PREVIEW_ROWS);
    expect(RUN_PREVIEW_COUNT).toBe(LIST_PREVIEW_ROWS);
    expect(TIMELINE_PREVIEW_COUNT).toBe(LIST_PREVIEW_ROWS);
  });

  it("keeps no second Health preview constant or Health-only toggle", () => {
    expect(read("health/sections.ts")).not.toContain("HEALTH_PREVIEW_ROWS");
    expect(existsSync(join(lib, "components/health/HealthPreviewToggle.svelte"))).toBe(false);
    for (const { file } of CONVERTED) {
      const source = read(file);
      expect(source, file).not.toContain("HEALTH_PREVIEW_ROWS");
      expect(source, file).not.toContain("HealthPreviewToggle");
    }
  });

  it("draws the shared control, and slices with the shared helper", () => {
    for (const { file, slices } of CONVERTED) {
      const source = read(file);
      expect(source, file).toMatch(/import ListPreviewToggle from "\.{1,2}\/ListPreviewToggle\.svelte"/);
      expect(source, file).toContain("<ListPreviewToggle");
      if (slices) {
        // The GitHub rail reaches it through `runActions`, which re-exports it.
        expect(source, file).toMatch(
          /import \{[^}]*\bpreviewSlice\b[^}]*\} from "(\.\.\/)+(ui\/previewList|github\/runActions)"/,
        );
        expect(source, file).toContain("previewSlice(");
      }
    }
  });

  it("does not hand-roll an expander beside the shared one", () => {
    // A local button with its own "Show all" label is how two lists came to
    // disagree about what the control does.
    for (const { file } of CONVERTED) {
      const source = read(file);
      expect(source, file).not.toContain("expandLabel(");
      expect(source, file).not.toContain("overflowsPreview(");
      expect(source, file).not.toContain("data-delivery-expand");
    }
  });

  it("marks the shared control with one attribute the harnesses read", () => {
    const toggle = read("components/ListPreviewToggle.svelte");
    expect(toggle).toContain("data-list-preview-toggle");
    expect(toggle).not.toContain("data-health-preview-toggle");
  });
});
