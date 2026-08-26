import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "TerminalPanel.svelte"),
  "utf8"
);

describe("TerminalPanel PTY lifecycle hygiene", () => {
  it("unwinds listeners that resolve after teardown (no leak on early unmount)", () => {
    expect(source).toContain("createListenerTracker()");
    expect(source).toContain("unlisteners.track(fn)");
  });

  it("disposes unlisteners on cleanup", () => {
    const cleanupIdx = source.indexOf("unlisteners.dispose();");
    expect(cleanupIdx).toBeGreaterThan(-1);
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
