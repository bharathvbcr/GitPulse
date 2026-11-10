import { describe, expect, it } from "vitest";
import { DETACHED_STATUS, ENDING_STATUS } from "./detachedSessions";
import { sessionCountLabel, sessionStatusLabel } from "./sessionList";

describe("sessionCountLabel", () => {
  it("names the open sessions and the limit", () => {
    expect(sessionCountLabel(22, 50)).toBe("22 open / 50 limit");
    expect(sessionCountLabel(Number.NaN, -3)).toBe("0 open / 0 limit");
  });
});

describe("sessionStatusLabel", () => {
  it("keeps a detached or ending session's own sentence", () => {
    expect(sessionStatusLabel("running")).toBe("Running");
    expect(sessionStatusLabel(DETACHED_STATUS)).toBe(DETACHED_STATUS);
    expect(sessionStatusLabel(ENDING_STATUS)).toBe(ENDING_STATUS);
    expect(sessionStatusLabel("idle")).toBe("Idle");
    expect(sessionStatusLabel("   ")).toBe("Unknown");
  });
});
