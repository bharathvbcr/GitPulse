import { describe, expect, it } from "vitest";
import { memoryStorage, type StorageLike } from "../repos/persist";
import {
  MAX_PINNED_REPOS,
  PINNED_INDEX_KEY,
  loadPinnedIndex,
  parseIndex,
  parsePinned,
  pinnedKey,
  prunePinnedIndex,
  saveRepoPins,
  serializePinned,
  touchIndex,
} from "./pins";

/** Deterministic PRNG so failures reproduce exactly (mulberry32). */
function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** Storage whose writes always throw (quota exhausted); reads work. */
function quotaFullStorage(initial: Record<string, string> = {}): StorageLike {
  const map = { ...initial };
  return {
    getItem: (key) => (Object.prototype.hasOwnProperty.call(map, key) ? map[key] : null),
    setItem: () => {
      throw new Error("QuotaExceededError");
    },
    removeItem: () => {
      throw new Error("QuotaExceededError");
    },
  };
}

/** Storage where every operation throws (private mode worst case). */
function deadStorage(): StorageLike {
  return {
    getItem: () => {
      throw new Error("SecurityError");
    },
    setItem: () => {
      throw new Error("SecurityError");
    },
    removeItem: () => {
      throw new Error("SecurityError");
    },
  };
}

describe("pinnedKey", () => {
  it("namespaces pins under the repo path", () => {
    expect(pinnedKey("/repo/a")).toBe("gitpulse:pinned:/repo/a");
    expect(pinnedKey("")).toBe("gitpulse:pinned:");
  });
});

describe("parsePinned", () => {
  it("returns [] for null/undefined/empty input", () => {
    expect(parsePinned(null)).toEqual([]);
    expect(parsePinned(undefined)).toEqual([]);
    expect(parsePinned("")).toEqual([]);
  });

  it("parses a stored array into sorted deduped names", () => {
    expect(parsePinned('["b","a","b","c"]')).toEqual(["a", "b", "c"]);
  });

  it("fails closed on non-array JSON", () => {
    expect(parsePinned('{"pinned":["main"]}')).toEqual([]);
    expect(parsePinned('"main"')).toEqual([]);
    expect(parsePinned("42")).toEqual([]);
    expect(parsePinned("true")).toEqual([]);
    expect(parsePinned("null")).toEqual([]);
  });

  it("fails closed on unparseable garbage instead of throwing", () => {
    expect(parsePinned("{{{")).toEqual([]);
    expect(parsePinned('["main",')).toEqual([]);
    expect(parsePinned("not json at all")).toEqual([]);
  });

  it("drops non-string entries rather than rejecting the whole list", () => {
    const raw = '[1,"keep",null,{"nested":true},["deep"],true,"also"]';
    expect(parsePinned(raw)).toEqual(["also", "keep"]);
  });
});

describe("serializePinned", () => {
  it("dedupes and sorts regardless of input order", () => {
    expect(serializePinned(["z", "a", "z", "m"])).toBe('["a","m","z"]');
  });

  it("round-trips through parsePinned to an identical value", () => {
    const names = ["feat/x", "main", "release-1.0"];
    expect(parsePinned(serializePinned(names))).toEqual([...names].sort());
    // And the serialization itself is stable under round-trip.
    const once = serializePinned(names);
    expect(serializePinned(parsePinned(once))).toBe(once);
  });

  it("serializes an empty list as an empty array", () => {
    expect(serializePinned([])).toBe("[]");
    expect(serializePinned(new Set<string>())).toBe("[]");
  });
});

describe("parsePinned fuzz/stress: hostile storage payloads", () => {
  it("never throws and always returns a sorted deduped string[] for random garbage", () => {
    const rand = mulberry32(0xc0ffee);
    const alphabet = '[]{}":,\\abcXYZ0 \t\n"\'`<>/\\~!@#$%^&*()_+中文😀';
    for (let i = 0; i < 5_000; i += 1) {
      const len = Math.floor(rand() * 200);
      let s = "";
      for (let j = 0; j < len; j += 1) {
        s += alphabet[Math.floor(rand() * alphabet.length)];
      }
      const out = parsePinned(s);
      expect(Array.isArray(out)).toBe(true);
      expect(out.every((n) => typeof n === "string")).toBe(true);
      expect([...out].sort()).toEqual(out);
      expect(new Set(out).size).toBe(out.length);
    }
  });

  it("survives deeply nested arrays that blow JSON.parse's stack", () => {
    const deep = `${"[".repeat(50_000)}${"]".repeat(50_000)}`;
    expect(parsePinned(deep)).toEqual([]);
    // Mixed depth with a real string buried at the top level.
    expect(() => parsePinned(`["a",${"[".repeat(10_000)}]`)).not.toThrow();
  });

  it("handles 100k entries quickly without throwing or losing dedup/sort", () => {
    const names: string[] = [];
    for (let i = 0; i < 100_000; i += 1) {
      names.push(`branch-${i % 40_000}`);
    }
    names.push("zz-last");
    const raw = JSON.stringify(names);

    const startedAt = performance.now();
    const parsed = parsePinned(raw);
    const serialized = serializePinned(parsed);
    const elapsedMs = performance.now() - startedAt;

    expect(elapsedMs).toBeLessThan(2_000);
    expect(parsed).toHaveLength(40_001);
    expect(parsed[parsed.length - 1]).toBe("zz-last");
    expect(parsed[0]).toBe("branch-0");
    expect(serialized.length).toBeGreaterThan(0);
    expect(parsePinned(serialized)).toEqual(parsed);
  });

  it("tolerates huge single entries (megabyte branch names)", () => {
    const big = "b".repeat(1_000_000);
    const raw = JSON.stringify([big, "a"]);
    const out = parsePinned(raw);
    expect(out).toEqual(["a", big]);
    expect(serializePinned(out)).toBe(JSON.stringify(["a", big]));
  });
});

describe("parseIndex", () => {
  it("returns [] for null/undefined/empty input", () => {
    expect(parseIndex(null)).toEqual([]);
    expect(parseIndex(undefined)).toEqual([]);
    expect(parseIndex("")).toEqual([]);
  });

  it("preserves MRU order instead of sorting", () => {
    // Unlike pin names, index position encodes recency — newest FIRST.
    expect(parseIndex('["/z","/a","/m"]')).toEqual(["/z", "/a", "/m"]);
  });

  it("dedupes keeping only the first (most recent) occurrence", () => {
    expect(parseIndex('["/b","/a","/b"]')).toEqual(["/b", "/a"]);
  });

  it("drops hostile entries: non-strings, empties, nested junk", () => {
    const raw = '[1,"/keep",null,{"p":1},["/deep"],"",true,"/also"]';
    expect(parseIndex(raw)).toEqual(["/keep", "/also"]);
  });

  it("fails closed on non-array JSON", () => {
    expect(parseIndex('{"repos":["/a"]}')).toEqual([]);
    expect(parseIndex('"/a"')).toEqual([]);
    expect(parseIndex("42")).toEqual([]);
    expect(parseIndex("null")).toEqual([]);
  });

  it("fails closed on unparseable garbage instead of throwing", () => {
    expect(parseIndex("{{{")).toEqual([]);
    expect(parseIndex('["/a",')).toEqual([]);
    expect(parseIndex("not json at all")).toEqual([]);
  });

  it("survives deeply nested arrays that blow JSON.parse's stack", () => {
    const deep = `${"[".repeat(50_000)}${"]".repeat(50_000)}`;
    expect(parseIndex(deep)).toEqual([]);
    expect(() => parseIndex(`["/a",${"[".repeat(10_000)}]`)).not.toThrow();
  });

  it("never throws and always returns a deduped string[] for random garbage", () => {
    const rand = mulberry32(0x1ade5);
    const alphabet = '[]{}":,\\abcXYZ0 \t\n"\'`<>/\\~!@#$%^&*()_+中文😀';
    for (let i = 0; i < 2_000; i += 1) {
      const len = Math.floor(rand() * 200);
      let s = "";
      for (let j = 0; j < len; j += 1) {
        s += alphabet[Math.floor(rand() * alphabet.length)];
      }
      const out = parseIndex(s);
      expect(Array.isArray(out)).toBe(true);
      expect(out.every((n) => typeof n === "string" && n.length > 0)).toBe(true);
      expect(new Set(out).size).toBe(out.length);
    }
  });
});

describe("touchIndex", () => {
  it("prepends an unseen repo path", () => {
    expect(touchIndex(["/a", "/b"], "/c")).toEqual(["/c", "/a", "/b"]);
  });

  it("moves a seen repo path to the front, preserving the others' order", () => {
    expect(touchIndex(["/a", "/b", "/c"], "/b")).toEqual(["/b", "/a", "/c"]);
  });

  it("does not mutate the input array", () => {
    const input = ["/a", "/b"];
    touchIndex(input, "/a");
    expect(input).toEqual(["/a", "/b"]);
  });

  it("collapses duplicate entries of the touched path", () => {
    expect(touchIndex(["/a", "/b", "/a"], "/a")).toEqual(["/a", "/b"]);
  });
});

describe("saveRepoPins", () => {
  it("writes the blob under the per-repo key and touches the index front", () => {
    const store = memoryStorage({
      [PINNED_INDEX_KEY]: JSON.stringify(["/old"]),
    });
    expect(saveRepoPins(store, "/repo/a", '["main"]')).toBe(true);
    expect(store.getItem(pinnedKey("/repo/a"))).toBe('["main"]');
    expect(loadPinnedIndex(store)).toEqual(["/repo/a", "/old"]);
  });

  it("keeps MRU order across successive saves of different repos", () => {
    const store = memoryStorage();
    saveRepoPins(store, "/one", "[]");
    saveRepoPins(store, "/two", "[]");
    saveRepoPins(store, "/three", "[]");
    saveRepoPins(store, "/two", '["x"]');
    expect(loadPinnedIndex(store)).toEqual(["/two", "/three", "/one"]);
  });

  it("does not cap the index itself — pruning owns eviction", () => {
    // Trimming here would orphan blobs beyond discovery, recreating the
    // accumulation this module exists to bound.
    const store = memoryStorage();
    for (let i = 0; i < MAX_PINNED_REPOS + 5; i += 1) {
      saveRepoPins(store, `/repo/${i}`, "[]");
    }
    expect(loadPinnedIndex(store)).toHaveLength(MAX_PINNED_REPOS + 5);
  });

  it("returns false and leaves the index untouched when the blob write fails", () => {
    const store = quotaFullStorage({
      [PINNED_INDEX_KEY]: JSON.stringify(["/old"]),
    });
    expect(saveRepoPins(store, "/new", "[]")).toBe(false);
    expect(store.getItem(pinnedKey("/new"))).toBe(null);
    expect(loadPinnedIndex(store)).toEqual(["/old"]);
  });

  it("no-ops on null storage or empty repo path without throwing", () => {
    expect(() => saveRepoPins(null, "/a", "[]")).not.toThrow();
    expect(saveRepoPins(null, "/a", "[]")).toBe(false);
    const store = memoryStorage();
    expect(() => saveRepoPins(store, "", "[]")).not.toThrow();
    expect(saveRepoPins(store, "", "[]")).toBe(false);
    expect(store.getItem(PINNED_INDEX_KEY)).toBe(null);
  });

  it("survives a fully dead storage without throwing", () => {
    expect(() => saveRepoPins(deadStorage(), "/a", "[]")).not.toThrow();
    expect(saveRepoPins(deadStorage(), "/a", "[]")).toBe(false);
  });
});

describe("prunePinnedIndex", () => {
  function seeded(count: number): StorageLike {
    const store = memoryStorage();
    for (let i = 0; i < count; i += 1) {
      saveRepoPins(store, `/repo/${i}`, JSON.stringify([`b${i}`]));
    }
    return store;
  }

  function survivingBlobs(store: StorageLike): number {
    let n = 0;
    for (let i = 0; i < 200; i += 1) {
      if (store.getItem(pinnedKey(`/repo/${i}`)) !== null) n += 1;
    }
    return n;
  }

  it("caps at MAX_PINNED_REPOS and deletes the evicted repos' blob keys", () => {
    const store = seeded(MAX_PINNED_REPOS + 10);
    prunePinnedIndex(store);
    expect(survivingBlobs(store)).toBe(MAX_PINNED_REPOS);
    const index = loadPinnedIndex(store);
    expect(index).toHaveLength(MAX_PINNED_REPOS);
    // MRU order survives: the ten NEWEST repos (/repo/64../repo/73 plus
    // their later saves) stay listed, the oldest ten are gone everywhere.
    expect(index[0]).toBe("/repo/73");
    expect(index).not.toContain("/repo/9");
    expect(store.getItem(pinnedKey("/repo/9"))).toBe(null);
    expect(store.getItem(pinnedKey("/repo/73"))).not.toBe(null);
  });

  it("drops index entries whose pinned blob has already vanished", () => {
    const store = memoryStorage({ [PINNED_INDEX_KEY]: JSON.stringify(["/ghost", "/alive"]) });
    store.setItem(pinnedKey("/alive"), "[]");
    prunePinnedIndex(store);
    expect(loadPinnedIndex(store)).toEqual(["/alive"]);
    // The live blob is untouched.
    expect(store.getItem(pinnedKey("/alive"))).toBe("[]");
  });

  it("treats a corrupt index as empty: no-op, never mass-deletes blobs", () => {
    const store = memoryStorage();
    store.setItem(PINNED_INDEX_KEY, "{{{");
    for (const path of ["/a", "/b"]) store.setItem(pinnedKey(path), "[]");
    prunePinnedIndex(store);
    expect(store.getItem(pinnedKey("/a"))).toBe("[]");
    expect(store.getItem(pinnedKey("/b"))).toBe("[]");
  });

  it("ignores hostile JSON entries while pruning real ones", () => {
    const raw = `[42,null,{"p":1},["/x"],"",${JSON.stringify("/real")},"/gone"]`;
    const store = memoryStorage({ [PINNED_INDEX_KEY]: raw });
    store.setItem(pinnedKey("/real"), "[]");
    prunePinnedIndex(store);
    expect(loadPinnedIndex(store)).toEqual(["/real"]);
  });

  it("keeps failed evictions discoverable in the index (quota-safe)", () => {
    const paths = Array.from({ length: MAX_PINNED_REPOS + 3 }, (_, i) => `/r/${i}`);
    const initial: Record<string, string> = { [PINNED_INDEX_KEY]: JSON.stringify(paths) };
    for (const path of paths) initial[pinnedKey(path)] = "[]";
    const store = quotaFullStorage(initial);
    prunePinnedIndex(store);
    // Nothing could be deleted, so NOTHING may leave the index — otherwise
    // those blobs become undiscoverable forever.
    expect(loadPinnedIndex(store)).toHaveLength(paths.length);
  });

  it("never throws when storage reads/writes all fail", () => {
    expect(() => prunePinnedIndex(deadStorage())).not.toThrow();
    expect(() => prunePinnedIndex(null)).not.toThrow();
  });

  it("is idempotent: a second prune changes nothing further", () => {
    const store = seeded(MAX_PINNED_REPOS + 7);
    prunePinnedIndex(store);
    const afterFirst = store.getItem(PINNED_INDEX_KEY);
    prunePinnedIndex(store);
    expect(store.getItem(PINNED_INDEX_KEY)).toBe(afterFirst);
    expect(survivingBlobs(store)).toBe(MAX_PINNED_REPOS);
  });

  it("leaves a within-cap set byte-identical (no pointless rewrite)", () => {
    const store = seeded(5);
    const before = store.getItem(PINNED_INDEX_KEY);
    prunePinnedIndex(store);
    expect(store.getItem(PINNED_INDEX_KEY)).toBe(before);
  });
});
