import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

// The behaviour lives in pure owners (`terminal/checkoutLabel.ts`,
// `workbench/taskSessions.ts`, `workbench/taskTerminal.ts`), tested there;
// the browser harness `task-runs` drives the pane itself. These pin that the
// pane reaches those owners instead of keeping its own copies.
const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "TaskAgentPanel.svelte"), "utf8");

const boardSource = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "TaskBoard.svelte"), "utf8");

// ADVERSARIAL (L1, ft-67f95e2d item 3): `asksForReader` reads a module flag that
// Svelte cannot track. A `$derived` that reaches it through a pure function is
// not recomputed when the reader flips the setting, so the pane's count and the
// board's chip keep the old answer until the next poll or activity tick, while
// the tray (whose store lists the interface store as an input) updates at once.
describe("the bell setting reaches the pane and the board without a reload", () => {
  it("the pane's asking count depends on the setting, so a toggle recomputes it", () => {
    const asking = /const askingCount = \$derived\([^;]*\);/.exec(source)?.[0] ?? "";
    expect(asking).toContain("bellAsksForReader");
  });

  it("the board's per-task agent summaries depend on the setting, so a toggle recomputes them", () => {
    const start = boardSource.indexOf("const agentsByTask = $derived.by");
    expect(start).toBeGreaterThan(-1);
    const derivation = boardSource.slice(start, boardSource.indexOf("\n  });", start));
    expect(derivation).toContain("bellAsksForReader");
  });
});

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
