import { describe, expect, it } from "vitest";
import { changedPaths } from "./files";

const line = (value: unknown) => ({ line: JSON.stringify(value) });

describe("changedPaths", () => {
  it("reads finished edits inside the root and turn-end moved paths", () => {
    expect(
      changedPaths("/r/", [
        line({ type: "user", tool_use_result: { filePath: "/r/src/a.ts", structuredPatch: [] } }),
        line({ type: "user", tool_use_result: { filePath: "/elsewhere/b.ts" } }),
        line({ type: "assistant", message: { content: [] } }),
        line({ type: "mavi_turn_files", turn: 1, moved: [{ path: "package-lock.json", code: " M" }] }),
        { line: "not json \"tool_use_result\" \"filePath\"" },
      ]),
    ).toEqual(["src/a.ts", "package-lock.json"]);
  });
});

describe("Windows paths", () => {
  it("matches a backslash path to its root, whatever the drive letter's case", async () => {
    const { relativeTo, shortPath } = await import("./rows");
    expect(relativeTo("C:\\repo\\src\\a.ts", "C:\\repo")).toBe("src/a.ts");
    expect(relativeTo("c:\\Repo\\src\\a.ts", "C:\\repo\\")).toBe("src/a.ts");
    expect(relativeTo("C:\\repository\\a.ts", "C:\\repo")).toBeNull();
    expect(relativeTo("/Repo/a.ts", "/repo")).toBeNull();
    expect(shortPath("C:\\repo\\src\\a.ts", "C:\\repo")).toBe("src/a.ts");
  });

  it("reports an edit under a Windows root as a relative path", () => {
    const line = JSON.stringify({ type: "user", tool_use_result: { filePath: "C:\\repo\\src\\a.ts" } });
    expect(changedPaths("C:\\repo", [{ line }])).toEqual(["src/a.ts"]);
  });
});
