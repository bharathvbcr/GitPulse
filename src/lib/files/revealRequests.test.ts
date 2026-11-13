import { beforeEach, describe, expect, it } from "vitest";
import { get } from "svelte/store";
import {
  clearReveal,
  consumeReveal,
  goToLineTarget,
  requestReveal,
  revealRequest,
  MAX_REVEAL_LINE,
  MAX_REVEAL_PATH,
  REVEAL_TTL_MS,
} from "./revealRequests";

describe("reveal requests", () => {
  beforeEach(() => clearReveal());

  it("hands the request to the file that asked for it", () => {
    expect(requestReveal("src/a.ts", 12, 5, 1_000)).toBe(true);
    expect(consumeReveal("src/a.ts", 1_100)).toMatchObject({ path: "src/a.ts", line: 12, column: 5 });
  });

  it("is collected exactly once", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    expect(consumeReveal("src/a.ts", 1_000)).not.toBeNull();
    expect(consumeReveal("src/a.ts", 1_000)).toBeNull();
  });

  it("will not let a different file swallow it", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    expect(consumeReveal("src/b.ts", 1_000)).toBeNull();
    // Still waiting for the file it named.
    expect(consumeReveal("src/a.ts", 1_000)).toMatchObject({ line: 3 });
  });

  it("keeps only the newest request", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    requestReveal("src/b.ts", 9, null, 1_010);
    expect(consumeReveal("src/a.ts", 1_020)).toBeNull();
    expect(consumeReveal("src/b.ts", 1_020)).toMatchObject({ line: 9 });
  });

  it("expires rather than firing later on an unrelated open", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    expect(consumeReveal("src/a.ts", 1_000 + REVEAL_TTL_MS + 1)).toBeNull();
    // And the expired request is gone, not merely skipped this once.
    expect(get(revealRequest)).toBeNull();
  });

  it("still collects at the edge of the window", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    expect(consumeReveal("src/a.ts", 1_000 + REVEAL_TTL_MS)).not.toBeNull();
  });

  it("refuses a request it cannot act on, without throwing", () => {
    expect(requestReveal("src/a.ts", null)).toBe(false);
    expect(requestReveal("", 3)).toBe(false);
    expect(requestReveal("a".repeat(MAX_REVEAL_PATH + 1), 3)).toBe(false);
    expect(requestReveal("src/a.ts", 0)).toBe(false);
    expect(requestReveal("src/a.ts", -1)).toBe(false);
    expect(requestReveal("src/a.ts", 1.5)).toBe(false);
    expect(requestReveal("src/a.ts", Number.NaN)).toBe(false);
    expect(requestReveal("src/a.ts", Number.POSITIVE_INFINITY)).toBe(false);
    expect(requestReveal("src/a.ts", MAX_REVEAL_LINE + 1)).toBe(false);
    // A refusal leaves nothing behind for a later file to pick up.
    expect(get(revealRequest)).toBeNull();
  });

  it("drops an unusable column but keeps the line", () => {
    requestReveal("src/a.ts", 4, 0, 1_000);
    expect(consumeReveal("src/a.ts", 1_000)).toMatchObject({ line: 4, column: null });
    requestReveal("src/a.ts", 4, 2.5, 1_000);
    expect(consumeReveal("src/a.ts", 1_000)).toMatchObject({ line: 4, column: null });
  });

  it("clears on demand", () => {
    requestReveal("src/a.ts", 3, null, 1_000);
    clearReveal();
    expect(consumeReveal("src/a.ts", 1_000)).toBeNull();
  });
});

describe("go-to-line target", () => {
  it("takes the number a number input binds", () => {
    // `<input type="number" bind:value>` hands over a number, not text: the
    // old handler called `.trim()` on it and threw on every Go.
    expect(goToLineTarget(42, 100)).toBe(42);
  });

  it("takes typed text, trimmed", () => {
    expect(goToLineTarget(" 7 ", 100)).toBe(7);
  });

  it("treats a cleared field as nothing to go to", () => {
    // A number input that is emptied binds null, not "".
    expect(goToLineTarget(null, 100)).toBeNull();
    expect(goToLineTarget(undefined, 100)).toBeNull();
    expect(goToLineTarget("", 100)).toBeNull();
    expect(goToLineTarget("   ", 100)).toBeNull();
  });

  it("refuses what is not a whole line number", () => {
    expect(goToLineTarget(0, 100)).toBeNull();
    expect(goToLineTarget(-3, 100)).toBeNull();
    expect(goToLineTarget(2.5, 100)).toBeNull();
    expect(goToLineTarget(Number.NaN, 100)).toBeNull();
    expect(goToLineTarget("12abc", 100)).toBeNull();
    expect(goToLineTarget("1e3", 100)).toBeNull();
  });

  it("clamps a line past the end to the last line, as a reveal does", () => {
    expect(goToLineTarget(500, 100)).toBe(100);
    expect(goToLineTarget(Number.POSITIVE_INFINITY, 100)).toBeNull();
  });

  it("has nowhere to go in an empty file", () => {
    expect(goToLineTarget(1, 0)).toBeNull();
  });
});
