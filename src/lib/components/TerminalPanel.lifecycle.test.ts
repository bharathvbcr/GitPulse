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
  it("is App's key on the current repository path", () => {
    // The panel no longer carries a repo-keyed teardown effect, so this is
    // the single thing that ends every session on a repo switch. If the key
    // moves inside the dock, shells would survive into the wrong repository
    // and keep running commands against a path the user has left.
    const keyIdx = app.indexOf("{#key $repoStore.currentPath}");
    expect(keyIdx).toBeGreaterThan(-1);
    const dockIdx = app.indexOf("<TerminalDock");
    expect(dockIdx).toBeGreaterThan(keyIdx);
    expect(app.indexOf("{/key}")).toBeGreaterThan(dockIdx);
  });
});
