import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

/**
 * Background work has to ask the foreground helper. A local hidden check drifts.
 * The live task surfaces ask it through `liveOnScreen` (`workbench/liveLoader.ts`),
 * which pauses only a hidden window: one on screen but unfocused is where an
 * agent's new task is meant to appear.
 */
const GATES = [
  "stores/repoStore.ts",
  "workbench/client.ts",
  "runtime/adaptiveTimer.ts",
  "async/backgroundScope.ts",
  "components/TaskAgentPanel.svelte",
  "components/TaskManviAssist.svelte",
  "components/AutomaticEnhancements.svelte",
  "components/AttentionInbox.svelte",
  "components/TaskArchive.svelte",
  "components/HygienePanel.svelte",
  "components/TaskBoard.svelte",
  "components/IssueTaskActions.svelte",
] as const;

const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

describe("background work shares one foreground rule", () => {
  it.each(GATES)("%s pauses through the foreground helper", (path) => {
    const source = read(path);
    expect(source.includes("readBackgroundDocument") || source.includes("isBackgroundDocument") || source.includes("liveOnScreen")).toBe(true);
    expect(source).not.toMatch(/document\.hidden/);
    expect(source).not.toMatch(/visibilityState\s*===/);
    expect(source).not.toMatch(/visibilityState\s*!==/);
  });

  it("keeps the responsiveness probe on its own focus tracking", () => {
    const probe = read("diagnostics/responsiveness.ts");
    expect(probe).toContain("hasFocus");
    expect(probe).not.toContain("readBackgroundDocument");
  });

  it("routes the browser host through the shared listener", () => {
    const source = read("runtime/adaptiveTimer.ts");
    expect(source).toContain("addForegroundListener");
    expect(source).toContain('addEventListener("focus"');
    expect(source).toContain('addEventListener("blur"');
  });

  it("polls a live task run through the shared delay", () => {
    const panel = read("components/TaskAgentPanel.svelte");
    expect(panel).toContain("nextTaskRunPollDelay");
    expect(panel).not.toMatch(/setTimeout\([^)]*1500\)/);
  });

  it("live task surfaces pause on a hidden window through the loader's one rule, not on lost focus", () => {
    const liveLoader = read("workbench/liveLoader.ts");
    expect(liveLoader).toMatch(/export function liveOnScreen\(\): boolean \{\n  return !readHiddenDocument\(\);/);
    expect(liveLoader).toContain("const visible = liveOnScreen();");
    // Focus only decides whether a foreground event re-reads (`createLiveReturn`), never whether a surface runs.
    expect(liveLoader).toContain("if (returned(visible)) schedule();");
    for (const path of ["components/AttentionInbox.svelte", "components/TaskArchive.svelte"]) {
      expect(read(path), path).toContain("visible = $state(liveOnScreen())");
    }
  });

  it("drops a debounced workbench refresh while the window is hidden", () => {
    // The board refreshes through the shared live loader, which asks `canRun`
    // both when a refresh is scheduled and when it fires (liveLoader.test.ts).
    const board = read("components/TaskBoard.svelte");
    const loader = board.slice(board.indexOf("const background = createLiveLoader("), board.indexOf("function scheduleRefresh()"));
    expect(loader).toMatch(/canRun: \(\) => [^\n]*liveOnScreen\(\)/);
    expect(loader).not.toContain("readBackgroundDocument");
    expect(board).toMatch(/function scheduleRefresh\(\) \{ background\.schedule\(\); \}/);
    expect(board).toContain("bindForegroundChanges");
    expect(board).not.toContain('window.addEventListener("focus", scheduleRefresh)');

    // Issue import marks issues already on the board; the task list it reads
    // moved with the import actions from Policy into Remote's Open issues.
    const ops = read("components/IssueTaskActions.svelte");
    const tasks = ops.slice(ops.indexOf("const repoTaskLoader = createLiveLoader("), ops.indexOf("async function loadRepoTasks"));
    expect(tasks).toMatch(/canRun: \(\) => liveOnScreen\(\)/);
    expect(tasks).toMatch(/function scheduleRefreshRepoTasks\(\) \{ repoTaskLoader\.schedule\(\); \}/);
    expect(ops).toContain("bindForegroundChanges");
    expect(ops).toContain("changed: scheduleRefreshRepoTasks");
  });
});
