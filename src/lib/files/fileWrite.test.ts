import { describe, expect, it } from "vitest";
import {
  FileWriteFailure,
  newPathProblem,
  readFileWriteError,
  saveBase,
  UnbasedDraft,
  withAncestors,
} from "./fileWrite";
import type { FileWriteError } from "./types";

const valid: FileWriteError[] = [
  { code: "already_exists", message: "'a.txt' already exists", path: "a.txt" },
  { code: "changed_on_disk", message: "changed", path: "a.txt", expected: "3:aa", actual: "absent" },
  { code: "refused", message: "Git metadata cannot be read or written as a repository file: .git/config" },
];

describe("readFileWriteError", () => {
  it("reads every variant the host sends, as it was sent", () => {
    for (const error of valid) expect(readFileWriteError({ ...error })).toEqual(error);
  });

  it("reads one that was already wrapped", () => {
    expect(readFileWriteError(new FileWriteFailure(valid[1]!))).toEqual(valid[1]);
    expect(new FileWriteFailure(valid[0]!).message).toBe("'a.txt' already exists");
  });

  it("trusts nothing else: strings, errors, unknown codes and variants missing a field", () => {
    for (const bad of [
      "a plain string rejection",
      new Error("boom"),
      null,
      undefined,
      42,
      { code: "already_exists", message: "m" },
      { code: "changed_on_disk", message: "m", path: "a", expected: "e" },
      { code: "changed_on_disk", message: "m", path: "a", expected: 1, actual: "x" },
      { code: "refused", message: "   " },
      { code: "exploded", message: "m" },
      { message: "no code" },
    ]) expect(readFileWriteError(bad), JSON.stringify(bad)).toBeNull();
  });
});

describe("newPathProblem", () => {
  const listed = withAncestors(["src/lib/a.ts", "README.md"]);

  it("accepts a new name, at the root and inside a folder", () => {
    expect(newPathProblem("notes.ts", "", listed)).toBeNull();
    expect(newPathProblem("deep/new/file.ts", "src", listed)).toBeNull();
  });

  it("refuses empty names, empty segments and dot segments", () => {
    expect(newPathProblem("   ", "", listed)).toMatch(/name/i);
    expect(newPathProblem("a//b", "", listed)).toMatch(/empty segments/);
    expect(newPathProblem("a/", "", listed)).toMatch(/empty segments/);
    expect(newPathProblem("../x", "", listed)).toMatch(/\.\./);
    expect(newPathProblem("a/./b", "src", listed)).toMatch(/\./);
  });

  it("refuses a name the listing already holds, file or folder, trimmed", () => {
    expect(newPathProblem("README.md", "", listed)).toMatch(/already exists/);
    expect(newPathProblem("  README.md ", "", listed)).toMatch(/already exists/);
    expect(newPathProblem("lib", "src", listed)).toMatch(/src\/lib/);
    expect(newPathProblem("a.ts", "src/lib", listed)).toMatch(/already exists/);
  });
});

describe("withAncestors", () => {
  it("adds every folder above each path", () => {
    expect([...withAncestors(["a/b/c.ts", "d.ts"])].sort()).toEqual(["a", "a/b", "a/b/c.ts", "d.ts"]);
  });
});

describe("saveBase", () => {
  it("judges a draft by the version it was made from, not the blob on screen", () => {
    expect(saveBase({ base: "3:old" }, "9:newer")).toEqual({ base: "3:old" });
  });

  it("fails closed for a draft with no base: it never borrows the blob's, which may be newer", () => {
    expect(saveBase({ base: null }, "9:newer")).toBe("unbased");
    expect(saveBase({ base: null }, null)).toBe("unbased");
    expect(saveBase({ base: null }, undefined)).toBe("unbased");
  });

  it("lets only an explicit override write an unbased draft, or name the version Overwrite saw", () => {
    expect(saveBase({ base: null }, "9:newer", null)).toEqual({ base: null });
    expect(saveBase({ base: "3:old" }, "9:newer", "9:newer")).toEqual({ base: "9:newer" });
  });

  it("uses the blob's own base only when there is no draft", () => {
    expect(saveBase(undefined, "9:newer")).toEqual({ base: "9:newer" });
    expect(saveBase(undefined, undefined)).toEqual({ base: null });
  });

  it("says what was refused", () => {
    expect(new UnbasedDraft("a.ts").message).toMatch(/a\.ts.*Nothing was written/);
  });
});
