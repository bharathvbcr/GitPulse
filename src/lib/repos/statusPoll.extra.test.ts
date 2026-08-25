import { describe, expect, it } from "vitest";
import { shouldRunStatusPoll, type PollGateInput } from "./statusPoll";

/** Same gate, but fed raw unknowns to probe JS truthiness coercion. */
function gate(raw: {
  hidden?: unknown;
  hasSession?: unknown;
  isLoading?: unknown;
  inflight?: unknown;
}): boolean {
  return shouldRunStatusPoll(raw as unknown as PollGateInput);
}

describe("shouldRunStatusPoll extra edges", () => {
  it("fails closed when every field is undefined", () => {
    expect(gate({})).toBe(false);
    expect(gate({ hidden: false })).toBe(false);
    expect(gate({ hasSession: true })).toBe(true); // others default falsy → runs
  });

  it("does not fall for string '0' being a falsy-sounding trap", () => {
    // '0' is TRUTHY in JS: a hidden flag carrying "0" must still block…
    expect(gate({ hidden: "0", hasSession: true, isLoading: false, inflight: false })).toBe(false);
    expect(gate({ isLoading: "0", hasSession: true, hidden: false, inflight: false })).toBe(false);
    expect(gate({ inflight: "0", hasSession: true, hidden: false, isLoading: false })).toBe(false);
    // …while "" is genuinely falsy and must NOT block.
    expect(gate({ hidden: "", hasSession: true, isLoading: false, inflight: false })).toBe(true);
    expect(gate({ isLoading: "", hasSession: true, hidden: false, inflight: false })).toBe(true);
    expect(gate({ inflight: "", hasSession: true, hidden: false, isLoading: false })).toBe(true);
  });

  it("treats null blockers as absent, matching !-coercion semantics", () => {
    expect(gate({ hidden: null, hasSession: true, isLoading: null, inflight: null })).toBe(true);
  });

  it("matches the reference predicate across all 16 boolean combinations", () => {
    const flags = [false, true];
    for (const hasSession of flags) {
      for (const hidden of flags) {
        for (const isLoading of flags) {
          for (const inflight of flags) {
            const input = { hasSession, hidden, isLoading, inflight };
            const expected =
              input.hasSession && !input.hidden && !input.isLoading && !input.inflight;
            expect(shouldRunStatusPoll(input)).toBe(expected);
          }
        }
      }
    }
  });
});
