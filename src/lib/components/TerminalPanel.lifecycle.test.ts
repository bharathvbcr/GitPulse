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
    // The registration callback must check the disposed flag...
    const disposedDecl = source.indexOf("let listenersDisposed = false;");
    expect(disposedDecl).toBeGreaterThan(-1);
    const thenIdx = source.indexOf(".then((unlistenFns) => {");
    expect(thenIdx).toBeGreaterThan(disposedDecl);
    const guardIdx = source.indexOf("if (listenersDisposed)", thenIdx);
    expect(guardIdx).toBeGreaterThan(-1);
    // ...and the guard must sit BEFORE the push that would orphan the fns.
    const pushIdx = source.indexOf("unlisteners.push(...unlistenFns)");
    expect(pushIdx).toBeGreaterThan(guardIdx);
  });

  it("sets the disposed flag in the teardown path", () => {
    const cleanupIdx = source.indexOf("listenersDisposed = true;");
    expect(cleanupIdx).toBeGreaterThan(-1);
    const spliceIdx = source.indexOf("unlisteners.splice(0)");
    expect(spliceIdx).toBeGreaterThan(cleanupIdx);
  });

  it("clears a pending copy-reset timer on teardown and before re-arming", () => {
    expect(source).toContain("if (copyResetTimer !== null) clearTimeout(copyResetTimer);");
    // Teardown clears too: both occurrences live inside the component.
    expect(source.match(/clearTimeout\(copyResetTimer\)/g)?.length).toBeGreaterThanOrEqual(2);
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
