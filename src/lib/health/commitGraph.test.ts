import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import type { CommitGraphReport, CommitGraphState } from "../storage/types";
import { COMMIT_GRAPH_COMMAND, commitGraphHint } from "./commitGraph";

function report(state: CommitGraphState, note: string | null = null): CommitGraphReport {
  return { state, layout: "none", layers: 0, commits: null, bloom: null, head_covered: null, note };
}

describe("commitGraphHint", () => {
  it("names the missing file and the exact command", () => {
    const hint = commitGraphHint(report("missing"));
    expect(hint?.tone).toBe("advice");
    expect(hint?.detail).toContain(".git/objects/info/commit-graph");
    expect(hint?.command).toBe("git commit-graph write --reachable --changed-paths");
    expect(hint?.command).toBe(COMMIT_GRAPH_COMMAND);
  });

  it.each(["stale", "no_bloom"] as const)("advises the same command for %s", (state) => {
    expect(commitGraphHint(report(state))?.command).toBe(COMMIT_GRAPH_COMMAND);
  });

  it.each(["present", "no_history"] as const)("says nothing for %s", (state) => {
    expect(commitGraphHint(report(state))).toBeNull();
  });

  it("says nothing for a payload from before the detector", () => {
    expect(commitGraphHint(undefined)).toBeNull();
    expect(commitGraphHint(null)).toBeNull();
  });

  it("never offers a command when the graph could not be checked", () => {
    const hint = commitGraphHint(report("unknown", "this is a shallow repository"));
    expect(hint?.tone).toBe("unchecked");
    expect(hint?.command).toBeUndefined();
    expect(hint?.detail).toContain("shallow");
    expect(hint?.title.toLowerCase()).not.toContain("no commit-graph");
    expect(commitGraphHint(report("unknown"))?.detail).toBeTruthy();
  });
});

describe("the panel wiring", () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const panel = readFileSync(join(here, "..", "components", "StoragePanel.svelte"), "utf8");

  it("renders the hint as display text and offers no way to run it", () => {
    expect(panel).toContain("commitGraphHint(report?.commit_graph)");
    expect(panel).toContain("commit-graph-command");
    expect(panel).not.toMatch(/commit-graph write|invoke\(["']cmd_commit_graph/);
  });
});
