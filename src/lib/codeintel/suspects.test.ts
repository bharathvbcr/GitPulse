import { describe, expect, it } from "vitest";
import { parseSuspectsPayload } from "./types";

/** A minimal valid envelope, so each test varies one thing. */
function envelope(extra: Record<string, unknown> = {}) {
  return {
    available: true,
    items: [],
    total: 0,
    shown: 0,
    truncated: false,
    source_freshness: { fresh: null, reason: "not checked" },
    ...extra,
  };
}

describe("parseSuspectsPayload", () => {
  it("throws rather than inventing an empty answer from a missing payload", () => {
    expect(() => parseSuspectsPayload(null)).toThrow(/no payload/);
    expect(() => parseSuspectsPayload([])).toThrow(/no payload/);
    // A payload with no `response` at all: the envelope parser is reached with
    // `undefined` and refuses, rather than this returning an empty success.
    expect(() => parseSuspectsPayload({})).toThrow(/no payload/);
    expect(() => parseSuspectsPayload({ response: {} })).toThrow(/omitted available/);
  });

  it("keeps a run that found nothing distinct from one that examined nothing", () => {
    const found = parseSuspectsPayload({
      response: envelope(),
      scope: { indexed_head: "abc", since: "base", cone_size: 7, blamed_symbols: 5, refusals: [] },
    });
    expect(found.response.available).toBe(true);
    expect(found.response.items).toHaveLength(0);
    expect(found.scope?.cone_size).toBe(7);

    const refused = parseSuspectsPayload({
      response: envelope({ available: false, reason: "git could not be run", items: undefined }),
      scope: null,
    });
    expect(refused.response.available).toBe(false);
    expect(refused.scope).toBeNull();
  });

  it("leaves a missing scope null instead of defaulting it to zeroes", () => {
    // `cone_size: 0` is a measurement — "the cone was empty". A run refused
    // before it started made no measurement, and defaulting would turn the
    // second into the first.
    for (const payload of [
      { response: envelope() },
      { response: envelope(), scope: null },
      { response: envelope(), scope: undefined },
    ]) {
      expect(parseSuspectsPayload(payload).scope).toBeNull();
    }
  });

  it("refuses a corrupt scope rather than reading zeroes off it", () => {
    expect(() => parseSuspectsPayload({ response: envelope(), scope: "nope" })).toThrow(
      /corrupt scope/,
    );
    expect(() => parseSuspectsPayload({ response: envelope(), scope: [] })).toThrow(
      /corrupt scope/,
    );
  });

  it("carries the refusals through, because they are why the list is a lower bound", () => {
    const parsed = parseSuspectsPayload({
      response: envelope({ walk_incomplete: "the cone was cut at depth 3" }),
      scope: {
        indexed_head: "abc",
        since: "base",
        cone_size: 7,
        blamed_symbols: 5,
        refusals: ["the cone was cut at depth 3", 42],
      },
    });
    expect(parsed.response.walk_incomplete).toContain("depth 3");
    // The non-string is dropped rather than rendered as "42".
    expect(parsed.scope?.refusals).toEqual(["the cone was cut at depth 3"]);
  });

  it("clamps nonsense counts to zero instead of propagating NaN into the UI", () => {
    const parsed = parseSuspectsPayload({
      response: envelope(),
      scope: {
        indexed_head: 7,
        since: null,
        cone_size: -3,
        blamed_symbols: "many",
        refusals: null,
      },
    });
    expect(parsed.scope).toEqual({
      indexed_head: "",
      since: "",
      cone_size: 0,
      blamed_symbols: 0,
      refusals: [],
    });
  });

  it("keeps a suspect's three-state body_changed intact", () => {
    const parsed = parseSuspectsPayload({
      response: envelope({
        items: [
          {
            commit: "abc",
            author: "Ada",
            author_time: 1,
            evidence: "body_changed",
            score: 10,
            nearest_distance: 0,
            touched: [
              { qualified_name: "a::b", file_path: "a", distance: 0, lines: 3, body_changed: true },
              { qualified_name: "a::c", file_path: "a", distance: 1, lines: 9, body_changed: false },
              { qualified_name: "a::d", file_path: "a", distance: 2, lines: 1, body_changed: null },
            ],
          },
        ],
        total: 1,
        shown: 1,
      }),
      scope: { indexed_head: "abc", since: "base", cone_size: 3, blamed_symbols: 3, refusals: [] },
    });
    const touched = parsed.response.items[0].touched;
    expect(touched.map((t) => t.body_changed)).toEqual([true, false, null]);
  });
});
