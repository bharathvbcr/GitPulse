import { beforeEach, describe, expect, it } from "vitest";
import {
  clearEditorDraftRegistryForTests,
  hasUnsavedEditorDrafts,
  recordEditorDrafts,
  unsavedEditorDrafts,
} from "./editorDraftRegistry";

describe("editor draft registry", () => {
  beforeEach(clearEditorDraftRegistryForTests);

  it("tracks dirty files across repositories and removes clean repositories", () => {
    recordEditorDrafts("/repo/a", ["src/a.ts", "src/a.ts", "src/b.ts"]);
    recordEditorDrafts("/repo/b", ["README.md"]);

    expect(hasUnsavedEditorDrafts()).toBe(true);
    expect(unsavedEditorDrafts()).toEqual([
      { repo: "/repo/a", paths: ["src/a.ts", "src/b.ts"] },
      { repo: "/repo/b", paths: ["README.md"] },
    ]);

    recordEditorDrafts("/repo/a", []);
    expect(unsavedEditorDrafts()).toEqual([
      { repo: "/repo/b", paths: ["README.md"] },
    ]);
  });

  it("returns defensive snapshots and ignores invalid paths", () => {
    recordEditorDrafts("/repo", ["", "  ", "src/a.ts"]);
    const snapshot = unsavedEditorDrafts();
    snapshot[0]?.paths.push("tamper.ts");

    expect(unsavedEditorDrafts()).toEqual([
      { repo: "/repo", paths: ["src/a.ts"] },
    ]);
  });
});
