import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { showsWorkspaceControls } from "./WorkspaceActions.svelte";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "WorkspaceActions.svelte"), "utf8");

describe("showsWorkspaceControls", () => {
  it("hides the controls until a second repository is open", () => {
    // With one tab, "fetch all" is just "fetch" and the roll-up repeats what
    // the tab already says; the controls would be pure noise.
    expect(showsWorkspaceControls(0)).toBe(false);
    expect(showsWorkspaceControls(1)).toBe(false);
  });

  it("shows them from the second repository onward", () => {
    expect(showsWorkspaceControls(2)).toBe(true);
    expect(showsWorkspaceControls(24)).toBe(true);
  });

  it("does not let the action cluster shrink off the right edge", () => {
    // The tab strip shrinks. These controls are pinned after it, so a narrow
    // window must scroll the tabs rather than crush Fetch and Work.
    expect(source).toContain('class="flex shrink-0 items-center gap-1"');
  });
});

describe("the work-in-progress roll-up panel", () => {
  it("makes each listed repository a control, not a caption", () => {
    // The defect: the panel named the repositories holding work and then left
    // the reader to find them in the tab strip by hand. Every row is a button
    // that activates its repository now, so the list is the door to what it
    // reports.
    expect(source).toContain("function reveal(path: string)");
    expect(source).toContain("onclick={() => reveal(repo.path)}");
    // Rows are open tabs: activate rather than reopen, and fall back to
    // openRepo only for a tab closed between render and click.
    expect(source).toContain("repoStore.activateTab(open.id)");
    expect(source).toContain("repoStore.openRepo(path)");
  });

  it("lands on the pane that answers the row's worst reason", () => {
    // The destination comes from the tested model, not from a second copy of
    // the mapping written into the markup.
    expect(source).toContain("wipDestination(wip.repos.find((repo) => repo.path === path)?.severity");
    expect(source).toContain("repoStore.setActiveTab(tab, section)");
    // …and the tooltip names that pane from the registry, so a renamed
    // section renames the promise too.
    expect(source).toContain("describeDestination(to.tab, to.section)");
  });

  it("makes the sweep report's repositories reachable too", () => {
    // Same defect, same panel: a row saying a fetch was skipped here names
    // exactly the repository the reader now wants to open. Both lists route
    // through one function, so neither can drift from the other.
    expect(source).toContain("onclick={() => reveal(result.path)}");
    expect(source).toContain("destinationFor(result.path)");
    // Live state decides the pane, not which list drew the row: a repository
    // the sweep skipped for conflicts opens the resolver.
    expect(source).toContain("function destinationFor(path: string)");
  });

  it("leaves Fleet before activating a tab underneath it", () => {
    // The tab strip stays on screen over Fleet, so activating a repository
    // without closing Fleet looks exactly like the inert row this replaced.
    expect(source).toContain("interfaceStore.setFleetOpen(false)");
  });

  it("dismisses the way every other overlay in the app does", () => {
    // Literally the same way now: the shared popover owner registers the
    // listeners, and because it lives on the panel node rather than on the
    // component, teardown IS the close — the pairing this used to assert by
    // hand cannot come apart. `popover.test.ts` proves that; what stays this
    // component's own contract is what it declares.
    expect(source).toContain("use:popover={dismissal}");
    // The trigger has to count as inside, or its own pointerdown would close
    // the panel a beat before its click reopened it.
    expect(source).toContain("[data-workspace-wip], [data-workspace-wip-trigger]");
    expect(source).toContain("data-workspace-wip-trigger");
    expect(source).toContain('escape: "bubble"');
    expect(source).toContain('aria-haspopup="dialog"');
    // Replaced, not accumulated: no second, hand-rolled dismissal beside it.
    expect(source).not.toContain("addEventListener");
  });

  it("closes from one red control at the top, not a button below the content", () => {
    // The panel grows with the sweep report, so a full-width Close at the
    // bottom was the first thing to scroll out of reach.
    expect(source).toContain('aria-label="Close workspace status"');
    expect(source).toMatch(/class="gp-icon-btn shrink-0"\s+data-size="sm"\s+data-tone="danger"\s+onclick=\{\(\) => \(detailsOpen = false\)\}/);
    expect(source).toContain('title="Close (Esc)"');
    // Replaced, not accumulated: the old bottom button is gone.
    expect(source).not.toContain('class="gp-btn mt-2 py-1! px-2! text-[11px]! w-full"');
  });

  it("styles the trigger as a toned chip, not a bare transparent element", () => {
    // The trigger sits beside 'Fetch all' in the header. Bare transparent
    // styling made it read as ghost text over the glass titlebar. It now wears
    // gp-chip, whose data-tone (ok when clear, warn when work is held) supplies
    // the tint, so no `!` override has to fight gp-btn's own background.
    expect(source).toMatch(/data-workspace-wip-trigger[\s\S]*?class="[^"]*\bgp-chip\b/);
    expect(source).toContain('data-tone={wip.allClear ? "ok" : "warn"}');
    expect(source).not.toMatch(/bg-warn\/15!|bg-surfaceHover!/);
    expect(source).not.toContain(
      'class="inline-flex items-center gap-1 rounded px-1.5 py-1 text-[11px] transition-colors',
    );
  });

  it("styles the work-in-progress dropdown as a floating gp-menu with the float shadow", () => {
    // The dropdown floats over content (code viewer, diffs, commit graph)
    // without a scrim. gp-menu is the app's floating-panel surface (solid
    // background, pop shadow), and shadow-float hooks it into the macOS float
    // tier blur. The hand-built `bg-surface/95 rounded-xl border` card is gone.
    expect(source).toMatch(/data-workspace-wip[\s\S]*?class="[^"]*\bgp-menu\b[^"]*\bshadow-float\b/);
    expect(source).not.toMatch(/data-workspace-wip[\s\S]*?class="[^"]*\bgp-card\b[^"]*"/);
    expect(source).not.toContain("bg-surface/95");
    expect(source).not.toMatch(/rounded-(xl|lg)\b/);
    expect(source).not.toMatch(/border border-edge/);
  });
});

describe("a fetch-all sweep", () => {
  it("shows a progress ring and Stop inside its own pill while it runs", () => {
    expect(source).toMatch(/<svg[\s\S]*?role="progressbar"/);
    expect(source).toContain('aria-valuemin="0"');
    expect(source).toContain("aria-valuemax={progress.total}");
    expect(source).toContain("aria-valuenow={progress.done}");
    expect(source).toContain('const runLabel = $derived(running === "pull" ? "Pulling repositories" : "Fetching repositories");');
    expect(source).toContain('stroke="currentColor"');
    expect(source).toContain("stroke-dasharray={ringDash}");
    // Stop lives inside the pill, which replaces the button while running.
    expect(source).toMatch(/\{#if running && progress\}[\s\S]*?data-tone="accent"[\s\S]*?Stop[\s\S]*?\{:else\}[\s\S]*?Fetch all/);
    // The dash is set per update: no CSS transition on the ring, and no hex colour.
    expect(source).not.toMatch(/transition/);
    expect(source).not.toMatch(/#[0-9a-fA-F]{3,8}\b/);
  });

  it("sizes the ring dash from done over total", () => {
    expect(source).toContain("(progress.done / progress.total) * RING_CIRCUMFERENCE");
    expect(source).toContain("progress && progress.total > 0");
  });

  it("reports in the workspace status panel, not a toast", () => {
    expect(source).not.toContain("toastStore");
    expect(source).not.toContain("toast");
    // Success and partial sweeps alike land in the panel, led by summarizeRun.
    expect(source).toContain('sweepLine = summarizeRun(result, kind === "fetch" ? "Fetched" : "Pulled");');
    expect(source).toContain("detailsOpen = true;");
    expect(source).toMatch(/\{sweepLine \?\? describeWorkspace\(wip\)\}/);
  });

  it("still names every failure and skip in the panel, and announces the summary", () => {
    expect(source).toContain("report.results.filter((r) => r.status !== \"ok\")");
    expect(source).toContain("{result.error ?? result.reason}");
    expect(source).toContain('data-tone={result.status === \'failed\' ? \'danger\' : \'warn\'}');
    expect(source).toMatch(/<span class="sr-only" role="status" aria-live="polite" aria-atomic="true">\{sweepLine \?\? ""\}<\/span>/);
  });

  it("collapses the Fetch all and status labels to icons below 1100px, keeping their names", () => {
    expect(source.match(/max-\[1099px\]:sr-only/g)?.length).toBeGreaterThanOrEqual(3);
    expect(source).toContain('aria-label="Fetch all"');
    expect(source).toContain('aria-label={wip.allClear ? "All clean" : `${wip.repos.length} with work`}');
    expect(source).toContain('title="Fetch every open repository.');
  });

  it("keeps the panel on the menu layer", () => {
    expect(source).toContain('style="z-index: {LAYERS.MENU}"');
  });
});
