import { describe, expect, it } from "vitest";
import {
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
        { ran: true },
      ),
    ).toBe("No known vulnerabilities");
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
