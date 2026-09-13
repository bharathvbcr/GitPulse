import { describe, expect, it } from "vitest";
import {
  DEFAULT_HYGIENE_DEFAULTS,
  DEFAULT_RETENTION_DAYS,
  INHERITED_OVERRIDE,
  escapeGitignoreLiteral,
  ignoreRule,
  loadHygieneSettings,
  readDefaults,
  readOverride,
  resolveRetention,
  reviewDue,
  saveDefaults,
  saveOverride,
} from "./preferences";
import type { StorageLike } from "../../repos/persist";

function memory(seed: Record<string, string> = {}): StorageLike & { keys(): string[] } {
  const map = new Map<string, string>(Object.entries(seed));
  return {
    getItem: key => map.get(key) ?? null,
    setItem: (key, value) => { map.set(key, value); },
    removeItem: key => { map.delete(key); },
    keys: () => [...map.keys()],
  };
}

const legacyKey = (repo: string) => `gitpulse:hygiene:v1:${repo}`;
const legacy = (value: Record<string, unknown>) => JSON.stringify(value);

describe("host-wide hygiene defaults", () => {
  it("ships opt out, and round-trips an edit", () => {
    const store = memory();
    expect(readDefaults(store)).toEqual(DEFAULT_HYGIENE_DEFAULTS);
    expect(saveDefaults(store, { retentionDays: 14, reviewSharedCaches: true, lastSharedReview: 42 })).toBe(true);
    expect(readDefaults(store)).toEqual({ retentionDays: 14, reviewSharedCaches: true, lastSharedReview: 42 });
  });

  it("falls back field by field rather than discarding a readable record", () => {
    // A corrupt retention must not also silently clear an opt-in the user made.
    const store = memory({
      "gitpulse:hygiene:defaults:v2": legacy({ retentionDays: 0, reviewSharedCaches: true, lastSharedReview: -1 }),
    });
    expect(readDefaults(store)).toEqual({ retentionDays: DEFAULT_RETENTION_DAYS, reviewSharedCaches: true, lastSharedReview: 0 });
  });

  it("rejects corrupted and hostile persisted values", () => {
    for (const raw of ["{", "null", "7", "[]", legacy({ retentionDays: 5000, reviewSharedCaches: "true", lastSharedReview: "yesterday" })]) {
      expect(readDefaults({ ...memory(), getItem: () => raw })).toEqual(DEFAULT_HYGIENE_DEFAULTS);
    }
    expect(readDefaults(null)).toEqual(DEFAULT_HYGIENE_DEFAULTS);
    expect(saveDefaults(null, DEFAULT_HYGIENE_DEFAULTS)).toBe(false);
    expect(saveDefaults({ ...memory(), setItem: () => { throw new Error("quota"); } }, DEFAULT_HYGIENE_DEFAULTS)).toBe(false);
  });
});

describe("retention across repositories", () => {
  it("inherits the default everywhere until a repository overrides it", () => {
    const store = memory();
    saveDefaults(store, { ...DEFAULT_HYGIENE_DEFAULTS, retentionDays: 14 });
    saveOverride(store, "/b", { retentionDays: 90 });

    // Three repositories, one changed: the other two follow the default,
    // including one that has never been opened.
    const read = (repo: string) => resolveRetention(readDefaults(store), readOverride(store, repo));
    expect(read("/a")).toEqual({ days: 14, source: "default" });
    expect(read("/b")).toEqual({ days: 90, source: "repository" });
    expect(read("/c")).toEqual({ days: 14, source: "default" });
  });

  it("moves every inheriting repository when the default moves, and leaves the override alone", () => {
    const store = memory();
    saveOverride(store, "/pinned", { retentionDays: 7 });
    saveDefaults(store, { ...DEFAULT_HYGIENE_DEFAULTS, retentionDays: 90 });

    expect(resolveRetention(readDefaults(store), readOverride(store, "/inherits"))).toEqual({ days: 90, source: "default" });
    expect(resolveRetention(readDefaults(store), readOverride(store, "/pinned"))).toEqual({ days: 7, source: "repository" });
  });

  it("returns to the default when an override is cleared", () => {
    const store = memory();
    saveOverride(store, "/a", { retentionDays: 7 });
    saveOverride(store, "/a", { ...INHERITED_OVERRIDE });
    expect(resolveRetention(readDefaults(store), readOverride(store, "/a"))).toEqual({ days: DEFAULT_RETENTION_DAYS, source: "default" });
  });

  it("inherits rather than inventing a number when an override is unreadable", () => {
    const store = memory({ "gitpulse:hygiene:repo:v2:/a": legacy({ retentionDays: 99999 }) });
    saveDefaults(store, { ...DEFAULT_HYGIENE_DEFAULTS, retentionDays: 7 });
    expect(resolveRetention(readDefaults(store), readOverride(store, "/a"))).toEqual({ days: 7, source: "default" });
  });

  it("keeps one repository's override out of every other repository", () => {
    const store = memory();
    saveOverride(store, "/a", { retentionDays: 7 });
    expect(readOverride(store, "/b")).toEqual(INHERITED_OVERRIDE);
    expect(readOverride(null, "/a")).toEqual(INHERITED_OVERRIDE);
    expect(saveOverride(null, "/a", { retentionDays: 7 })).toBe(false);
  });
});

describe("the shared-cache review is host-wide", () => {
  it("reaches every repository from one opt in, with one shared timer", () => {
    const week = 7 * 86_400_000;
    const store = memory();
    saveDefaults(store, { ...DEFAULT_HYGIENE_DEFAULTS, reviewSharedCaches: true });

    // Opting in once covers repositories that were never opened...
    expect(reviewDue(loadHygieneSettings(store, "/a").defaults, week)).toBe(true);
    // ...and reviewing from one repository satisfies the rest, rather than
    // each repository keeping its own stamp and rescanning the same caches.
    saveDefaults(store, { ...readDefaults(store), lastSharedReview: week });
    expect(reviewDue(loadHygieneSettings(store, "/b").defaults, week + 1)).toBe(false);
    expect(reviewDue(loadHygieneSettings(store, "/b").defaults, week + week)).toBe(true);
  });

  it("reviews only when enabled and due, including a backward clock", () => {
    const week = 7 * 86_400_000;
    expect(reviewDue(DEFAULT_HYGIENE_DEFAULTS, week)).toBe(false);
    const on = { ...DEFAULT_HYGIENE_DEFAULTS, reviewSharedCaches: true };
    expect(reviewDue(on, week - 1)).toBe(false);
    expect(reviewDue(on, week)).toBe(true);
    expect(reviewDue({ ...on, lastSharedReview: week }, 0)).toBe(true);
  });
});

describe("adopting the per-repository records this model replaces", () => {
  it("keeps a chosen retention as an override and a default one as inheritance", () => {
    const store = memory({
      [legacyKey("/chose")]: legacy({ retentionDays: 90, weeklyReview: false, lastReview: 0 }),
      [legacyKey("/left-alone")]: legacy({ retentionDays: 30, weeklyReview: false, lastReview: 0 }),
    });
    // 30 was the old hardcoded value, so it is not evidence of a choice: that
    // repository must follow the default when the default later moves.
    expect(loadHygieneSettings(store, "/chose").override).toEqual({ retentionDays: 90 });
    expect(loadHygieneSettings(store, "/left-alone").override).toEqual(INHERITED_OVERRIDE);

    saveDefaults(store, { ...readDefaults(store), retentionDays: 7 });
    expect(resolveRetention(readDefaults(store), readOverride(store, "/left-alone"))).toEqual({ days: 7, source: "default" });
    expect(resolveRetention(readDefaults(store), readOverride(store, "/chose"))).toEqual({ days: 90, source: "repository" });
  });

  it("promotes a weekly review to the host and carries the newest stamp", () => {
    const store = memory({
      [legacyKey("/a")]: legacy({ retentionDays: 30, weeklyReview: true, lastReview: 500 }),
      [legacyKey("/b")]: legacy({ retentionDays: 30, weeklyReview: true, lastReview: 900 }),
    });
    expect(loadHygieneSettings(store, "/a").defaults.reviewSharedCaches).toBe(true);
    expect(readDefaults(store).lastSharedReview).toBe(500);
    // Adopting a second record keeps the most recent measurement, so opening
    // an older repository cannot force an immediate rescan.
    expect(loadHygieneSettings(store, "/b").defaults.lastSharedReview).toBe(900);
    expect(loadHygieneSettings(store, "/a").defaults.lastSharedReview).toBe(900);
  });

  it("cannot switch the review back on after the user turns it off", () => {
    // The resurrection this guards: adopt one repository, opt out, then open a
    // second repository whose stale record still says true.
    const store = memory({
      [legacyKey("/a")]: legacy({ retentionDays: 30, weeklyReview: true, lastReview: 0 }),
      [legacyKey("/b")]: legacy({ retentionDays: 30, weeklyReview: true, lastReview: 0 }),
    });
    loadHygieneSettings(store, "/a");
    loadHygieneSettings(store, "/b");
    saveDefaults(store, { ...readDefaults(store), reviewSharedCaches: false });

    expect(loadHygieneSettings(store, "/a").defaults.reviewSharedCaches).toBe(false);
    expect(loadHygieneSettings(store, "/b").defaults.reviewSharedCaches).toBe(false);
  });

  it("consumes the legacy record so adoption happens once", () => {
    const store = memory({ [legacyKey("/a")]: legacy({ retentionDays: 90, weeklyReview: true, lastReview: 0 }) });
    loadHygieneSettings(store, "/a");
    expect(store.keys()).not.toContain(legacyKey("/a"));
    expect(store.keys()).toContain("gitpulse:hygiene:repo:v2:/a");
  });

  it("never lets one repository's adoption rewrite another's settings", () => {
    const store = memory({ [legacyKey("/a")]: legacy({ retentionDays: 7, weeklyReview: false, lastReview: 0 }) });
    saveOverride(store, "/b", { retentionDays: 90 });
    loadHygieneSettings(store, "/a");
    expect(readOverride(store, "/b")).toEqual({ retentionDays: 90 });
  });

  it("survives a corrupt legacy record and a storage that refuses removal", () => {
    for (const raw of ["{", "null", "[]", legacy({ retentionDays: "ninety", weeklyReview: "yes" })]) {
      const store = { ...memory(), getItem: (key: string) => (key.startsWith("gitpulse:hygiene:v1:") ? raw : null) };
      expect(loadHygieneSettings(store, "/a")).toEqual({ defaults: DEFAULT_HYGIENE_DEFAULTS, override: INHERITED_OVERRIDE });
    }
    const stubborn = { ...memory({ [legacyKey("/a")]: legacy({ retentionDays: 90, weeklyReview: false, lastReview: 0 }) }), removeItem: () => { throw new Error("read only"); } };
    expect(loadHygieneSettings(stubborn, "/a").override).toEqual({ retentionDays: 90 });
    expect(loadHygieneSettings(null, "/a")).toEqual({ defaults: DEFAULT_HYGIENE_DEFAULTS, override: INHERITED_OVERRIDE });
  });

  it("prefers an adopted override to the legacy record it replaced", () => {
    const store = memory({ [legacyKey("/a")]: legacy({ retentionDays: 90, weeklyReview: false, lastReview: 0 }) });
    loadHygieneSettings(store, "/a");
    saveOverride(store, "/a", { retentionDays: 7 });
    expect(loadHygieneSettings(store, "/a").override).toEqual({ retentionDays: 7 });
  });
});

describe("literal ignore rules", () => {
  it("anchors and escapes one directory, including Git glob metacharacters", () => {
    expect(ignoreRule("src-tauri/target")).toBe("/src-tauri/target/");
    expect(ignoreRule("space !#[a]*?/cache")).toBe("/space\\ \\!\\#\\[a\\]\\*\\?/cache/");
  });
  it("doubles backslash in the sanitizer so a crafted \\* cannot undo a meta escape", () => {
    expect(escapeGitignoreLiteral("cache\\*.tmp")).toBe("cache\\\\\\*.tmp");
    expect(escapeGitignoreLiteral("*cache\\")).toBe("\\*cache\\\\");
    expect(escapeGitignoreLiteral("a\\b")).toBe("a\\\\b");
    expect(ignoreRule("cache\\*.tmp")).toBeNull();
    expect(ignoreRule("a\\b")).toBeNull();
  });
  it("refuses traversal, control characters and Git internals", () => {
    for (const path of ["", "/target", "../target", "a/./target", "a//target", "a/.git/cache", "a\\b", "target\n.env", "a\u007f", "x".repeat(4097)]) expect(ignoreRule(path)).toBeNull();
  });
});
