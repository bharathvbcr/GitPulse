import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";

import CodeStackViewer from "./CodeStackViewer.svelte";
import { VIEW_REGISTRY } from "../views/viewRegistry";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "CodeStackViewer.svelte"),
  "utf8"
);

describe("CodeStackViewer restack safety", () => {
  it("latches exactly one restack in flight and refuses re-entry", () => {
    // A frontend guard cancel() abandons the await but cannot kill the
    // backend git rebase; only a latch prevents queueing a second one.
    expect(source).toContain("let restackingKey = $state<string | null>(null);");
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("$effect"));
    expect(fn).toContain("if (restackingKey !== null) return;");
    expect(fn).toContain("restackingKey = node.branch_name;");
  });

  it("releases the restack latch on every exit path", () => {
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("$effect"));
    // finally-release, not success-only release.
    expect(fn).toContain("} finally {");
    expect(fn).toContain("restackingKey = null;");
  });

  it("clears the previous restack error before starting a new attempt", () => {
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("$effect"));
    const clearIdx = fn.indexOf("restackError = null;");
    const tryIdx = fn.indexOf("try {");
    expect(clearIdx).toBeGreaterThan(-1);
    expect(tryIdx).toBeGreaterThan(clearIdx);
  });

  it("disables row actions while any restack is in flight", () => {
    expect(source.match(/disabled={restackingKey !== null}/g)?.length).toBeGreaterThanOrEqual(2);
    expect(source).toMatch(/disabled=\{isLoading \|\| restackingKey !== null\}/);
  });

  it("records the harness verdict and journals the action", () => {
    // README contract: mutating verdicts are recorded centrally and surface
    // in the header badge; restack used to bypass both.
    expect(source).toContain('from "../stores/harnessStore"');
    expect(source).toContain("harnessStore.recordVerdict(result.policy, repoPath)");
    expect(source).toMatch(/harnessStore\.recordAction\(\{\s+repoPath,/);
    expect(source).toContain('kind: "restack"');
  });

  it("journals the settled restack before returning stale UI work", () => {
    // Each rebase settles inside runStep, which journals on both paths and
    // holds no liveness check of its own — so a superseded cascade cannot
    // return past a rebase that really ran and leave it out of the journal.
    const body = source.slice(source.indexOf("async function runStep"), source.lastIndexOf("</script>"));
    const step = body.slice(0, body.indexOf("\n  }"));
    expect(step).not.toContain("guard.isLive()");
    const settled = step.indexOf('await invoke("cmd_restack"');
    const successJournal = step.indexOf("harnessStore.recordAction", settled);
    expect(successJournal).toBeGreaterThan(settled);
    const caught = step.indexOf("} catch", successJournal);
    const failureJournal = step.indexOf("harnessStore.recordAction", caught);
    expect(failureJournal).toBeGreaterThan(caught);
    // A failed step still ends the cascade: the error travels up rather than
    // letting the next branch rebase onto a parent that did not move.
    expect(step.slice(failureJournal)).toContain("throw err;");
  });

  it("keeps every rejection routed through the diagnostics reporting seam", () => {
    // Both catches (load + restack) report through reportPanelError, which
    // formats like formatError and additionally feeds the diagnostics ring.
    expect(source).toContain('from "../diagnostics/report"');
    expect((source.match(/reportPanelError\("stack", err\)/g) ?? []).length).toBe(2);
    expect(source).not.toContain('from "../ui/formatError"');
    expect(source).not.toMatch(/String\(\s*(err|reason|error|e)\s*\)/);
  });
});

describe("CodeStackViewer load correctness", () => {
  it("derives nothing from a frontend branch guess: no defaultStackRoot", () => {
    expect(source).not.toContain("defaultStackRoot");
    // The payload carries the backend-resolved root.
    expect(source).toContain(".default_branch");
  });

  it("tracks repo path AND generation so watcher refreshes reload the stack", () => {
    // Two script blocks exist (module cache + instance); slice the
    // instance block by its closing tag.
    const effectStart = source.indexOf("$effect(() => {\n    const repo");
    const effect = source.slice(effectStart, source.lastIndexOf("</script>"));
    expect(effect).toContain("$repoStore.currentPath");
    expect(effect).toContain("$repoStore.generation");
    // Memo guard: unrelated store emissions must not refetch.
    expect(effect).toContain("generation === prevGeneration) return;");
  });

  it("shows a loading state instead of a false empty state on first fetch", () => {
    expect(source).toContain("{:else if isLoading}");
  });

  it("keys each stack row", () => {
    expect(source).toContain("{#each rows as row (row.node.branch_name)}");
  });
});

describe("CodeStackViewer accessibility", () => {
  it("announces error banners", () => {
    expect((source.match(/role="alert"/g) ?? []).length).toBe(2);
  });

  it("keeps banner text readable in light theme", () => {
    expect(source).toContain("text-rose-700 dark:text-rose-300");
    expect(source).not.toContain("text-rose-300 {");
  });

  it("marks the busy row for assistive tech", () => {
    expect(source).toContain("aria-busy={restackingKey === node.branch_name}");
  });

  it("renders without a repository (SSR smoke)", () => {
    const { body } = render(CodeStackViewer);
    expect(body).toContain("Stack");
  });

  it("titles the pane the way the section that opens it is labelled", () => {
    const section = VIEW_REGISTRY.work.sections?.find((s) => s.id === "stack");
    expect(section?.label).toBe("Stack");
    expect(source).toContain(`\n        ${section?.label}\n`);
  });
});

describe("CodeStackViewer cascade contracts", () => {
  it("plans the whole subtree from the snapshot on screen, before anything moves", () => {
    // Once a parent is rebased the commit its children were cut from is no
    // longer reachable from it, so the fork points have to be read before the
    // first rewrite — which is the tree the reader was looking at.
    expect(source).toContain("cascadePlan(stackNodes, node.branch_name)");
    expect(source).toContain("forkPoint: step.forkPoint");
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("async function runStep"));
    const plan = fn.indexOf("cascadePlan(");
    const firstInvoke = fn.indexOf("runStep(");
    expect(plan).toBeGreaterThan(-1);
    expect(firstInvoke).toBeGreaterThan(plan);
  });

  it("names every branch a rewrite would touch before running it", () => {
    // "Restack 4 branches" does not tell the reader which four, and this
    // rewrites commits on all of them.
    expect(source).toContain("askConfirm(");
    expect(source).toContain("describeCascade(steps)");
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("async function runStep"));
    expect(fn.indexOf("askConfirm(")).toBeLessThan(fn.indexOf("runStep("));
  });

  it("reports what moved when a cascade stops part-way", () => {
    // Two of four branches rebased and then a failure leaves a repository the
    // reader has to know about; the error alone describes it as if nothing
    // had happened.
    expect(source).toContain("Rebased: ${done.join(\", \")}");
    expect(source).toContain("Still on their old base:");
    expect(source).toContain("No branch was rebased.");
  });

  it("does not erase the partial-cascade report with the reload it triggers", () => {
    // Caught in the real UI: the failure path sets the banner and then calls
    // loadStack, which cleared it — so a half-rebased repository showed a
    // screen saying nothing had happened. Clearing belongs to the next
    // attempt, which does it before its first await; a watcher tick is not
    // an attempt.
    const load = source.slice(source.indexOf("async function loadStack"), source.indexOf("async function restack"));
    expect(load).not.toContain("restackError = null");
    const attempt = source.slice(source.indexOf("async function restack"), source.indexOf("async function runStep"));
    expect(attempt.indexOf("restackError = null;")).toBeLessThan(attempt.indexOf("await runStep("));
  });

  it("reloads the tree once any branch has moved", () => {
    // A stale tree is what a second click would plan its fork points from.
    const fn = source.slice(source.indexOf("async function restack"), source.indexOf("async function runStep"));
    const caught = fn.indexOf("} catch");
    expect(fn.indexOf("if (done.length > 0)", caught)).toBeGreaterThan(caught);
  });

  it("draws the hierarchy as a tree rather than a flat list of parent names", () => {
    expect(source).toContain("stackTreeRows(stackNodes)");
    expect(source).toContain("row.depth");
  });

  it("says what the hierarchy cannot see, and lists what it placed nowhere", () => {
    // A branch left behind by a rebase of its parent stops being a child and
    // reappears as its own root. Without saying so, a stack that fell apart
    // reads as a repository that never had one.
    expect(source).toContain("rootlessBranches(stackNodes");
    expect(source).toContain("On no stack (");
    expect(source).toMatch(/only while it sits on its parent's current tip/);
  });
});
