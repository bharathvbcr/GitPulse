import { describe, expect, it } from "vitest";
import {
  AUTO_FETCH_STORAGE_KEY,
  BACKGROUND_MULTIPLIER,
  MAX_AUTO_FETCH_MINUTES,
  MAX_AUTO_FETCH_REPOS,
  MIN_AUTO_FETCH_MINUTES,
  clampMinutes,
  createAutoFetchScheduler,
  loadAutoFetchPrefs,
  parseAutoFetchOutcome,
  pickDueRepository,
  saveAutoFetchPrefs,
  withAutoFetchSetting,
  type AutoFetchCandidate,
  type AutoFetchPrefs,
  type StorageLike,
} from "./autoFetch";

const MINUTE = 60_000;

function memoryStorage(initial: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
  const data = { ...initial };
  return { data, getItem: (key) => data[key] ?? null, setItem: (key, value) => { data[key] = value; } };
}

describe("auto-fetch preferences", () => {
  it("is off for every repository until someone turns it on", () => {
    expect(loadAutoFetchPrefs(memoryStorage())).toEqual({});
    expect(loadAutoFetchPrefs(null)).toEqual({});
  });

  it("round-trips, keeps only enabled repositories, and clamps the interval", () => {
    const storage = memoryStorage();
    let prefs = withAutoFetchSetting({}, "/a", { enabled: true, minutes: 1 });
    prefs = withAutoFetchSetting(prefs, "/b", { enabled: true, minutes: 10_000 });
    prefs = withAutoFetchSetting(prefs, "/c", { enabled: false, minutes: 10 });
    saveAutoFetchPrefs(prefs, storage);
    expect(loadAutoFetchPrefs(storage)).toEqual({
      "/a": { enabled: true, minutes: MIN_AUTO_FETCH_MINUTES },
      "/b": { enabled: true, minutes: MAX_AUTO_FETCH_MINUTES },
    });
    expect(withAutoFetchSetting(prefs, "/a", { enabled: false, minutes: 10 })).not.toHaveProperty("/a");
  });

  it("reads anything malformed as off rather than on", () => {
    for (const raw of ["not json", "[]", "null", JSON.stringify({ "/a": { enabled: "yes" } }), JSON.stringify({ "/a": null })]) {
      expect(loadAutoFetchPrefs(memoryStorage({ [AUTO_FETCH_STORAGE_KEY]: raw }))).toEqual({});
    }
    expect(clampMinutes(Number.NaN)).toBe(10);
  });

  it("refuses to grow past its bound", () => {
    let prefs: AutoFetchPrefs = {};
    for (let i = 0; i < MAX_AUTO_FETCH_REPOS; i += 1) prefs = withAutoFetchSetting(prefs, `/r${i}`, { enabled: true, minutes: 10 });
    expect(() => withAutoFetchSetting(prefs, "/one-more", { enabled: true, minutes: 10 })).toThrow(/at most/);
    expect(() => withAutoFetchSetting(prefs, "/r0", { enabled: true, minutes: 30 })).not.toThrow();
  });
});

describe("pickDueRepository", () => {
  const prefs: AutoFetchPrefs = { "/front": { enabled: true, minutes: 10 }, "/back": { enabled: true, minutes: 10 } };
  const tabs: AutoFetchCandidate[] = [
    { path: "/front", active: true, skip: null },
    { path: "/back", active: false, skip: null },
    { path: "/off", active: false, skip: null },
  ];

  it("never fetches a repository the moment it is first seen", () => {
    expect(pickDueRepository(tabs, prefs, new Map(), 10 * MINUTE)).toBeNull();
  });

  it("fetches the active tab every interval and background tabs less often", () => {
    const last = new Map([["/front", 0], ["/back", 0]]);
    expect(pickDueRepository(tabs, prefs, last, 9 * MINUTE)).toBeNull();
    expect(pickDueRepository(tabs, prefs, last, 10 * MINUTE)).toBe("/front");
    const backgroundPeriod = 10 * MINUTE * BACKGROUND_MULTIPLIER;
    const frontJustRan = new Map([["/front", backgroundPeriod - MINUTE], ["/back", 0]]);
    expect(pickDueRepository(tabs, prefs, frontJustRan, backgroundPeriod - 1)).toBeNull();
    expect(pickDueRepository(tabs, prefs, frontJustRan, backgroundPeriod)).toBe("/back");
  });

  it("skips repositories that are off, parked or busy", () => {
    const last = new Map([["/front", 0], ["/back", 0], ["/off", 0]]);
    const parked = tabs.map((tab) => ({ ...tab, skip: tab.active ? "rebase in progress" : null }));
    expect(pickDueRepository(parked, prefs, last, 100 * MINUTE)).toBe("/back");
    expect(pickDueRepository(tabs.filter((t) => t.path === "/off"), prefs, last, 1e9)).toBeNull();
  });
});

describe("createAutoFetchScheduler", () => {
  it("fetches at most one repository per tick and survives a failure", async () => {
    let now = 0;
    const fetched: string[] = [];
    const errors: string[] = [];
    const scheduler = createAutoFetchScheduler({
      now: () => now,
      prefs: () => ({ "/a": { enabled: true, minutes: 5 }, "/b": { enabled: true, minutes: 5 } }),
      candidates: () => [
        { path: "/a", active: true, skip: null },
        { path: "/b", active: true, skip: null },
      ],
      fetch: async (path) => {
        fetched.push(path);
        if (path === "/a") throw new Error("offline");
        return { status: "fetched" };
      },
      onError: (path) => errors.push(path),
    });
    await scheduler.tick();
    expect(fetched).toEqual([]);
    now = 5 * MINUTE;
    await scheduler.tick();
    expect(fetched).toHaveLength(1);
    await scheduler.tick();
    expect(fetched).toHaveLength(2);
    expect(new Set(fetched)).toEqual(new Set(["/a", "/b"]));
    expect(errors).toEqual(["/a"]);
    await scheduler.tick();
    expect(fetched).toHaveLength(2);
  });

  it("does nothing at all when no repository opted in", async () => {
    let calls = 0;
    const scheduler = createAutoFetchScheduler({
      now: () => 1e12,
      prefs: () => ({}),
      candidates: () => [{ path: "/a", active: true, skip: null }],
      fetch: async () => { calls += 1; return { status: "fetched" }; },
    });
    await scheduler.tick();
    await scheduler.tick();
    expect(calls).toBe(0);
    expect(scheduler.lastAttempt.size).toBe(0);
  });
});

describe("parseAutoFetchOutcome", () => {
  it("accepts the two shapes the backend sends and nothing else", () => {
    expect(parseAutoFetchOutcome({ status: "fetched" })).toEqual({ status: "fetched" });
    expect(parseAutoFetchOutcome({ status: "skipped", reason: "busy" })).toEqual({ status: "skipped", reason: "busy" });
    expect(() => parseAutoFetchOutcome({ status: "skipped" })).toThrow();
    expect(() => parseAutoFetchOutcome("fetched")).toThrow();
  });
});
