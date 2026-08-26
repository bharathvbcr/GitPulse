import { describe, expect, it } from "vitest";
import { parsePinned, pinnedKey, serializePinned } from "./pins";

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
