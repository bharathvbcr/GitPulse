import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { compile } from "svelte/compiler";
import ScanProposals from "./ScanProposals.svelte";
import TaskRefine from "./TaskRefine.svelte";
import { refineResult, scanView } from "../workbench/proposals";
import { proposal, refine, task, view } from "../workbench/proposals.fixtures";
import type { Task } from "../workbench/client";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
const here = dirname(fileURLToPath(import.meta.url));
const drawer = (initial: unknown) => render(ScanProposals, { props: { repositoryPath: "/r", startOpen: true, initial: initial === null ? null : scanView(initial) } }).body;

describe("ScanProposals renders", () => {
  it("lists a proposal with both of its signals and offers Accept, Edit and Dismiss", () => {
    const body = drawer(view());
    expect(body).toContain("Remove quuxify");
    expect(body).toContain("devmap dead_symbols");
    expect(body).toContain("dcgrep -uu");
    for (const id of ["proposal-accept", "proposal-edit", "proposal-dismiss"]) expect(body).toContain(id);
    expect(body).not.toContain("Partial result");
  });
  it("a capped, partial scan says so with both numbers and never reads as clean", () => {
    const partial = drawer(view({ complete: false, over_cap: 17, coverage: [{ category: "dead_symbols", status: "truncated", detail: "60 of 140 checked", hidden: 80 }] }));
    expect(partial).toContain("Partial result");
    expect(partial).toContain("1 of 18 new proposals");
    expect(partial).toContain("60 of 140 checked");
    const empty = drawer(view({ complete: false, proposals: [], new_count: 0, coverage: [{ category: "dead_symbols", status: "degraded" }] }));
    expect(empty).toContain("not a clean result");
    expect(empty).not.toContain("No gaps found");
    expect(drawer(view({ proposals: [], new_count: 0 }))).toContain("No gaps found");
  });
  it("a proposal the board already holds is shown as held, with nothing to file", () => {
    const body = drawer(view({ proposals: [{ ...proposal, status: "attached", attached_item_id: "ft-1", attached_item_title: "Existing card" }] }));
    expect(body).toContain("Existing card");
    expect(body).toContain("Nothing to file");
    expect(body).not.toContain("proposal-accept");
  });
  it("is closed until opened and offers no scan without an open repository", () => {
    expect(render(ScanProposals, { props: { repositoryPath: "/r" } }).body).not.toContain("proposals-drawer");
    expect(render(ScanProposals, { props: { repositoryPath: "" } }).body).not.toContain("proposals-open");
  });
});

describe("TaskRefine renders", () => {
  const shown = (over: Record<string, unknown> = {}, t: Task = task) => render(TaskRefine, { props: { task: t, repositoryPath: "/r", onApplied: () => {}, initial: refineResult(refine(over)) } }).body;
  it("shows criteria as additions, already-present ones as such, and applies nothing by itself", () => {
    const body = shown();
    expect(body).toContain("+ Exports CSV");
    expect(body).toContain("already on the task");
    expect(body).toContain("Apply selected");
    expect(body).toContain("Indexer");
    // Criteria start chosen; splits create tasks and duplicates link one, so they wait for a choice.
    expect(body).toMatch(/data-testid="task-refine-part-acceptance_criteria"[^>]*checked/);
    expect(body).not.toMatch(/data-testid="task-refine-part-split_into"[^>]*checked/);
    expect(body).not.toMatch(/data-testid="task-refine-part-duplicate_of"[^>]*checked/);
    expect(body).toContain("were dropped");
  });
  it("offers 'apply anyway' only after the host held a split, and never before", () => {
    const body = shown();
    expect(body).not.toContain("task-refine-related");
    expect(readFileSync(join(here, "TaskRefine.svelte"), "utf8")).toMatch(/task-refine-apply-anyway[\s\S]*apply\(\[/);
  });
  it("a task that moved since the proposal disables Apply and says to ask again", () => {
    const body = shown({}, { ...task, revision: 5 });
    expect(body).toContain("task-refine-stale");
    expect(body).toMatch(/data-testid="task-refine-apply"[^>]*disabled/);
  });
  it("an ungrounded proposal is not presented as grounded", () => {
    const body = shown({ grounding: { status: "unavailable", reason: "the index is not built", index_fresh: false, symbols: [] } });
    expect(body).toContain("Not grounded in the code: the index is not built");
    expect(body).not.toContain("Grounded in the code index");
  });
  it("unsaved edits disable asking", () => {
    expect(render(TaskRefine, { props: { task, repositoryPath: "/r", disabled: true, onApplied: () => {} } }).body).toMatch(/data-testid="task-refine-ask"[^>]*disabled/);
  });
});

describe("the new components compile cleanly and are mounted without touching the board", () => {
  it.each(["ScanProposals.svelte", "TaskRefine.svelte"])("%s compiles", (file) => {
    const { warnings } = compile(readFileSync(join(here, file), "utf8"), { generate: "client", filename: file });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });
  it("RepositoryTasks hosts the dock beside the board and TaskEditor hosts refine in the assist section", () => {
    expect(readFileSync(join(here, "RepositoryTasks.svelte"), "utf8")).toContain("<ScanProposals");
    expect(readFileSync(join(here, "TaskEditor.svelte"), "utf8")).toContain("<TaskRefine");
  });
});
