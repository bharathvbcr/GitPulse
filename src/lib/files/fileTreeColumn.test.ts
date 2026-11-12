import { get } from "svelte/store";
import { describe, expect, it } from "vitest";
import {
  FILE_TREE_ABSOLUTE_MAX,
  FILE_TREE_COLUMN_STORAGE_KEY,
  FILE_TREE_DEFAULT_WIDTH,
  FILE_TREE_MIN_WIDTH,
  clampFileTreeWidth,
  createFileTreeColumnStore,
  fileTreeMaxWidth,
  loadFileTreeWidth,
} from "./fileTreeColumn";

function memoryStorage(initial: Record<string, string> = {}) {
  const backing = { ...initial };
  return {
    backing,
    storage: {
      getItem: (key: string) => (key in backing ? backing[key] : null),
      setItem: (key: string, value: string) => {
        backing[key] = value;
      },
    },
  };
}

describe("file tree column width", () => {
  it("defaults to the old w-72 and refuses a non-width blob", () => {
    expect(FILE_TREE_DEFAULT_WIDTH).toBe(288);
    expect(FILE_TREE_MIN_WIDTH).toBe(180);
    for (const raw of [null, "", "{", "[]", "288", "null", '{"width":"320"}', '{"nope":1}']) {
      expect(loadFileTreeWidth(raw)).toBe(FILE_TREE_DEFAULT_WIDTH);
    }
  });

  it("clamps a saved width into the floor and the absolute ceiling", () => {
    expect(loadFileTreeWidth('{"width":320}')).toBe(320);
    expect(loadFileTreeWidth('{"width":10}')).toBe(FILE_TREE_MIN_WIDTH);
    expect(loadFileTreeWidth('{"width":0}')).toBe(FILE_TREE_MIN_WIDTH);
    expect(loadFileTreeWidth('{"width":99999}')).toBe(FILE_TREE_ABSOLUTE_MAX);
    expect(loadFileTreeWidth('{"width":null}')).toBe(FILE_TREE_DEFAULT_WIDTH);
  });

  it("keeps an unmeasured preference and caps a measured one at half the row", () => {
    expect(clampFileTreeWidth(288, 0)).toBe(288);
    expect(clampFileTreeWidth(Number.NaN, 1000)).toBe(FILE_TREE_DEFAULT_WIDTH);
    expect(clampFileTreeWidth(Number.POSITIVE_INFINITY, 1000)).toBe(FILE_TREE_DEFAULT_WIDTH);
    expect(clampFileTreeWidth(10, 1000)).toBe(FILE_TREE_MIN_WIDTH);
    expect(fileTreeMaxWidth(1000)).toBe(500);
    expect(clampFileTreeWidth(900, 1000)).toBe(500);
    expect(clampFileTreeWidth(180.6, 1000)).toBe(181);
  });

  it("leaves the reserved sibling its room, which can be tighter than half", () => {
    // 800px row, 420px editor: half is 400, but 400 would leave the editor 380.
    expect(fileTreeMaxWidth(800, 420)).toBe(380);
    expect(clampFileTreeWidth(700, 800, 420)).toBe(380);
    // Too narrow to spare the reserve: the filter floor still holds.
    expect(fileTreeMaxWidth(300, 420)).toBe(FILE_TREE_MIN_WIDTH);
    expect(clampFileTreeWidth(288, 300, 420)).toBe(FILE_TREE_MIN_WIDTH);
  });

  it("persists a committed width and not the drag stream", () => {
    const { backing, storage } = memoryStorage();
    const store = createFileTreeColumnStore(storage);
    expect(get(store)).toBe(FILE_TREE_DEFAULT_WIDTH);
    expect(backing[FILE_TREE_COLUMN_STORAGE_KEY]).toBeUndefined();

    store.setWidthLive(420, 1200, 180);
    expect(get(store)).toBe(420);
    expect(backing[FILE_TREE_COLUMN_STORAGE_KEY]).toBeUndefined();

    store.setWidth(420, 1200, 180);
    expect(JSON.parse(backing[FILE_TREE_COLUMN_STORAGE_KEY] ?? "")).toEqual({ width: 420 });

    store.setWidthLive(200, 1200, 180);
    expect(get(store)).toBe(200);
    expect(JSON.parse(backing[FILE_TREE_COLUMN_STORAGE_KEY] ?? "")).toEqual({ width: 420 });

    const restored = createFileTreeColumnStore(storage);
    expect(get(restored)).toBe(420);
  });

  it("one store is what a second column reads back", () => {
    const { storage } = memoryStorage();
    const explorer = createFileTreeColumnStore(storage);
    explorer.setWidth(360, 1000, 420);
    const blame = createFileTreeColumnStore(storage);
    expect(get(blame)).toBe(360);
    expect(clampFileTreeWidth(get(blame), 1000, 180)).toBe(360);
  });
});
