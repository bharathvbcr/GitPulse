import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { stopChildrenQuestion } from "./stopChildren";

describe("stopChildrenQuestion", () => {
  it("names a single child and its count", () => {
    const q = stopChildrenQuestion(["Claude 2"]);
    expect(q.title).toBe("Stop 1 child session?");
    expect(q.message).toContain("Claude 2 is still running");
  });

  it("names every child when there are a few", () => {
    const q = stopChildrenQuestion(["Claude 2", "Codex 3", "Shell 4"]);
    expect(q.title).toBe("Stop 3 child sessions?");
    expect(q.message).toContain("Claude 2, Codex 3, Shell 4 are still running");
  });

  it("bounds a long list and still carries the full count", () => {
    const labels = Array.from({ length: 12 }, (_, i) => `Child ${i + 1}`);
    const q = stopChildrenQuestion(labels);
    expect(q.title).toBe("Stop 12 child sessions?");
    expect(q.message).toContain("Child 8, and 4 more");
    expect(q.message).not.toContain("Child 9");
  });

  it("does not print an empty name", () => {
    expect(stopChildrenQuestion(["  "]).message).toContain("Untitled session");
  });
});

describe("Stop children call site", () => {
  const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../components/TerminalPanel.svelte"), "utf8");
  const body = source.slice(source.indexOf("async function stopActiveChildren()"), source.indexOf("function handOffToPairPartner("));

  it("asks, destructively, before closing any child, and stops when declined", () => {
    const ask = body.indexOf("askConfirm(");
    const close = body.indexOf(".close()");
    expect(ask).toBeGreaterThan(-1);
    expect(close).toBeGreaterThan(ask);
    expect(body).toContain("destructive: true");
    expect(body).toContain("stopChildrenQuestion(");
    expect(body).toMatch(/if \(!\(await askConfirm\([^\n]*\)\)\) return;/);
  });
});
