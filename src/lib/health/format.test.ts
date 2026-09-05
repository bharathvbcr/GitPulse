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

  /**
   * The breakdown is the only thing most readers look at, so it has to
   * account for every finding in the total. `info` was absent from the
   * parameter type entirely, so TypeScript never noticed that informational
   * findings were counted into `total` and then dropped from the summary —
   * the Rust side pins exactly this shape in `AuditSummary::from_vulns`
   * (critical 1, unknown 1, info 1, total 3).
   */
  it("accounts for every finding in the total, informational ones included", () => {
    expect(
      formatAuditCounts({
        critical: 1,
        high: 0,
        moderate: 0,
        low: 0,
        info: 1,
        unknown: 1,
        total: 3,
      }),
    ).toBe("1 critical · 1 info · 1 unranked");
  });

  /**
   * Derived rather than spot-checked: whatever buckets exist, the numbers the
   * summary prints must add up to the total it is summarising. A future
   * severity bucket that nobody wires into the parts list shows up here as a
   * sum mismatch instead of silently vanishing from the UI.
   */
  it("prints a breakdown that sums to the total, for every bucket combination", () => {
    const buckets = ["critical", "high", "moderate", "low", "info", "unknown"] as const;
    // 2^6 subsets, each bucket carrying a distinct count so a dropped bucket
    // cannot be masked by an equal one elsewhere.
    for (let mask = 1; mask < 1 << buckets.length; mask += 1) {
      const summary = {
        critical: 0,
        high: 0,
        moderate: 0,
        low: 0,
        info: 0,
        unknown: 0,
        total: 0,
      };
      buckets.forEach((bucket, index) => {
        if (mask & (1 << index)) {
          summary[bucket] = index + 1;
          summary.total += index + 1;
        }
      });
      const rendered = formatAuditCounts(summary, { complete: true, ran: true });
      const printed = [...rendered.matchAll(/(\d+)\s/g)].reduce(
        (sum, match) => sum + Number(match[1]),
        0,
      );
      expect(printed, `mask ${mask} rendered ${rendered}`).toBe(summary.total);
    }
  });

  /**
   * A total larger than the buckets explain means the backend grew a bucket
   * this formatter does not know about. Saying so is the honest outcome;
   * printing only the buckets it recognises understates the finding count.
   */
  it("names the remainder when the buckets do not explain the total", () => {
    expect(
      formatAuditCounts({
        critical: 1,
        high: 0,
        moderate: 0,
        low: 0,
        info: 0,
        unknown: 0,
        total: 4,
      }),
    ).toBe("1 critical · 3 unclassified");
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
