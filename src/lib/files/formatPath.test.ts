import { describe, expect, it } from "vitest";
import { formatPathParts } from "./formatPath";

describe("formatPathParts", () => {
  it("splits paths with directories correctly", () => {
    expect(formatPathParts("src/lib/components/Sidebar.svelte")).toEqual({
      dir: "src/lib/components/",
      name: "Sidebar.svelte",
    });
    expect(formatPathParts("a/b/c.txt")).toEqual({
      dir: "a/b/",
      name: "c.txt",
    });
  });

  it("handles top-level filenames with no directories", () => {
    expect(formatPathParts("package.json")).toEqual({
      dir: "",
      name: "package.json",
    });
    expect(formatPathParts("README.md")).toEqual({
      dir: "",
      name: "README.md",
    });
  });

  it("handles empty or degenerate paths safely", () => {
    expect(formatPathParts("")).toEqual({
      dir: "",
      name: "",
    });
    expect(formatPathParts("/")).toEqual({
      dir: "/",
      name: "",
    });
    expect(formatPathParts("/file.txt")).toEqual({
      dir: "/",
      name: "file.txt",
    });
  });
});
