import { describe, expect, it } from "vitest";
import { identityKey } from "../repos/paths";
import { MAX_AGENT_CWD_READS, agentCwdTargets, nearestContaining } from "./cwd";

const rec = (sessionId: string | undefined, label: string, fields: Record<string, unknown> = {}) =>
  ({ sessionId, label, ...fields });

describe("agentCwdTargets", () => {
  it("puts agents ahead of shells, so shells only fill what the cap leaves", () => {
    const shells = Array.from({ length: MAX_AGENT_CWD_READS }, (_, index) => rec(`sh-${index}`, "Shell"));
    const ids = agentCwdTargets([...shells, rec("claude-1", "Claude"), rec(undefined, "Codex"), rec("codex-1", "Codex")]);
    expect(ids.slice(0, 2)).toEqual(["claude-1", "codex-1"]);
    expect(ids).toHaveLength(MAX_AGENT_CWD_READS);
    expect(ids.filter((id) => id.startsWith("sh-"))).toHaveLength(MAX_AGENT_CWD_READS - 2);
  });

  it("does not change when only a title, a status or the order changes", () => {
    const before = agentCwdTargets([rec("a", "Claude", { title: "one", status: "starting" }), rec("b", "Shell")]);
    const after = agentCwdTargets([rec("b", "Shell", { status: "exited" }), rec("a", "Claude", { title: "two", status: "running" })]);
    expect(after).toEqual(before);
  });

  it("reads one session once", () => {
    expect(agentCwdTargets([rec(" a ", "Claude"), rec("a", "Claude"), rec("", "Claude")])).toEqual(["a"]);
  });
});

describe("nearestContaining", () => {
  const PATHS = { caseInsensitive: true };
  const index = new Map([
    [identityKey("/Repo", PATHS), "main"],
    [identityKey("/repo/.claude/worktrees/alpha", PATHS), "alpha"],
  ]);

  it("matches the path itself, then the nearest ancestor", () => {
    expect(nearestContaining(index, "/repo/.claude/worktrees/alpha", PATHS)).toBe("alpha");
    expect(nearestContaining(index, "/REPO/.claude/worktrees/alpha/src/lib/", PATHS)).toBe("alpha");
    expect(nearestContaining(index, "/repo/src", PATHS)).toBe("main");
  });

  it("does not match a sibling that only shares a prefix, or anything outside", () => {
    expect(nearestContaining(index, "/repo/.claude/worktrees/alphabet", PATHS)).toBe("main");
    expect(nearestContaining(index, "/repository", PATHS)).toBeUndefined();
    expect(nearestContaining(index, "/elsewhere", PATHS)).toBeUndefined();
    expect(nearestContaining(index, "", PATHS)).toBeUndefined();
    expect(nearestContaining(new Map([["c:/work", "drive"]]), "C:\\work\\sub", PATHS)).toBe("drive");
  });
});
