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
    // The distinction is still consulted and still changes what the reader
    // sees — it drives the header subtitle and the catch-all row's label. It
    // no longer produces a dead-end empty state naming DevCouncil, because
    // most readers do not run one and that message gave them nothing to do.
    expect(source).toContain("sources.tasks.present");
    expect(source).toContain("hasTasks");
  });

  it("never sends a reader to a system they may not run", () => {
    // The empty state used to read "This repository has no DevCouncil task
    // store, so there is no task model to project" — accurate, and useless:
    // it named a system the reader has no way to reach from here, on a screen
    // that was otherwise blank.
    // Comments are stripped first: the property is about text a reader sees,
    // and this file's comments legitimately discuss DevCouncil when explaining
    // why a column is hidden from people who do not run one.
    const rendered = source.replace(/<!--[\s\S]*?-->/g, "");
    const emptyState = rendered.slice(rendered.indexOf("Nothing in flight"));
    expect(emptyState).not.toContain("DevCouncil");
  });

  it("keys rows on something that exists in every repository", () => {
    // Keying on task id collapsed a repository with no task store into a
    // single row labelled "Not bound to a task". Rows are keyed on `key`,
    // which is the worktree path when there is no task model.
    expect(source).toContain("projection.rows as row (row.key");
    expect(source).not.toContain("as row (row.taskId");
  });

  it("surfaces a parked operation, which is the one thing that needs a person", () => {
    expect(source).toContain("row.operation");
    expect(source).toContain("headline(row.operation)");
  });

  it("lets the reader act on a row rather than only read it", () => {
    // A row you cannot open is a report. The whole value of showing that a
    // worktree is stuck is being one click from the view that unsticks it.
    expect(source).toContain("openWorktree");
    expect(source).toContain("repoStore.openRepo");
  });
});
