import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";

import CodeStackViewer from "./CodeStackViewer.svelte";

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
    const body = source.slice(source.indexOf("async function restack"), source.indexOf("$effect"));
    const settled = body.indexOf('await invoke("cmd_restack"');
    const successJournal = body.indexOf("harnessStore.recordAction", settled);
    const successGuard = body.indexOf("if (!guard.isLive()", settled);
    expect(successJournal).toBeGreaterThan(settled);
    expect(successJournal).toBeLessThan(successGuard);

    const caught = body.indexOf("} catch", successGuard);
    const failureJournal = body.indexOf("harnessStore.recordAction", caught);
    const failureGuard = body.indexOf("if (!guard.isLive()", caught);
    expect(failureJournal).toBeGreaterThan(caught);
    expect(failureJournal).toBeLessThan(failureGuard);
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
    expect(source).toContain("{#each stackNodes as node (node.branch_name)}");
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
    expect(body).toContain("Code Stack Hierarchy");
  });
});
