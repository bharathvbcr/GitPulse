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
