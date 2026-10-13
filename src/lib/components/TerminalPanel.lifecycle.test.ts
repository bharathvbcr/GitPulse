import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TerminalPanel.svelte"), "utf8");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");

describe("TerminalPanel lifecycle hygiene", () => {
  it("owns no event subscription of its own", () => {
    // `terminal-exit` and `terminal-loss` are process-wide events carrying a
    // session id (output travels on each session's own channel). One listener
    // per tab would inspect every event once per open tab; ptyBus subscribes
    // once and routes by id, and its own tests cover the late-resolving-listen
    // race this panel used a tracker for.
    expect(source).not.toContain('from "@tauri-apps/api/event"');
    expect(source).not.toContain("createListenerTracker");
  });

  it("clears a pending copy-reset timer on teardown and before re-arming", () => {
    expect(source).toContain("if (copiedResetTimer !== null) clearTimeout(copiedResetTimer);");
    // Teardown clears too: both occurrences live inside the component.
    expect(source.match(/clearTimeout\(copiedResetTimer\)/g)?.length).toBeGreaterThanOrEqual(2);
  });

  it("guards command-input keys against IME composition", () => {
    const importIdx = source.indexOf('from "../keyboard/imeGuard"');
    expect(importIdx).toBeGreaterThan(-1);
    const handlerIdx = source.indexOf("function handleKeyDown");
    expect(handlerIdx).toBeGreaterThan(-1);
    const guardIdx = source.indexOf("isImeComposition(e)", handlerIdx);
    expect(guardIdx).toBeGreaterThan(-1);
    // The guard runs before any Enter/Arrow handling.
    const enterIdx = source.indexOf('e.key === "Enter"', handlerIdx);
    expect(enterIdx).toBeGreaterThan(guardIdx);
  });
});

const read = (name: string) => readFileSync(join(here, name), "utf8");
const panel = read("ViewSectionPanel.svelte");
const VIEWS = ["WorkspaceView", "CodeView", "HistoryView", "InsightsView"];
const KEY = "{#key $repoStore.currentPath}";
const keyCount = (text: string) => text.split(KEY).length - 1;

describe("terminal repository boundary", () => {
  it("hosts the dock outside every currentPath key so a repo tab switch cannot kill the shell", () => {
    // The dock is hosted by App.svelte, which holds no per-repository key at
    // all, and none of the views whose panels ARE keyed render a dock.
    expect(app).toContain("<TerminalDock");
    expect(keyCount(app), "App.svelte keys something on currentPath").toBe(0);
    for (const view of VIEWS) {
      expect(read(`${view}.svelte`), `${view} hosts the dock inside a keyed panel`).not.toContain("TerminalDock");
    }
    expect(panel).not.toContain("TerminalDock");
  });

  it("does not rebuild the shell when the repository tab changes", () => {
    // Sidebar, the workspace column and the status bar used to sit inside
    // three `{#key $repoStore.currentPath}` blocks, so every tab switch tore
    // down and rebuilt all of them. Each now resets its own per-repository
    // state in place; the harness that mounts the real App covers behaviour.
    expect(app).toContain("<Sidebar");
    expect(app).toContain("<StatusBar");
    expect(app).toContain("gp-view");
    expect(keyCount(app)).toBe(0);
    // A stateful component that survives the switch must say how it resets.
    for (const name of ["Sidebar", "BranchList", "CommitComposer", "AgentCommitMenu", "WorktreesPanel", "LanguageSegment"]) {
      expect(read(`${name}.svelte`), `${name} outlives a tab switch without a repoScope reset`).toContain("repoScope(");
    }
  });

  it("keys the view leaves per repository inside the panel, in one place", () => {
    // What a panel holds (a diff, a file tree, a commit table) keeps scroll
    // offsets, open tabs and drafts that belong to one repository. Until a
    // leaf is untangled it is rebuilt on a switch, so nothing leaks into the
    // next worktree. ViewSectionPanel is the single owner of that key.
    expect(keyCount(panel)).toBe(1);
    expect(panel).toMatch(/\{#key \$repoStore\.currentPath\}\s*\{@render children\(\)\}\s*\{\/key\}/);
    for (const view of VIEWS) {
      expect(read(`${view}.svelte`), `${view} does not render its content in a ViewSectionPanel`).toContain("<ViewSectionPanel");
      expect(keyCount(read(`${view}.svelte`)), `${view} keys on its own`).toBe(0);
    }
  });
});
