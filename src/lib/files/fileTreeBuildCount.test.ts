import { describe, expect, it } from "vitest";
import { buildFileTree, fileTreeBuildCount } from "./fileTree";

describe("fileTreeBuildCount", () => {
  it("counts every build, valid input or not, and nothing else", () => {
    const before = fileTreeBuildCount();
    buildFileTree(["a/b.ts"]);
    buildFileTree([]);
    buildFileTree(["../escape"]);
    expect(fileTreeBuildCount() - before).toBe(3);
  });
});
