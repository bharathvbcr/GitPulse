import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "TerminalPanel.svelte"), "utf8");
const app = readFileSync(join(here, "..", "..", "App.svelte"), "utf8");

describe("TerminalPanel lifecycle hygiene", () => {
  it("owns no event subscription of its own", () => {
    // `terminal-output`/`terminal-exit` are process-wide events carrying a
    // session id. One listener per tab would decode every chunk once per open
    // tab; ptyBus subscribes once and routes by id, and its own tests cover
    // the late-resolving-listen race this panel used a tracker for.
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

describe("terminal repository boundary", () => {
  it("hosts the dock outside every currentPath key so a repo tab switch cannot kill the shell", () => {
    // Git UI still remounts per repository; the PTY must not. Nested keys
    // would make "the next {/key}" the wrong closer — these keys are
    // sequential on purpose.
    const key = "{#key $repoStore.currentPath}";
    const close = "{/key}";
    const dockIdx = app.indexOf("<TerminalDock");
    expect(dockIdx).toBeGreaterThan(-1);
    expect(app.split(key).length - 1).toBeGreaterThanOrEqual(2);

    let from = 0;
    let keys = 0;
    while (from < app.length) {
      const start = app.indexOf(key, from);
      if (start === -1) break;
      const end = app.indexOf(close, start + key.length);
      expect(end).toBeGreaterThan(start);
      expect(
        dockIdx < start || dockIdx > end,
        "TerminalDock sits inside {#key $repoStore.currentPath}, which remounts and kills the PTY on a tab switch",
      ).toBe(true);
      keys += 1;
      from = end + close.length;
    }
    expect(keys).toBeGreaterThanOrEqual(2);
  });

  it("still remounts the git UI when the repository tab changes", () => {
    // The dock moved out of the key; Sidebar and the view column must stay
    // inside one, or a switch would leak the previous repo's graph/diff into
    // the next worktree.
    expect(app).toMatch(/\{#key \$repoStore\.currentPath\}[\s\S]*<Sidebar/);
    expect(app).toMatch(/\{#key \$repoStore\.currentPath\}[\s\S]*gp-view/);
  });
});
