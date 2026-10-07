import { describe, expect, it } from "vitest";
import { liveRunIn } from "./liveRun";

const WT = "/work/repo/.gitpulse/worktrees/fix-e42-wt0attem";

describe("liveRunIn", () => {
  it("finds the live attempt working in a worktree by checkout identity, not spelling", () => {
    const runs = [{ id: "a", cwd: `${WT}/` }, { id: "b", cwd: "/work/repo" }];
    expect(liveRunIn(WT, runs)?.id).toBe("a");
    expect(liveRunIn("/work/repo/.gitpulse/worktrees/other-12345678", runs)).toBeUndefined();
    expect(liveRunIn("", runs)).toBeUndefined();
  });
});
