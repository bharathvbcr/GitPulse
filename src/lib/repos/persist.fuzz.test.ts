import { describe, expect, it } from "vitest";
import { identityKey, type PathIdentityOptions } from "./paths";
import {
  loadPersistedWorkspace,
  memoryStorage,
  STORAGE_KEY_WORKSPACE,
  VIEW_TABS,
  type PersistedWorkspace,
} from "./persist";
import { MAX_LAST_CLOSED, MAX_OPEN_TABS, MAX_RECENT_REPOS } from "./tabModel";

const opts: PathIdentityOptions = { caseInsensitive: true };

/**
 * Every hostile payload is a raw JSON string (never a JS literal) so keys
 * like __proto__ survive as OWN properties exactly as a tampered
 * localStorage blob would deliver them.
 */
function payload(body: unknown): string {
  return JSON.stringify(body);
}

function nestArrays(depth: number): unknown {
  let node: unknown = "leaf";
  for (let i = 0; i < depth; i += 1) node = [node];
  return node;
}

interface HostileCase {
  name: string;
  raw: string;
}

const HOSTILE_CASES: HostileCase[] = [
  { name: "proto pollution at top level", raw: '{"__proto__":{"polluted":1}}' },
  {
    name: "proto pollution on a tab entry",
    raw: '{"version":1,"tabs":[{"__proto__":{"polluted":1}}]}',
  },
  {
    name: "proto pollution beside valid fields",
    raw: '{"version":1,"tabs":[{"path":"/a","__proto__":{"polluted":"deep"}}],"activePath":"/a"}',
  },
  {
    name: "constructor.prototype pollution attempt",
    raw: '{"constructor":{"prototype":{"polluted":1}}}',
  },
  {
    name: "proto via recents",
    raw: '{"version":1,"tabs":[],"recents":{"__proto__":{"polluted":1}}}',
  },
  { name: "400-deep nested arrays as tabs", raw: payload({ version: 1, tabs: nestArrays(400) }) },
  { name: "nested array tab entries", raw: payload({ version: 1, tabs: [[["/deep"]]] }) },
  { name: "null tab entries", raw: payload({ version: 1, tabs: [null, null, { path: null }] }) },
  { name: "numeric tab entries", raw: payload({ version: 1, tabs: [1, 2, 3] }) },
  { name: "empty-object tab entries", raw: payload({ version: 1, tabs: [{}, {}, {}] }) },
  {
    name: "non-string tab paths",
    raw: payload({
      version: 1,
      tabs: [{ path: 123 }, { path: true }, { path: {} }, { path: ["/x"] }],
    }),
  },
  {
    name: "control-character path",
    raw: '{"version":1,"tabs":[{"path":"\\u0000/etc/evil"}]}',
  },
  {
    name: "numbers-as-paths in recents",
    raw: payload({ version: 1, tabs: [], recents: [0, 1e9, -1, null, true] }),
  },
  {
    name: "mixed garbage recents",
    raw: payload({
      version: 1,
      tabs: [{ path: "/valid" }],
      recents: [null, "", "   ", "/valid", 42, {}, [], "/valid/"],
    }),
  },
  {
    name: "garbage lastClosed",
    raw: payload({ version: 1, lastClosed: [null, 7, false, {}, ["x"]] }),
  },
  {
    name: "10k-entry tab list",
    raw: payload({
      version: 1,
      tabs: Array.from({ length: 10_000 }, (_, i) => ({ path: `/r/${i}` })),
    }),
  },
  {
    name: "10k-entry recents list",
    raw: payload({
      version: 1,
      tabs: [],
      recents: Array.from({ length: 10_000 }, (_, i) => `/rec/${i}`),
    }),
  },
  {
    name: "10k-entry lastClosed list",
    raw: payload({
      version: 1,
      lastClosed: Array.from({ length: 10_000 }, (_, i) => `/closed/${i}`),
    }),
  },
  {
    name: "duplicate-heavy tabs",
    raw: payload({
      version: 1,
      tabs: Array.from({ length: 50 }, () => ({ path: "/same" })),
    }),
  },
  {
    name: "activePath matching nothing",
    raw: payload({ version: 1, tabs: [{ path: "/a" }], activePath: "/nowhere" }),
  },
  { name: "activePath non-string", raw: payload({ version: 1, tabs: [{ path: "/a" }], activePath: 123 }) },
  { name: "activePath control char", raw: '{"version":1,"tabs":[{"path":"/a"}],"activePath":"/\\u0000"}' },
  { name: "version 0", raw: payload({ version: 0, tabs: [{ path: "/legacy" }] }) },
  { name: "version '1' string", raw: payload({ version: "1", tabs: [{ path: "/legacy" }] }) },
  { name: "version 999", raw: payload({ version: 999, tabs: [{ path: "/legacy" }] }) },
  { name: "version null", raw: payload({ version: null, tabs: [{ path: "/legacy" }] }) },
  { name: "missing version", raw: payload({ tabs: [{ path: "/legacy" }] }) },
  { name: "tabs as string", raw: payload({ version: 1, tabs: "nope" }) },
  { name: "tabs as object", raw: payload({ version: 1, tabs: { 0: { path: "/x" } } }) },
  { name: "blob is an array", raw: '[{"path":"/x"}]' },
  { name: "blob is a bare string", raw: '"just a string"' },
  { name: "blob is null", raw: "null" },
  { name: "blob is a number", raw: "12345" },
  { name: "blob is boolean", raw: "true" },
  {
    name: "hostile scalar fields",
    raw: payload({
      version: 1,
      tabs: [
        { path: "/a", searchQuery: 42, selectedBranch: 7, pinned: "yes", viewTab: "DROP TABLE" },
        { path: "/b", pinned: 1 },
        { path: "/c", searchQuery: null, selectedBranch: false },
      ],
    }),
  },
  {
    name: "unicode NFC/NFD dedupe + backslash paths",
    raw: payload({
      version: 1,
      tabs: [{ path: "/r/cafe\u0301" }, { path: "/r/café" }, { path: "C:\\repo" }],
    }),
  },
  {
    name: "case-folded dedupe under caseInsensitive identity",
    raw: payload({
      version: 1,
      tabs: [{ path: "/Repo/A" }, { path: "/repo/a/" }, { path: "/repo/b" }],
      activePath: "/REPO/A",
    }),
  },
];

function expectSchemaValid(result: PersistedWorkspace): void {
  expect(result.version).toBe(1);
  expect(Array.isArray(result.tabs)).toBe(true);
  expect(result.tabs.length).toBeLessThanOrEqual(MAX_OPEN_TABS);
  for (const tab of result.tabs) {
    expect(typeof tab.path).toBe("string");
    expect(tab.path.length).toBeGreaterThan(0);
    expect(typeof tab.pinned).toBe("boolean");
    expect(VIEW_TABS).toContain(tab.viewTab);
    expect(typeof tab.searchQuery).toBe("string");
    expect(tab.selectedBranch === null || typeof tab.selectedBranch === "string").toBe(true);
  }
  expect(Array.isArray(result.recents)).toBe(true);
  expect(result.recents.length).toBeLessThanOrEqual(MAX_RECENT_REPOS);
  for (const path of result.recents) expect(typeof path).toBe("string");
  expect(Array.isArray(result.lastClosed)).toBe(true);
  expect(result.lastClosed.length).toBeLessThanOrEqual(MAX_LAST_CLOSED);
  for (const path of result.lastClosed) expect(typeof path).toBe("string");
  if (result.activePath !== null) {
    expect(typeof result.activePath).toBe("string");
    const activeId = identityKey(result.activePath, opts);
    expect(result.tabs.some((tab) => identityKey(tab.path, opts) === activeId)).toBe(true);
  }
}

function prototypeIsClean(): void {
  expect(({} as Record<string, unknown>).polluted).toBeUndefined();
  expect(Object.getOwnPropertyDescriptor(Object.prototype, "polluted")).toBeUndefined();
}

describe("loadPersistedWorkspace fuzz", () => {
  it("survives every hostile payload with schema-valid output and no global pollution", () => {
    expect(HOSTILE_CASES.length).toBeGreaterThanOrEqual(30);
    for (const testCase of HOSTILE_CASES) {
      prototypeIsClean(); // pre-condition: nothing polluted by earlier cases
      const storage = memoryStorage({ [STORAGE_KEY_WORKSPACE]: testCase.raw });

      let result: PersistedWorkspace | undefined;
      expect(() => {
        result = loadPersistedWorkspace(storage, opts);
      }, testCase.name).not.toThrow();

      expect(result).toBeDefined();
      expectSchemaValid(result!);
      prototypeIsClean(); // post-condition: no Object.prototype pollution
    }
  });

  it("enforces the documented caps on oversized payloads", () => {
    const tenKTabs = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: payload({
        version: 1,
        tabs: Array.from({ length: 10_000 }, (_, i) => ({ path: `/r/${i}` })),
        recents: Array.from({ length: 10_000 }, (_, i) => `/rec/${i}`),
        lastClosed: Array.from({ length: 10_000 }, (_, i) => `/closed/${i}`),
      }),
    });
    const capped = loadPersistedWorkspace(tenKTabs, opts);
    expect(capped.tabs).toHaveLength(MAX_OPEN_TABS);
    expect(capped.recents).toHaveLength(MAX_RECENT_REPOS);
    expect(capped.lastClosed).toHaveLength(MAX_LAST_CLOSED);
    expect(capped.activePath).toBe("/r/0");

    const duped = loadPersistedWorkspace(
      memoryStorage({
        [STORAGE_KEY_WORKSPACE]: payload({
          version: 1,
          tabs: Array.from({ length: 50 }, () => ({ path: "/same" })),
        }),
      }),
      opts
    );
    expect(duped.tabs).toHaveLength(1);
  });

  it("falls back to tabs[0] when activePath matches nothing", () => {
    const storage = memoryStorage({
      [STORAGE_KEY_WORKSPACE]: payload({
        version: 1,
        tabs: [{ path: "/a" }, { path: "/b" }],
        activePath: "/nowhere",
      }),
    });
    expect(loadPersistedWorkspace(storage, opts).activePath).toBe("/a");
  });

  it("lands non-v1 blobs on the legacy path with an empty workspace", () => {
    for (const raw of [
      payload({ version: 0, tabs: [{ path: "/legacy" }] }),
      payload({ version: "1", tabs: [{ path: "/legacy" }] }),
      payload({ version: 999 }),
      payload({ version: null }),
      payload({}),
    ]) {
      const loaded = loadPersistedWorkspace(memoryStorage({ [STORAGE_KEY_WORKSPACE]: raw }), opts);
      expectSchemaValid(loaded);
      expect(loaded.tabs).toEqual([]);
      expect(loaded.activePath).toBeNull();
    }
  });
});
