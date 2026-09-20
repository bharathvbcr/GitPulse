import { describe, expect, it } from "vitest";
import { parseCodeintelResponse, parseCodeintelStatus } from "./types";

describe("parseCodeintelResponse", () => {
  it("throws on null, arrays, and non-objects", () => {
    expect(() => parseCodeintelResponse(null)).toThrow(/no payload/);
    expect(() => parseCodeintelResponse(undefined)).toThrow(/no payload/);
    expect(() => parseCodeintelResponse([])).toThrow(/no payload/);
  });

  it("throws when available is true without items", () => {
    expect(() => parseCodeintelResponse({ available: true, truncated: false })).toThrow(
      /without items/,
    );
  });

  it("does not invent an empty success from a missing available flag", () => {
    expect(() => parseCodeintelResponse({})).toThrow(/omitted available/);
  });

  it("normalises an unavailable payload to an empty items list", () => {
    const parsed = parseCodeintelResponse<{ n: string }>({
      available: false,
      reason: "stale",
      source_freshness: { fresh: null, reason: "this query did not verify whole-tree freshness" },
    });
    expect(parsed.available).toBe(false);
    expect(parsed.items).toEqual([]);
    expect(parsed.truncated).toBe(false);
    expect(parsed.source_freshness).toEqual({
      fresh: null,
      reason: "this query did not verify whole-tree freshness",
    });
  });

  it("refuses an unavailable payload that omitted source_freshness", () => {
    expect(() => parseCodeintelResponse({ available: false, reason: "stale" })).toThrow(
      /source_freshness must be an object/,
    );
  });

  it("refuses a boolean source_freshness, which would collapse unverified into verified", () => {
    expect(() =>
      parseCodeintelResponse({
        available: false,
        source_freshness: true,
      }),
    ).toThrow(/source_freshness must be an object/);
  });

  it("keeps a verified freshness object, including generation", () => {
    const parsed = parseCodeintelResponse<{ n: string }>({
      available: true,
      items: [],
      truncated: false,
      source_freshness: { fresh: true, generation_id: 12 },
    });
    expect(parsed.source_freshness).toEqual({ fresh: true, generation_id: 12 });
  });
});

describe("parseCodeintelResponse folds walk_incomplete at the boundary", () => {
  const ESSAY =
    "the walk did not complete: stopped at depth 10; the result is a lower bound, not the full " +
    "blast radius; 16977 of 54269 unresolved attribution site(s) have no indexed target after " +
    "excluding 37292 site(s) classified as builtin, runtime-global, external-import, " +
    "no-namesake, or module-path; classification does not prove complete source coverage; these " +
    "repository-wide counts are not specific to this target, so this answer may omit callers";

  const FRESHNESS = { fresh: null, reason: "this query did not verify whole-tree freshness" };

  const ok = (walk?: unknown) =>
    parseCodeintelResponse({
      source_freshness: FRESHNESS,
      available: true,
      items: [],
      truncated: false,
      ...(walk === undefined ? {} : { walk_incomplete: walk }),
    });

  it("bounds an unbounded engine essay before any panel can render it", () => {
    // Four panels read this field; three of them used to print it verbatim.
    // Folding here is what makes forgetting impossible at the fourth.
    const repeated = Array.from({ length: 200 }, (_, i) => `${ESSAY}; ${i} edges unrecorded`).join(
      " · ",
    );
    const parsed = ok(repeated);
    expect(parsed.walk_incomplete!.length).toBeLessThanOrEqual(720);
    // One copy of the corpus clause, not two hundred.
    expect(parsed.walk_incomplete!.split("unresolved attribution").length - 1).toBeLessThanOrEqual(
      1,
    );
  });

  it("NEVER folds a non-empty qualification into silence", () => {
    // Prose with no letters or digits yields no usable clause. Dropping it
    // would turn "this answer is qualified" into "this answer is complete",
    // which is the one thing the field exists to prevent.
    for (const odd of ["---", "···", "!!!", "…", "-;-;-"]) {
      const parsed = ok(odd);
      expect(parsed.walk_incomplete, `dropped a qualification for ${JSON.stringify(odd)}`)
        .toBeTruthy();
    }
  });

  it("treats genuinely empty prose as the absence of a qualification", () => {
    for (const empty of ["", "   ", "\n\t "]) {
      expect(ok(empty).walk_incomplete).toBeUndefined();
    }
    expect(ok(undefined).walk_incomplete).toBeUndefined();
  });

  it("is idempotent, so callers that already fold keep working", () => {
    const once = ok(ESSAY).walk_incomplete!;
    const twice = ok(once).walk_incomplete!;
    expect(twice).toBe(once);
  });

  it("still refuses a corrupt walk_incomplete rather than folding it", () => {
    expect(() => ok(42)).toThrow(/corrupt walk_incomplete/);
    expect(() => ok({})).toThrow(/corrupt walk_incomplete/);
  });

  it("bounds an engine reason without altering a short one", () => {
    const short = "package-lock.json has no indexed traversal start";
    expect(
      parseCodeintelResponse({ source_freshness: FRESHNESS, available: false, reason: short })
        .reason,
    ).toBe(short);

    const long = "x".repeat(5_000);
    const bounded = parseCodeintelResponse({
      source_freshness: FRESHNESS,
      available: false,
      reason: long,
    }).reason!;
    expect(bounded.length).toBeLessThanOrEqual(720);
    // The clip must say it clipped; a silently shortened reason reads as the
    // whole reason.
    expect(bounded.endsWith("…")).toBe(true);
  });

  it("keeps a null reason null rather than inventing an empty one", () => {
    expect(
      parseCodeintelResponse({ source_freshness: FRESHNESS, available: false, reason: null })
        .reason,
    ).toBeNull();
    expect(
      parseCodeintelResponse({ source_freshness: FRESHNESS, available: false }).reason,
    ).toBeNull();
  });

  it("does not disturb the rest of the response", () => {
    const parsed = parseCodeintelResponse({
      source_freshness: FRESHNESS,
      available: true,
      items: [1, 2, 3],
      truncated: true,
      total: 9,
      shown: 3,
      reason: null,
      walk_incomplete: ESSAY,
    });
    expect(parsed.items).toHaveLength(3);
    expect(parsed.total).toBe(9);
    expect(parsed.shown).toBe(3);
    expect(parsed.truncated).toBe(true);
  });
});

describe("parseCodeintelStatus", () => {
  it("throws on null", () => {
    expect(() => parseCodeintelStatus(null)).toThrow(/no payload/);
  });

  it("throws when available is true without db_path", () => {
    expect(() => parseCodeintelStatus({ available: true })).toThrow(/without db_path/);
  });

  it("accepts an unavailable status with an empty path", () => {
    const parsed = parseCodeintelStatus({ available: false, reason: "missing" });
    expect(parsed.available).toBe(false);
    expect(parsed.db_path).toBe("");
    expect(parsed.reason).toBe("missing");
  });

  it("bounds its two prose fields, which are long in practice", () => {
    // The real analyzer-freshness reason runs to a couple of hundred
    // characters; nothing caps what the backend may send.
    const parsed = parseCodeintelStatus({
      available: false,
      reason: "y".repeat(5_000),
      freshness_reason: "z".repeat(5_000),
    });
    expect(parsed.reason!.length).toBeLessThanOrEqual(720);
    expect(parsed.freshness_reason!.length).toBeLessThanOrEqual(720);
    expect(parsed.reason!.endsWith("…")).toBe(true);
    expect(parsed.freshness_reason!.endsWith("…")).toBe(true);
  });

  it("keeps a real analyzer-freshness sentence intact", () => {
    const real =
      "analyzer freshness unverified: whether the stored payload is current cannot be decided " +
      "by this build: the answer is the compiled grammar version for \"markdown\", and this " +
      "binary was built without the parsing frontend. Build with `--features parse` to ask.";
    expect(parseCodeintelStatus({ available: true, db_path: "/x", freshness_reason: real })
      .freshness_reason).toBe(real);
  });

  it("does not collapse an absent freshness_reason into null", () => {
    // "the backend said nothing" is not "the backend said null"; flattening
    // the two makes an unasked question look answered.
    expect(parseCodeintelStatus({ available: true, db_path: "/x" }).freshness_reason).toBeUndefined();
    expect(
      parseCodeintelStatus({ available: true, db_path: "/x", freshness_reason: null })
        .freshness_reason,
    ).toBeNull();
  });
});
