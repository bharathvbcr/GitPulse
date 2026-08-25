import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "BranchList.svelte"),
  "utf8"
);

describe("BranchList delete escalation", () => {
  it("attempts the safe non-forced delete first", () => {
    const safeIdx = source.indexOf("repoStore.deleteBranch(branch.name, false)");
    expect(safeIdx).toBeGreaterThan(-1);
    const forceIdx = source.indexOf("repoStore.deleteBranch(branch.name, true)");
    expect(forceIdx).toBeGreaterThan(safeIdx);
  });

  it("escalates to force only through an explicit confirm fed by the decision helper", () => {
    const decisionIdx = source.indexOf("escalateDeleteDecision(outcome.error ?? \"\", branch)");
    expect(decisionIdx).toBeGreaterThan(-1);
    const confirmIdx = source.indexOf('title: "Force-delete branch"');
    expect(confirmIdx).toBeGreaterThan(decisionIdx);
    // The retry is gated on both a positive decision and an explicit confirm.
    expect(source).toContain("!decision.canRetryForce || !decision.message");
    expect(source.indexOf("if (!forceOk) return;")).toBeGreaterThan(confirmIdx);
  });

  it("no longer deletes with a bare unconditional force", () => {
    // The only `force=true` call sits after the escalation confirm block.
    const confirmIdx = source.indexOf('title: "Force-delete branch"');
    const forceIdx = source.indexOf("repoStore.deleteBranch(branch.name, true)");
    expect(forceIdx).toBeGreaterThan(confirmIdx);
    expect(source.indexOf("if (!ok) return;")).toBeLessThan(
      source.indexOf("repoStore.deleteBranch(branch.name, false)")
    );
  });
});

describe("BranchList create-form safety", () => {
  it("keeps the typed name when creation fails (F14)", () => {
    const outcomeCheck = source.indexOf("if (!outcome.ok) return;");
    const clear = source.indexOf("createName = \"\";", source.indexOf("async function submitCreate"));
    expect(outcomeCheck).toBeGreaterThan(-1);
    expect(clear).toBeGreaterThan(outcomeCheck);
  });

  it("bails out of suggestName when the repo changed mid-flight (race)", () => {
    const fn = source.slice(source.indexOf("async function suggestName"), source.indexOf("function openBranchMenu"));
    expect(fn).toContain("const repo = $repoStore.currentPath");
    expect(fn.match(/\$repoStore\.currentPath !== repo/g)?.length).toBeGreaterThanOrEqual(2);
  });
});
