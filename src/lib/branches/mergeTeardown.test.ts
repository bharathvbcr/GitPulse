import { describe, expect, it } from "vitest";
import { MergeTeardownFailure, readMergeTeardownError } from "./mergeTeardown";
import type { MergeTeardownError } from "./types";

const valid: MergeTeardownError[] = [
  { code: "conflict", message: "conflicted", paths: ["a.txt"], total: 1, rolled_back: true, rollback_error: null },
  { code: "conflict", message: "conflicted", paths: [], total: 0, rolled_back: false, rollback_error: "index.lock exists" },
  { code: "moved", message: "moved", branch: "b", expected: "e", actual: "a" },
  { code: "base_mismatch", message: "mismatch", base_branch: "main", target_branch: "dev", base_oid: "abc" },
  { code: "squash_parked", message: "parked" },
  { code: "refused", message: "no" },
];

describe("readMergeTeardownError", () => {
  it("reads every variant the host sends, as it was sent", () => {
    for (const error of valid) expect(readMergeTeardownError({ ...error })).toEqual(error);
  });

  it("reads one that was already wrapped", () => {
    expect(readMergeTeardownError(new MergeTeardownFailure(valid[0]!))).toEqual(valid[0]);
    expect(new MergeTeardownFailure(valid[2]!).message).toBe("moved");
  });

  it("trusts nothing else: strings, errors, unknown codes and variants missing a field are not typed failures", () => {
    for (const bad of [
      "a plain string rejection",
      new Error("boom"),
      null,
      undefined,
      42,
      { code: "conflict", message: "m", paths: "a.txt", total: 1, rolled_back: true, rollback_error: null },
      { code: "conflict", message: "m", paths: ["a"], total: -1, rolled_back: true, rollback_error: null },
      { code: "conflict", message: "m", paths: ["a"], total: 1, rolled_back: "yes", rollback_error: null },
      { code: "moved", message: "m", branch: "b" },
      { code: "base_mismatch", message: "m", base_branch: "x" },
      { code: "squash_parked" },
      { code: "squash_parked", message: "   " },
      { code: "exploded", message: "m" },
      { message: "no code" },
    ]) expect(readMergeTeardownError(bad), JSON.stringify(bad)).toBeNull();
  });
});
