import { describe, expect, it } from "vitest";
import { planConflictSave } from "./conflictSave";

describe("planConflictSave", () => {
  it("reports full success only when the write and stage both land", () => {
    expect(planConflictSave(true, true)).toEqual({
      written: true,
      staged: true,
      journalOk: true,
      complete: true,
      message: null,
    });
  });

  it("keeps a successful write honest when staging fails afterwards", () => {
    const plan = planConflictSave(true, false, new Error("index locked"));
    expect(plan.journalOk).toBe(true);
    expect(plan.complete).toBe(false);
    expect(plan.message).toMatch(/staging failed.*index locked/s);
  });

  it("marks the edit as not done when the write itself failed", () => {
    const plan = planConflictSave(false, false, new Error("disk full"));
    expect(plan.journalOk).toBe(false);
    expect(plan.complete).toBe(false);
    expect(plan.message).toMatch(/Save failed.*disk full/s);
  });
});
