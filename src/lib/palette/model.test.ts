import { describe, expect, it } from "vitest";
import { actionFailure, FRECENCY_KEY, MAX_QUERY_LENGTH, PALETTE_MODES, parsePaletteQuery, rankItems, readFrecency, recordFrecency, type PaletteStorage } from "./model";

const item = (id: string, label = id, description = "", disabledReason?: string) => ({ id, label, description, disabledReason, category: "Commands" });
function storage(raw: string | null): PaletteStorage {
  let value = raw;
  return { getItem: key => key === FRECENCY_KEY ? value : null, setItem: (_key, next) => { value = next; } };
}

describe("palette query and relevance", () => {
  it.each(PALETTE_MODES)("parses $mode and its longest prefix", entry => {
    expect(parsePaletteQuery(` ${entry.prefix} query `)).toEqual({ mode: entry.mode, text: "query", semantic: false });
  });
  it("confines TF-IDF syntax to workspace search and bounds query size", () => {
    expect(parsePaletteQuery(":: symbol ~")).toEqual({ mode: "workspace", text: "symbol", semantic: true });
    expect(parsePaletteQuery(": symbol~").text).toBe("symbol~");
    expect(parsePaletteQuery("a".repeat(10000)).text).toHaveLength(MAX_QUERY_LENGTH);
  });
  it("ranks exact and prefix matches ahead of metadata and fuzzy matches", () => {
    expect(rankItems([item("fuzzy", "f e t c h"), item("metadata", "Network", "fetch"), item("prefix", "Fetch remotes"), item("exact", "Fetch")], "fetch").map(i => i.id)).toEqual(["exact", "prefix", "metadata", "fuzzy"]);
  });
  it("matches unordered tokens across label and path without a regex", () => {
    const files = [item("a", "CommandPalette.svelte", "src/lib/components/CommandPalette.svelte"), item("b", "Palette.ts", "src/canvas/Palette.ts")];
    expect(rankItems(files, "components palette").map(i => i.id)).toEqual(["a"]);
    expect(rankItems(files, "cmdplt").map(i => i.id)).toEqual(["a"]);
    expect(rankItems(files, "(a+)+$")).toEqual([]);
  });
  it("ranks a named destination above a broad view that mentions it later", () => {
    expect(rankItems([item("code", "Open Code — explorer, editor and map"), item("map", "Open Map — subsystems and docs")], "map")[0].id).toBe("map");
  });
  it("uses recency for ties, but never lets frequent fuzzy matches outrank exact ones", () => {
    const now = 1_000_000_000;
    const history = new Map([["old", { count: 20, lastUsed: 1 }], ["recent", { count: 1, lastUsed: now }]]);
    expect(rankItems([item("old"), item("recent")], "", history, now)[0].id).toBe("recent");
    expect(rankItems([item("old", "f e t c h"), item("exact", "Fetch")], "fetch", history, now)[0].id).toBe("exact");
  });
  it("deduplicates identities, preserves stable ties, and explains disabled matches", () => {
    const list = [item("disabled", "Fetch", "", "No repository"), item("a", "Fetch all"), item("a", "duplicate"), item("b", "Fetch all")];
    expect(rankItems(list, "").map(i => i.id)).toEqual(["a", "b", "disabled"]);
    expect(rankItems(list, "fetch")[0].disabledReason).toBe("No repository");
    expect(rankItems([], "")).toEqual([]);
  });
  it("searches beyond the first page of a large file inventory", () => {
    const items = Array.from({ length: 100_000 }, (_, i) => item(String(i), `file-${i}.ts`));
    expect(rankItems(items, "file-99999.ts")[0].id).toBe("99999");
  });
});

describe("optional bounded palette history", () => {
  it.each([null, "null", "[]", "false", "malformed", '"text"', " ".repeat(100001)])("survives malformed persistence (%s)", raw => {
    expect(readFrecency(storage(raw)).size).toBe(0);
  });
  it("migrates counts and rejects malformed records without prototype inheritance", () => {
    const history = readFrecency(storage('{"legacy":4,"string":"3","negative":-2,"bad":{"count":2,"lastUsed":"now"},"future":{"count":999999,"lastUsed":9999999},"__proto__":2}'), 100);
    expect(history.get("legacy")).toEqual({ count: 4, lastUsed: 0 });
    expect(history.get("future")).toEqual({ count: 10000, lastUsed: 100 });
    expect(history.has("string")).toBe(false);
    expect(history.has("negative")).toBe(false);
    expect(history.has("bad")).toBe(false);
    expect(history.get("__proto__")?.count).toBe(2);
  });
  it("keeps only the most recent 120 entries and persists successful use", () => {
    const store = storage(JSON.stringify(Object.fromEntries(Array.from({ length: 200 }, (_, i) => [String(i), { count: 1, lastUsed: i }]))));
    let history = readFrecency(store, 1000);
    expect(history.size).toBe(120);
    history = recordFrecency(history, "fresh", store, 1000);
    expect(history.size).toBe(120);
    expect(readFrecency(store, 1000).get("fresh")).toEqual({ count: 1, lastUsed: 1000 });
  });
  it("works for the session when reads and writes throw", () => {
    const denied = { getItem: () => { throw Error("Denied"); }, setItem: () => { throw Error("Quota"); } };
    expect(readFrecency(denied).size).toBe(0);
    expect(recordFrecency(new Map(), "refresh", denied).get("refresh")?.count).toBe(1);
    expect(readFrecency(null).size).toBe(0);
  });
  it("distinguishes refusal or cancellation from completion", () => {
    expect(actionFailure({ ok: false, error: "Policy denied" })).toBe("Policy denied");
    expect(actionFailure({ ok: false })).toContain("cancelled");
    expect(actionFailure({ ok: true })).toBeNull();
    expect(actionFailure(undefined)).toBeNull();
  });
});
