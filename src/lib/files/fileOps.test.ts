import { describe, expect, it } from "vitest";
import { describeDelete, isAtOrUnder, remapPath } from "./fileOps";

describe("isAtOrUnder", () => {
  it("matches the path and its contents, not a sibling sharing a prefix", () => {
    expect(isAtOrUnder("src", "src")).toBe(true);
    expect(isAtOrUnder("src/a.ts", "src")).toBe(true);
    expect(isAtOrUnder("src-old/a.ts", "src")).toBe(false);
  });
});

describe("remapPath", () => {
  it("follows a moved file and files inside a moved folder", () => {
    expect(remapPath("a.ts", "a.ts", "b/a.ts")).toBe("b/a.ts");
    expect(remapPath("src/x/y.ts", "src", "lib")).toBe("lib/x/y.ts");
    expect(remapPath("srcx/y.ts", "src", "lib")).toBe("srcx/y.ts");
  });
});

describe("describeDelete", () => {
  const files = ["dir/a.ts", "dir/b.ts", "dir/new.ts", "other.ts"];
  const untracked = new Set(["dir/new.ts"]);

  it("splits a folder into restorable tracked files and unrecoverable untracked ones", () => {
    const message = describeDelete("dir", "dir", files, untracked);
    expect(message).toContain("Delete the folder dir and the 3 files it shows here?");
    expect(message).toContain("Tracked (2 files): removed with git rm and staged");
    expect(message).toContain("Untracked (1 file): removed with git clean");
    expect(message).toContain("cannot be undone");
    expect(message).toContain("Ignored files inside it are left in place.");
  });

  it("does not promise a restore for an untracked file, nor warn of loss for a tracked one", () => {
    const lost = describeDelete("dir/new.ts", "file", files, untracked);
    expect(lost).toContain("cannot be undone");
    expect(lost).not.toContain("restorable");
    const kept = describeDelete("other.ts", "file", files, untracked);
    expect(kept).toContain("restorable from HEAD");
    expect(kept).not.toContain("cannot be undone");
  });
});
