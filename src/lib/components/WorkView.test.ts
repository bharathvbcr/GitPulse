import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import WorkView from "./WorkView.svelte";
import { ALL_STATUSES } from "../work/projection";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "WorkView.svelte"),
  "utf8",
);

describe("WorkView", () => {
  it("renders its header without a repository open", () => {
    const { body } = render(WorkView);
    expect(body).toContain("Work");
    expect(body).toContain("No repository open");
  });

  it("hydrates the cached projection before refetching on remount", () => {
    expect(source).toContain("createRepoPanelCache<WorkProjection>()");
    expect(source).toContain("workCache.set(repo, result);");
    expect(source).toContain("workCache.get(repo)");
  });

  /**
   * The tone map is a `Record<PolicyStatus, string>`, so a status added to the
   * union without a colour is a compile error rather than a chip that renders
   * as the default. This asserts the map is actually exhaustive today — a
   * `Record` with a stale key set would still typecheck if the union shrank.
   */
  it("gives every policy status a tone", () => {
    for (const status of ALL_STATUSES) {
      expect(source, `${status} has no tone`).toContain(`${status}:`);
    }
  });

  it("shows unreadable verdicts as their own chip, never as allowed", () => {
    // The failure this guards is the one the whole view exists to avoid: a
    // verdict the ledger recorded and this build could not parse, counted as
    // a clean pass.
    expect(source).toContain("row.verdicts.unparsed > 0");
    expect(source).toContain("unreadable");
    expect(source).not.toContain('?? "allowed"');
  });

  it("states an incomplete screen above the rows, not below them", () => {
    const warning = source.indexOf("degradedSummary");
    const rows = source.indexOf("{#each projection.rows");
    expect(warning).toBeGreaterThan(-1);
    expect(rows).toBeGreaterThan(-1);
    expect(source.indexOf("{degraded}")).toBeLessThan(rows);
  });

  it("tells an absent task store apart from a task store with nothing in it", () => {
    expect(source).toContain("projection.sources.tasks.present");
    expect(source).toContain("no DevCouncil task store");
  });
});
