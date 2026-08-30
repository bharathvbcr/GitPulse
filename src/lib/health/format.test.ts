import { describe, expect, it } from "vitest";
import {
  dependabotBadgeClass,
  formatAuditCounts,
  normalizeSeverity,
  severityClass,
  updateKind,
  updateKindClass,
} from "./format";

describe("health format", () => {
  it("normalizes npm and cargo severity labels", () => {
    expect(normalizeSeverity("CRITICAL")).toBe("critical");
    expect(normalizeSeverity("medium")).toBe("moderate");
    expect(normalizeSeverity("whatever")).toBe("info");
  });

  it("styles severities distinctly", () => {
    expect(severityClass("critical")).toContain("rose");
    expect(severityClass("high")).toContain("red");
    expect(severityClass("moderate")).toContain("amber");
    expect(severityClass("low")).toContain("sky");
  });

  it("classifies semver gaps between current and latest", () => {
    expect(updateKind("1.2.3", "2.0.0")).toBe("major");
    expect(updateKind("1.2.3", "1.3.0")).toBe("minor");
    expect(updateKind("1.2.3", "1.2.4")).toBe("patch");
    expect(updateKind("1.2.3", "1.2.3")).toBe("same");
    expect(updateKind("v5.6.0", "5.9.2")).toBe("minor");
    expect(updateKind("not-a-version", "1.0.0")).toBe("unknown");
    expect(
      updateKind("7.0.0-dev.20260514.1", "7.0.0-dev.20260707.2"),
    ).toBe("prerelease");
    expect(updateKind("1.0.0-rc.1", "1.0.0")).toBe("prerelease");
    expect(updateKindClass("major")).toContain("rose");
    expect(updateKindClass("patch")).toContain("sky");
  });

  it("summarises audit counts without implying a clean scan when empty", () => {
    // UPDATED: a bare zero used to read as "No known vulnerabilities", which
    // claimed a clean scan even when no scanner had run. Absence of the ran
    // signal now fails closed to "Audit did not run".
    expect(
      formatAuditCounts({ critical: 0, high: 0, moderate: 0, low: 0, total: 0 }),
    ).toBe("Audit did not run");
    expect(
      formatAuditCounts(
        { critical: 0, high: 0, moderate: 0, low: 0, total: 0 },
        { ran: false },
      ),
    ).toBe("Audit did not run");
    expect(
      formatAuditCounts(
        { critical: 0, high: 0, moderate: 0, low: 0, total: 0 },
        { complete: true, ran: true },
      ),
    ).toBe("No known vulnerabilities");
    expect(
      formatAuditCounts(
        { critical: 0, high: 0, moderate: 0, low: 0, total: 0 },
        { complete: false, ran: true },
      ),
    ).toBe("Audit incomplete");
    expect(
      formatAuditCounts({ critical: 1, high: 2, moderate: 0, low: 4, total: 7 }),
    ).toBe("1 critical · 2 high · 4 low");
  });

  it("reports unranked findings separately instead of calling them informational", () => {
    expect(
      formatAuditCounts({
        critical: 0,
        high: 0,
        moderate: 0,
        low: 0,
        unknown: 3,
        total: 3,
      }),
    ).toBe("3 unranked");
    expect(
      formatAuditCounts({
        critical: 1,
        high: 0,
        moderate: 0,
        low: 0,
        unknown: 2,
        total: 3,
      }),
    ).toBe("1 critical · 2 unranked");
  });

  it("styles unrated severities as muted, not alarming", () => {
    expect(severityClass("unknown")).toBe(severityClass("info"));
    expect(normalizeSeverity("unknown")).toBe("info");
  });
});

describe("health format — audit coverage honesty (regression)", () => {
  it("never renders a known-incomplete audit as a bare finding count", () => {
    // A capped/partial scan that happens to have findings must still say so.
    // Previously `complete` was consulted only when total === 0, so an
    // incomplete scan with findings rendered identically to a full one.
    const summary = { critical: 1, high: 2, moderate: 0, low: 4, total: 7 };
    const incomplete = formatAuditCounts(summary, { complete: false, ran: true });
    const complete = formatAuditCounts(summary, { complete: true, ran: true });

    expect(complete).toBe("1 critical · 2 high · 4 low");
    expect(incomplete).not.toBe(complete);
    expect(incomplete).toContain("1 critical");
    expect(incomplete).toMatch(/incomplete/i);
  });

  it("keeps the bare count when the caller states no coverage opinion", () => {
    // Callers that pass no options get the unchanged legacy rendering.
    expect(
      formatAuditCounts({ critical: 1, high: 2, moderate: 0, low: 4, total: 7 }),
    ).toBe("1 critical · 2 high · 4 low");
  });
});

describe("dependabotBadgeClass — severity casing (regression)", () => {
  it("ranks GitHub severities case-insensitively, as the Rust parser does", () => {
    // github/mod.rs passes `security_vulnerability.severity` through verbatim
    // and only lowercases for ranking, so "HIGH"/"Critical" reach the UI.
    const lower = dependabotBadgeClass([{ severity: "high" }]);
    expect(lower).toBe("text-rose-300");
    expect(dependabotBadgeClass([{ severity: "HIGH" }])).toBe(lower);
    expect(dependabotBadgeClass([{ severity: "Critical" }])).toBe(lower);
    expect(dependabotBadgeClass([{ severity: " high " }])).toBe(lower);
  });

  it("maps GitHub's 'medium' onto the moderate tier and leaves the rest muted", () => {
    expect(dependabotBadgeClass([{ severity: "medium" }])).toBe("text-amber-300");
    expect(dependabotBadgeClass([{ severity: "MEDIUM" }])).toBe("text-amber-300");
    expect(dependabotBadgeClass([{ severity: "low" }])).toBe("text-sky-300");
    expect(dependabotBadgeClass([{ severity: "" }])).toBe("text-sky-300");
    expect(dependabotBadgeClass([])).toBe("");
  });

  it("takes the worst severity in the list, not the first", () => {
    expect(
      dependabotBadgeClass([{ severity: "low" }, { severity: "CRITICAL" }]),
    ).toBe("text-rose-300");
  });
});
