import { beforeEach, describe, expect, it } from "vitest";
import { memoryStorage, type StorageLike } from "../repos/persist";
import {
  clearRepoHistory,
  COALESCE_WINDOW_MS,
  deltaOver,
  deltaVsPrevious,
  historyFor,
  loadHistory,
  MAX_HISTORY_PER_REPO,
  MAX_REPOS_WITH_HISTORY,
  recordSnapshot,
  saveHistory,
  STORAGE_KEY_STORAGE_HISTORY,
  type StorageHistoryMap,
  type StorageSnapshot,
} from "./history";

/** Realistic epoch-ms base so every timestamp passes production validation. */
const BASE = Date.UTC(2026, 7, 25, 12, 0, 0);

function snap(t: number, grand: number, git = 0, build = 0, cache = 0): StorageSnapshot {
  return { t, grand, git, build, cache };
}

let storage: StorageLike;

beforeEach(() => {
  storage = memoryStorage();
});

describe("loadHistory", () => {
  it("returns empty for missing storage or key", () => {
    expect(loadHistory(null)).toEqual({});
    expect(loadHistory(storage)).toEqual({});
  });

  it("round-trips through save", () => {
    const map = recordSnapshot(recordSnapshot({}, "a", snap(BASE, 10)), "a", snap(BASE + 1, 20));
    saveHistory(storage, map);
    expect(loadHistory(storage)).toEqual(map);
  });

  it("sanitizes corrupt JSON to empty", () => {
    storage.setItem(STORAGE_KEY_STORAGE_HISTORY, "{not json");
    expect(loadHistory(storage)).toEqual({});
  });

  it("drops malformed snapshots but keeps valid siblings", () => {
    storage.setItem(
      STORAGE_KEY_STORAGE_HISTORY,
      JSON.stringify({
        repo: [
          { t: BASE + 1, grand: 5, git: 1, build: 2, cache: 2 },
          { t: -3, grand: 9 },
          { t: "nope", grand: 9, git: 0, build: 0, cache: 0 },
          null,
          { t: Number.NaN, grand: 4, git: 0, build: 0, cache: 0 },
          { t: BASE + 2, grand: 7, git: 0, build: 7, cache: 0 },
        ],
      }),
    );
    const loaded = loadHistory(storage);
    expect(loaded.repo.map((s) => s.t)).toEqual([BASE + 1, BASE + 2]);
  });

  it("deduplicates identical timestamps instead of double-plotting them", () => {
    storage.setItem(
      STORAGE_KEY_STORAGE_HISTORY,
      JSON.stringify({
        repo: [
          { t: BASE, grand: 5, git: 0, build: 0, cache: 0 },
          { t: BASE, grand: 9, git: 0, build: 0, cache: 0 },
        ],
      }),
    );
    expect(historyFor(loadHistory(storage), "repo").length).toBe(1);
  });
});

describe("recordSnapshot", () => {
  it("appends chronologically regardless of insertion order", () => {
    let map: StorageHistoryMap = {};
    map = recordSnapshot(map, "r", snap(BASE + 2000, 2));
    map = recordSnapshot(map, "r", snap(BASE + 1000, 1));
    expect(historyFor(map, "r").map((s) => s.t)).toEqual([BASE + 1000, BASE + 2000]);
  });

  it("coalesces rescans inside the window instead of stacking", () => {
    let map = recordSnapshot({}, "r", snap(BASE, 100));
    map = recordSnapshot(map, "r", snap(BASE + COALESCE_WINDOW_MS - 1, 140));
    const series = historyFor(map, "r");
    expect(series.length).toBe(1);
    expect(series[0].grand).toBe(140);
  });

  it("keeps distinct points outside the window", () => {
    let map = recordSnapshot({}, "r", snap(BASE, 100));
    map = recordSnapshot(map, "r", snap(BASE + COALESCE_WINDOW_MS, 150));
    expect(historyFor(map, "r").length).toBe(2);
  });

  it("never grows past the per-repo cap; the newest entries survive", () => {
    let map: StorageHistoryMap = {};
    const total = MAX_HISTORY_PER_REPO + 25;
    for (let i = 0; i < total; i += 1) {
      // Spaced beyond coalescing so every point is distinct.
      map = recordSnapshot(map, "r", snap(BASE + i * COALESCE_WINDOW_MS * 2, i));
    }
    const series = historyFor(map, "r");
    expect(series.length).toBe(MAX_HISTORY_PER_REPO);
    expect(series[series.length - 1].t).toBe(BASE + (total - 1) * COALESCE_WINDOW_MS * 2);
    // The oldest 25 fell off the front.
    expect(series[0].t).toBe(BASE + 25 * COALESCE_WINDOW_MS * 2);
  });

  it("ignores empty repo keys", () => {
    expect(recordSnapshot({}, "", snap(1, 1))).toEqual({});
  });
});

describe("repo cap", () => {
  it("evicts exactly the oldest-inserted repo once saturated", () => {
    let map: StorageHistoryMap = {};
    for (let i = 0; i < MAX_REPOS_WITH_HISTORY + 10; i += 1) {
      map = recordSnapshot(map, `repo-${i}`, snap(BASE + i * 1_000_000_000, i));
    }
    const keys = Object.keys(map);
    expect(keys.length).toBe(MAX_REPOS_WITH_HISTORY);
    expect(keys[0]).toBe(`repo-${10}`);
    expect(keys[keys.length - 1]).toBe(`repo-${MAX_REPOS_WITH_HISTORY + 9}`);
    // No gaps: saturation slides the window one key at a time.
    for (let i = 0; i < keys.length; i += 1) {
      expect(keys[i]).toBe(`repo-${10 + i}`);
    }
    // And the cap survives a persistence round-trip.
    saveHistory(storage, map);
    expect(Object.keys(loadHistory(storage)).length).toBe(MAX_REPOS_WITH_HISTORY);
  });
});

describe("deltas", () => {
  it("deltaVsPrevious needs two points", () => {
    expect(deltaVsPrevious([])).toBeNull();
    expect(deltaVsPrevious([snap(BASE, 10)])).toBeNull();
    const delta = deltaVsPrevious([snap(BASE, 10), snap(BASE + 1, 40)]);
    expect(delta).toEqual({ bytes: 30, sinceMs: 1 });
  });

  it("deltaOver looks back across the requested window", () => {
    const now = Date.now();
    const series = [
      snap(now - 10 * 86_400_000, 100),
      snap(now - 2 * 86_400_000, 300),
      snap(now - 1_000, 500),
    ];
    const week = deltaOver(series, 7 * 86_400_000, now);
    expect(week?.bytes).toBe(200);
    const allTime = deltaOver(series, 365 * 86_400_000, now);
    expect(allTime?.bytes).toBe(400);
    // Stale series (newest older than the window) yields nothing.
    expect(
      deltaOver([snap(now - 30 * 86_400_000, 1), snap(now - 29 * 86_400_000, 2)], 86_400_000, now),
    ).toBeNull();
  });

  it("deltaOver rejects degenerate windows", () => {
    expect(deltaOver([snap(1, 1), snap(2, 2)], 0)).toBeNull();
  });
});

describe("clearRepoHistory", () => {
  it("removes only the target repo", () => {
    let map = recordSnapshot(recordSnapshot({}, "a", snap(BASE, 1)), "b", snap(BASE + 1, 2));
    map = clearRepoHistory(map, "a");
    expect(Object.keys(map)).toEqual(["b"]);
    expect(clearRepoHistory(map, "missing")).toBe(map);
  });
});
