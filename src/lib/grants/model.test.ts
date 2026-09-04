import { describe, expect, it } from "vitest";
import type { Grant } from "./types";
import { activeGrants, grantLifecycle } from "./model";

const grantsFromManvi: Grant[] = [
  {
    id: "G-expired",
    grantor: { authority: "human", id: "bharath" },
    reason: "old exception",
    scope: {
      task_id: "TASK-1",
      rules: ["scope.unplanned"],
      paths: ["src/old.rs"],
      once: false,
    },
    issued_at: "2026-09-01T10:00:00Z",
    expires_at: "2026-09-01T11:00:00Z",
    consumed: false,
  },
  {
    id: "G-used",
    grantor: { authority: "human", id: "bharath" },
    reason: "single-use exception",
    scope: {
      task_id: "TASK-2",
      rules: ["task.forbidden_change"],
      paths: ["src/used.rs"],
      once: true,
    },
    issued_at: "2026-09-01T11:00:00Z",
    expires_at: "2026-09-01T14:00:00Z",
    consumed: true,
  },
  {
    id: "G-live",
    grantor: { authority: "human", id: "bharath" },
    reason: "current exception",
    scope: {
      task_id: "TASK-3",
      rules: ["scope.unplanned", "task.forbidden_change"],
      paths: ["src/live.rs", "docs/**"],
      once: false,
    },
    issued_at: "2026-09-01T12:00:00Z",
    expires_at: "2026-09-01T14:00:00Z",
    consumed: false,
  },
];

describe("MANVI grant lifecycle", () => {
  it("does not count a consumed but unexpired grant as active", () => {
    const now = Date.parse("2026-09-01T12:30:00Z");

    expect(activeGrants(grantsFromManvi, now).map((grant) => grant.id)).toEqual([
      "G-live",
    ]);
    expect(grantLifecycle(grantsFromManvi[0], now)).toBe("expired");
    expect(grantLifecycle(grantsFromManvi[1], now)).toBe("used");
    expect(grantLifecycle(grantsFromManvi[2], now)).toBe("active");
  });

  it("returns newest grants first without mutating the ledger order", () => {
    const input = grantsFromManvi.slice(1);

    expect(activeGrants(input, Date.parse("2026-09-01T12:30:00Z"))).toEqual([
      grantsFromManvi[2],
    ]);
    expect(input.map((grant) => grant.id)).toEqual(["G-used", "G-live"]);
  });

  it("does not treat a forward-compatible entry with no usable expiry as active", () => {
    const incomplete = {
      ...grantsFromManvi[2],
      expires_at: "",
    };

    expect(grantLifecycle(incomplete, Date.parse("2026-09-01T12:30:00Z"))).toBe(
      "unknown",
    );
    expect(activeGrants([incomplete], Date.parse("2026-09-01T12:30:00Z"))).toEqual(
      [],
    );
  });
});
