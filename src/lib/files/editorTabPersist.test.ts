import { describe, expect, it } from "vitest";
import { memoryStorage } from "../repos/persist";
import {
  EDITOR_TAB_STORAGE_KEY,
  loadPersistedEditorTabs,
  MAX_EDITOR_TABS,
  savePersistedEditorTabs,
} from "./editorTabPersist";

const repo = "/Users/dev/Code/GitPulse";

function files(count: number) {
  return Array.from({ length: count }, (_, index) => ({
    path: `src/file-${index}.ts`,
    preview: index === count - 1,
  }));
}

describe("editor tab persistence", () => {
  it("restores open files and ignores a same-generation snapshot that drops half of them", () => {
    const storage = memoryStorage();
    expect(savePersistedEditorTabs(repo, { tabs: files(8), active: "src/file-3.ts" }, false, storage)).toBe(true);
    expect(savePersistedEditorTabs(repo, { tabs: files(4), active: "src/file-0.ts" }, false, storage)).toBe(false);
    const loaded = loadPersistedEditorTabs(repo, storage);
    expect(loaded?.tabs).toHaveLength(8);
    expect(loaded?.active).toBe("src/file-3.ts");
    expect(loaded?.tabs.filter((tab) => tab.preview).map((tab) => tab.path)).toEqual(["src/file-7.ts"]);
    expect(loaded?.drafts).toEqual({});
  });

  it("commits an explicit close", () => {
    const storage = memoryStorage();
    savePersistedEditorTabs(repo, { tabs: files(6), active: "src/file-1.ts" }, false, storage);
    expect(savePersistedEditorTabs(repo, { tabs: files(2), active: "src/file-1.ts" }, true, storage)).toBe(true);
    expect(loadPersistedEditorTabs(repo, storage)?.tabs.map((tab) => tab.path)).toEqual([
      "src/file-0.ts",
      "src/file-1.ts",
    ]);
  });

  it("drops hostile paths and keeps one preview", () => {
    const storage = memoryStorage();
    savePersistedEditorTabs(repo, {
      tabs: [
        { path: "../secrets.env", preview: false },
        { path: "/etc/passwd", preview: true },
        { path: "src/ok.ts", preview: true },
        { path: "src/also.ts", preview: true },
        { path: "bad\u0000name.ts", preview: false },
      ],
      active: "src/ok.ts",
    }, true, storage);
    const loaded = loadPersistedEditorTabs(repo, storage);
    expect(loaded?.tabs.map((tab) => tab.path)).toEqual(["src/ok.ts", "src/also.ts"]);
    expect(loaded?.tabs.filter((tab) => tab.preview)).toHaveLength(1);
  });

  it("does not adopt a prototype-polluting blob as tabs", () => {
    const storage = memoryStorage({
      [EDITOR_TAB_STORAGE_KEY]:
        '{"version":1,"repos":{"__proto__":{"polluted":true},"/Users/dev/Code/GitPulse":{"epoch":1,"tabs":[{"path":"src/a.ts","preview":false}],"active":"src/a.ts"}}}',
    });
    expect(({} as { polluted?: boolean }).polluted).toBeUndefined();
    expect(loadPersistedEditorTabs(repo, storage)?.tabs).toHaveLength(1);
  });

  it("caps a burst of files instead of growing without a bound", () => {
    const storage = memoryStorage();
    savePersistedEditorTabs(repo, { tabs: files(MAX_EDITOR_TABS + 30), active: null }, true, storage);
    expect(loadPersistedEditorTabs(repo, storage)?.tabs).toHaveLength(MAX_EDITOR_TABS);
  });
});
