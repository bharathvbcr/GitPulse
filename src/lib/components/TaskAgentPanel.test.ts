import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// The behaviour lives in pure owners (`terminal/checkoutLabel.ts`,
// `workbench/taskSessions.ts`, `workbench/taskTerminal.ts`), tested there;
// the browser harness `task-runs` drives the pane itself. These pin that the
// pane reaches those owners instead of keeping its own copies.
const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "TaskAgentPanel.svelte"), "utf8");

describe("TaskAgentPanel checkout links", () => {
  it("names an attempt's checkout by the canonical rule, not a private basename", () => {
    // A private `checkoutName` returned the path's last segment, which for an
    // agent worktree is a slug with no repository, and disagreed with the
    // tab strip's `repoFamily.checkoutName`.
    expect(source).not.toMatch(/function checkoutName\(/);
    expect(source).toContain("describeCheckout(run.cwd, $repoStore.openTabs, pathOpts)");
  });

  it("decides a shared checkout from the run and its live peers", () => {
    expect(source).toContain("checkoutChanges(run.cwd, $repoStore.openTabs, pathOpts, { runId: run.id, peers: live })");
  });

  it("offers Open checkout for each attempt, through the checkout-root owner", () => {
    expect(source).toContain("openAttemptCheckout(run)");
    expect(source).toContain('data-testid="agent-open-checkout"');
  });

  it("says where the released worktree stays rather than implying it is gone", () => {
    expect(source).toContain("releasedNote(result.run)");
    expect(source).not.toContain('"Released. The checkout is free for another attempt."');
  });
});
