import { describe, expect, it } from "vitest";
import { DEFAULT_PREFERENCES, ignoreRule, readPreferences, reviewDue, savePreferences } from "./preferences";
import type { StorageLike } from "../../repos/persist";

function memory(): StorageLike {
  const map = new Map<string, string>();
  return { getItem: key => map.get(key) ?? null, setItem: (key, value) => { map.set(key, value); }, removeItem: key => { map.delete(key); } };
}

describe("hygiene preferences", () => {
  it("is opt in and isolated per repository", () => {
    const store = memory();
    expect(readPreferences(store, "/repo")).toEqual(DEFAULT_PREFERENCES);
    expect(savePreferences(store, "/repo", { retentionDays: 90, weeklyReview: true, lastReview: 42 })).toBe(true);
    expect(readPreferences(store, "/repo").retentionDays).toBe(90);
    expect(readPreferences(store, "/other")).toEqual(DEFAULT_PREFERENCES);
  });
  it("rejects corrupted and hostile persisted values", () => {
    for (const raw of ["{", "null", "7", JSON.stringify({ retentionDays: 0, weeklyReview: "true", lastReview: -1 }), JSON.stringify({ retentionDays: 5000, lastReview: "yesterday" })]) {
      const store = { ...memory(), getItem: () => raw };
      expect(readPreferences(store, "/repo")).toEqual(DEFAULT_PREFERENCES);
    }
    expect(readPreferences(null, "/repo")).toEqual(DEFAULT_PREFERENCES);
    expect(savePreferences(null, "/repo", DEFAULT_PREFERENCES)).toBe(false);
    const blocked = { ...memory(), setItem: () => { throw new Error("quota"); } };
    expect(savePreferences(blocked, "/repo", DEFAULT_PREFERENCES)).toBe(false);
  });
  it("reviews only when enabled and due, including a backward clock", () => {
    const week = 7 * 86_400_000;
    expect(reviewDue(DEFAULT_PREFERENCES, week)).toBe(false);
    const prefs = { ...DEFAULT_PREFERENCES, weeklyReview: true };
    expect(reviewDue(prefs, week - 1)).toBe(false);
    expect(reviewDue(prefs, week)).toBe(true);
    expect(reviewDue({ ...prefs, lastReview: week }, 0)).toBe(true);
  });
});

describe("literal ignore rules", () => {
  it("anchors and escapes one directory, including Git glob metacharacters", () => {
    expect(ignoreRule("src-tauri/target")).toBe("/src-tauri/target/");
    expect(ignoreRule("space !#[a]*?/cache")).toBe("/space\\ \\!\\#\\[a\\]\\*\\?/cache/");
  });
  it("refuses traversal, control characters and Git internals", () => {
    for (const path of ["", "/target", "../target", "a/./target", "a//target", "a/.git/cache", "a\\b", "target\n.env", "a\u007f", "x".repeat(4097)]) expect(ignoreRule(path)).toBeNull();
  });
});
