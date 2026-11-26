import { describe, expect, it } from "vitest";
import { CheckFailed, createChecks } from "./checks";

describe("createChecks", () => {
  it("names what a failed check observed, from the detail its caller passed", () => {
    const { results, check } = createChecks();
    expect(() => check("scans A once", () => false, () => "A 2/1; B 0/0")).toThrow(CheckFailed);
    expect(results.at(-1)).toEqual({ name: "scans A once — observed: A 2/1; B 0/0", pass: false });
  });

  it("never reads the detail of a check that passed", () => {
    const { results, check } = createChecks();
    let read = 0;
    check("passes", () => true, () => { read += 1; return "unused"; });
    expect(read).toBe(0);
    expect(results.at(-1)).toEqual({ name: "passes", pass: true });
  });

  it("keeps the failure when the detail itself throws", () => {
    const { results, check } = createChecks();
    expect(() => check("fails", () => false, () => { throw new Error("no view"); })).toThrow(CheckFailed);
    expect(results.at(-1)?.name).toBe("fails — detail threw Error: no view");
    expect(results.at(-1)?.pass).toBe(false);
  });

  it("reports an assertion that threw beside its detail", () => {
    const { results, check } = createChecks();
    expect(() => check("reads", () => { throw new Error("null"); }, () => "state")).toThrow(CheckFailed);
    expect(results.at(-1)?.name).toBe("reads — threw Error: null — observed: state");
  });

  it("is unchanged for a two-argument check", () => {
    const { results, check } = createChecks();
    expect(() => check("plain", () => false)).toThrow(CheckFailed);
    expect(results.at(-1)?.name).toBe("plain");
  });
});
