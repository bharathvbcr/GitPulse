import { describe, expect, it } from "vitest";
import { isRefScope, shortRefLabel, REF_LABEL_MAX } from "./refScope";

describe("isRefScope", () => {
  it("accepts only the two scopes the backend deserializes", () => {
    expect(isRefScope("named")).toBe(true);
    expect(isRefScope("all")).toBe(true);
    for (const bad of ["Named", "ALL", "", " named", null, undefined, 0, {}, ["all"]]) {
      expect(isRefScope(bad)).toBe(false);
    }
  });
});

describe("shortRefLabel", () => {
  it("leaves ordinary ref names untouched", () => {
    for (const name of ["main", "origin/feature", "v1.2.0", "release/2026-01"]) {
      expect(shortRefLabel(name)).toBe(name);
    }
  });

  /**
   * The case that motivated it: a real ref from this repository. Left whole it
   * stretches the commit row until the summary is pushed off-screen.
   */
  it("folds a 209-character agent checkpoint down to its namespace", () => {
    const name =
      "codex/turn-diffs/checkpoints/" +
      "146c832dd582d2f371d2d7f79aa5f0467658b5e962c28a281f2b46a1529f5c46/" +
      "3c0bd968060e6a19a71608ee26cc63d973b8dc4a8c31e9f95be0c2b68c219178/" +
      "1788535046539/ca796ac6-5927-4170-9a4b-ccadae440ddb";
    expect(name.length).toBeGreaterThan(200);
    const short = shortRefLabel(name);
    expect(short.length).toBeLessThanOrEqual(REF_LABEL_MAX);
    // The namespace survives — it is the part that answers "what is this?".
    expect(short).toBe("codex/turn-diffs/checkpoints/…");
  });

  it("never exceeds its budget, for any input", () => {
    const inputs = [
      "a".repeat(500),
      "a/".repeat(200),
      "/".repeat(64),
      "cmux/last-turn/57db0327a7aa1e33c506aa2ca75c30f9a2f366b2",
      "pull/17/head",
      "stash",
      "",
      "x",
    ];
    for (const name of inputs) {
      for (const max of [4, 8, 16, REF_LABEL_MAX, 64]) {
        const short = shortRefLabel(name, max);
        expect(short.length, `${JSON.stringify(name)} @ ${max} -> ${short}`).toBeLessThanOrEqual(
          Math.max(4, max)
        );
      }
    }
  });

  /**
   * A single unfoldable segment is cut by CODE POINT. Slicing UTF-16 units
   * would split a surrogate pair and render a replacement character — a
   * rendering bug that looks like a corrupt ref name.
   */
  it("cuts an unfoldable segment without splitting a surrogate pair", () => {
    const name = "🌱".repeat(40);
    const short = shortRefLabel(name, 10);
    expect(short).not.toContain("�");
    expect(short.endsWith("…")).toBe(true);
    // Every retained code point is a whole emoji.
    expect([...short.slice(0, -1)].every((c) => c === "🌱")).toBe(true);
  });

  it("degrades a nonsense budget instead of returning pure punctuation", () => {
    for (const max of [0, 1, -5, Number.NaN]) {
      const short = shortRefLabel("a/".repeat(50), max);
      expect(short.length).toBeGreaterThan(0);
      expect(short).not.toBe("…");
    }
  });
});
