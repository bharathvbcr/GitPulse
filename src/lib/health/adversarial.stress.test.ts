/**
 * Adversarial input sweep for the health-report surface.
 *
 * Every field here crosses an IPC boundary from scanner output that GitPulse
 * does not control (npm/cargo/pip JSON, the GitHub API). These tests feed the
 * shapes a well-behaved backend never produces — absent optional fields,
 * hostile casing, non-finite counts, enormous strings, bidi control marks —
 * and assert the renderers stay total and stay honest.
 */
import { describe, expect, it } from "vitest";
import { STRESS_TIMEOUT_MS, expectWithinBudget } from "../__tests__/perfBudget";
import {
  dependabotBadgeClass,
  formatAuditCounts,
  normalizeSeverity,
  severityClass,
  updateKind,
} from "./format";
import {
  coverageGap,
  failedAudits,
  formatHealthReport,
  observedTotal,
  skippedAudits,
} from "./report";
import type {
  CodeScanningReport,
  DeadCodeReport,
  DependabotReport,
  DepsHealthReport,
} from "./types";

function bareReport(over: Partial<DepsHealthReport> = {}): DepsHealthReport {
  return {
    node_version: null,
    npm_version: null,
    npm_cli_present: false,
    cargo_audit_present: false,
    manifests: [],
    ecosystems: [],
    issues: [],
    vulnerabilities: [],
    audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
    outdated: [],
    truncated: false,
    ...over,
  };
}

const HOSTILE_STRINGS = [
  "",
  "   ",
  "../../etc/passwd",
  "<script>alert(1)</script>",
  "`rm -rf /`",
  "a".repeat(50_000),
  "\u{1F525}".repeat(2_000),
  "line\nbreak\r\nmixed",
  "‮reversed", // RTL override: must not be treated as structure
  "\0nul",
];

describe("health renderers survive hostile scanner output", () => {
  it("formats a report whose every optional field is absent", () => {
    // An older backend (or a partial deserialize) omits every serde-default
    // field. The renderer must not reach into undefined.
    const text = formatHealthReport(bareReport(), null, null);
    expect(typeof text).toBe("string");
    expect(text).toContain("# Dependency health report");
    // No scanner ran and nothing was found: it must NOT claim an all-clear.
    expect(text).toContain("no audit scanner available");
    expect(text).not.toContain("No issues, vulnerabilities or outdated");
  });

  it("never throws on hostile strings in any text-bearing field", () => {
    for (const s of HOSTILE_STRINGS) {
      const report = bareReport({
        node_version: s,
        npm_version: s,
        scanners_ran: [s],
        issues: [{ severity: s, code: s, message: s, path: s }],
        vulnerabilities: [
          {
            name: s,
            severity: s,
            is_direct: true,
            title: s,
            url: s,
            range: s,
            fix_available: s,
            via: [s, s],
            ecosystem: s,
          },
        ],
        outdated: [
          { name: s, current: s, wanted: s, latest: s, dep_type: s, location: s },
        ],
        audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 1 },
      });
      const dependabot: DependabotReport = {
        available: true,
        cli_present: true,
        is_github_remote: true,
        slug: s,
        truncated: true,
        error: s,
        alerts: [
          {
            number: 1,
            package: s,
            ecosystem: s,
            manifest_path: s,
            scope: s,
            severity: s,
            title: s,
            advisory_id: s,
            cve_id: s,
            vulnerable_range: s,
            first_patched: s,
            url: s,
            created_at: s,
          },
        ],
      };
      const codeScanning: CodeScanningReport = {
        available: true,
        cli_present: true,
        is_github_remote: true,
        slug: s,
        truncated: true,
        error: s,
        alerts: [
          {
            number: 1,
            rule_id: s,
            rule_name: s,
            severity: s,
            state: s,
            tool: s,
            tool_version: s,
            title: s,
            path: s,
            start_line: 0,
            url: s,
            dismissed_reason: s,
            created_at: s,
            updated_at: s,
          },
        ],
      };
      const deadCode: DeadCodeReport = {
        available: true,
        reason: s,
        truncated: true,
        total: 1,
        items: [
          {
            symbol_name: s,
            file_path: s,
            confidence: Number.NaN,
            is_exempt: true,
            exemption_reason: s,
          },
        ],
      };
      expect(() => formatHealthReport(report, s, dependabot, codeScanning, deadCode)).not.toThrow();
      expect(() => skippedAudits(report)).not.toThrow();
      expect(() => normalizeSeverity(s)).not.toThrow();
      expect(() => severityClass(s)).not.toThrow();
      expect(() => dependabotBadgeClass(dependabot.alerts)).not.toThrow();
      expect(() => dependabotBadgeClass(codeScanning.alerts)).not.toThrow();
      expect(() => updateKind(s, s)).not.toThrow();
    }
  });

  it("classifies every severity into a known tier, whatever the casing", () => {
    const tiers = new Set(["critical", "high", "moderate", "low", "info"]);
    const probes = [...HOSTILE_STRINGS, "HIGH", "Critical", " MoDeRaTe ", "MEDIUM", "unranked"];
    for (const s of probes) {
      expect(tiers.has(normalizeSeverity(s))).toBe(true);
    }
    // Casing must never change the rendered tier — the bug this suite guards.
    for (const s of ["critical", "high", "medium", "moderate", "low", "info", "error", "warning", "note", "none", "bogus"]) {
      expect(severityClass(s.toUpperCase())).toBe(severityClass(s));
      expect(dependabotBadgeClass([{ severity: s.toUpperCase() }])).toBe(
        dependabotBadgeClass([{ severity: s }]),
      );
    }
  });

  it("keeps the badge monotonic: adding a worse alert never softens the tint", () => {
    const rank: Record<string, number> = {
      "text-rose-300": 3,
      "text-amber-300": 2,
      "text-sky-300": 1,
      "": 0,
    };
    const ladder = ["low", "medium", "high", "critical"];
    let previous = dependabotBadgeClass([]);
    for (let i = 0; i < ladder.length; i += 1) {
      const next = dependabotBadgeClass(
        ladder.slice(0, i + 1).map((severity) => ({ severity })),
      );
      expect(rank[next]).toBeGreaterThanOrEqual(rank[previous]);
      previous = next;
    }
    expect(previous).toBe("text-rose-300");
  });

  it("never reports a capped scan as complete coverage", () => {
    const summary = { critical: 2, high: 0, moderate: 0, low: 0, total: 2 };
    expect(formatAuditCounts(summary, { complete: false, ran: true })).toMatch(/incomplete/i);
    expect(formatAuditCounts(summary, { complete: true, ran: true })).not.toMatch(/incomplete/i);
    // And the same honesty at zero findings.
    const zero = { critical: 0, high: 0, moderate: 0, low: 0, total: 0 };
    expect(formatAuditCounts(zero, { complete: false, ran: true })).not.toMatch(/No known/);
    expect(formatAuditCounts(zero, { ran: false })).toBe("Audit did not run");
  });

  it("stays total on out-of-contract counts (it does not sanitize them)", () => {
    // Documented limit, not a guarantee: `AuditSummary` is six `u32`s in
    // `analyzer/deps.rs`, so NaN, negative and fractional counts cannot cross
    // the IPC boundary. The renderer is therefore total but NOT sanitizing —
    // it will happily print "NaN findings" if ever handed one. Asserted here
    // so the behavior is recorded rather than assumed away; if the wire type
    // ever loosens to a signed/float count, this test is where it breaks.
    for (const n of [Number.NaN, Infinity, -1, 0.5, Number.MAX_SAFE_INTEGER]) {
      const out = formatAuditCounts(
        { critical: n, high: 0, moderate: 0, low: 0, total: n },
        { complete: true, ran: true },
      );
      expect(typeof out).toBe("string");
      expect(out.length).toBeGreaterThan(0);
    }
    expect(
      formatAuditCounts(
        { critical: Number.NaN, high: 0, moderate: 0, low: 0, total: Number.NaN },
        { complete: true, ran: true },
      ),
    ).toBe("NaN findings");
  });

  it("falls back to the retained count when a limit notice is missing", () => {
    const report = bareReport({
      limit_notices: [{ resource: "health issues", kept: 2, total: 17 }],
    });
    expect(observedTotal(report, "health issues", 2)).toBe(17);
    // Unknown resource -> retained count, never undefined/NaN.
    expect(observedTotal(report, "nope", 5)).toBe(5);
    expect(observedTotal(bareReport(), "health issues", 4)).toBe(4);
  });

  it("keeps every finding's identity in the pasted report even when capped", () => {
    const report = bareReport({
      scanners_ran: ["npm"],
      npm_cli_present: true,
      audit_complete: false,
      truncated: true,
      limit_notices: [{ resource: "vulnerabilities", kept: 1, total: 400 }],
      audit: { info: 0, low: 0, moderate: 0, high: 400, critical: 0, total: 400 },
      vulnerabilities: [
        {
          name: "lodash",
          severity: "high",
          is_direct: true,
          title: "Prototype pollution",
          url: "https://example.invalid/a",
          range: "< 4.17.19",
          fix_available: "4.17.19",
          via: ["a", "b"],
          ecosystem: "npm",
        },
      ],
    });
    const text = formatHealthReport(report, "/repo", null);
    expect(text).toContain("retained 1 of 400");
    expect(text).toContain("not complete coverage");
    expect(text).toContain("showing 1");
    expect(text).toContain("https://example.invalid/a");
    expect(text).toContain("fix available: 4.17.19");
  });

  it("orders semver comparisons total and self-consistent under fuzzing", () => {
    const versions = [
      "0.0.0", "1.0.0", "1.0.1", "1.1.0", "2.0.0", "v3.4.5",
      "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-beta", "1.0.0+build",
      "1", "1.2", "not-a-version", "", "  ", "1.0.0.0", "-1.0.0",
    ];
    const kinds = ["major", "minor", "patch", "prerelease", "same", "unknown"];
    for (const a of versions) {
      expect(updateKind(a, a)).toMatch(/^(same|unknown)$/);
      for (const b of versions) {
        expect(kinds).toContain(updateKind(a, b));
      }
    }
    expect(updateKind("1.0.0", "2.0.0")).toBe("major");
    // A downgrade is never advertised as an available update.
    expect(updateKind("2.0.0", "1.0.0")).toBe("same");
    expect(updateKind("1.0.0-alpha", "1.0.0")).toBe("prerelease");
  });

  it("scales to a saturated report without pathological slowdown", () => {
    const big = bareReport({
      npm_cli_present: true,
      scanners_ran: ["npm"],
      audit_complete: true,
      audit: { info: 0, low: 0, moderate: 0, high: 200, critical: 0, total: 200 },
      vulnerabilities: Array.from({ length: 200 }, (_, i) => ({
        name: `pkg-${i}`,
        severity: "high",
        is_direct: i % 2 === 0,
        title: `Advisory ${i}`,
        url: `https://example.invalid/${i}`,
        range: "< 1.0.0",
        fix_available: "1.0.0",
        via: ["x"],
        ecosystem: "npm",
      })),
      outdated: Array.from({ length: 200 }, (_, i) => ({
        name: `pkg-${i}`,
        current: "1.0.0",
        wanted: "1.0.0",
        latest: "2.0.0",
        dep_type: "dev",
        location: `node_modules/pkg-${i}`,
      })),
      issues: Array.from({ length: 48 }, (_, i) => ({
        severity: "warning",
        code: `code_${i}`,
        message: `m${i}`,
        path: `p/${i}`,
      })),
    });
    const started = performance.now();
    const text = formatHealthReport(big, "/repo", null);
    expectWithinBudget(performance.now() - started, 200, "health adversarial report");
    expect(text.split("\n").length).toBeGreaterThan(400);
  }, STRESS_TIMEOUT_MS);
});

describe("the audit breakdown accounts for every finding", () => {
  /**
   * The invariant, fuzzed over magnitudes rather than bucket combinations
   * (`format.test.ts` sweeps all 2^6 of those exhaustively): whatever numbers
   * the summary prints must add up to the total it claims to summarise.
   *
   * The bug this pins: `info` was absent from both the parts list and the
   * parameter type, so informational findings counted into `total` and then
   * disappeared from the breakdown. `AuditSummary::from_vulns` pins a real
   * case — critical 1, unknown 1, info 1, total 3 — that rendered as
   * "1 critical · 1 unranked".
   */
  it("prints numbers that sum to the total, at every magnitude", () => {
    // Deterministic LCG, same shape the Rust fuzzers use, so a failure names
    // the seed that reproduces it.
    let state = 0x9e3779b9;
    const next = (bound: number) => {
      state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
      return state % bound;
    };
    for (let round = 0; round < 5_000; round += 1) {
      const summary = {
        critical: next(40),
        high: next(40),
        moderate: next(40),
        low: next(40),
        info: next(40),
        unknown: next(40),
        total: 0,
      };
      summary.total =
        summary.critical +
        summary.high +
        summary.moderate +
        summary.low +
        summary.info +
        summary.unknown;
      if (summary.total === 0) continue;
      const rendered = formatAuditCounts(summary, { complete: true, ran: true });
      const printed = [...rendered.matchAll(/(\d+)\s/g)].reduce(
        (sum, match) => sum + Number(match[1]),
        0,
      );
      expect(printed, `round ${round} rendered ${rendered}`).toBe(summary.total);
    }
  });

  /**
   * A backend that grows a severity bucket this formatter has never heard of
   * must widen the printed count, not shrink it. Silence here would be the
   * "capped sample as complete coverage" failure in miniature.
   */
  it("never prints fewer findings than the total claims", () => {
    for (const extra of [1, 7, 1_000, Number.MAX_SAFE_INTEGER - 10]) {
      const rendered = formatAuditCounts(
        { critical: 1, high: 0, moderate: 0, low: 0, info: 0, unknown: 0, total: 1 + extra },
        { complete: true, ran: true },
      );
      expect(rendered).toContain(`${extra} unclassified`);
    }
  });
});

describe("coverage gaps stay total on hostile issue lists", () => {
  it("never throws, whatever the issue codes are", () => {
    for (const hostile of HOSTILE_STRINGS) {
      const report = bareReport({
        issues: [
          { severity: hostile, code: hostile, message: hostile, path: hostile },
          { severity: "warning", code: "cargo_audit_failed", message: hostile, path: null },
        ],
      });
      expect(() => failedAudits(report)).not.toThrow();
      expect(() => coverageGap(report)).not.toThrow();
      // The real failure code must still be found next to the hostile ones.
      expect(failedAudits(report)).toContain("cargo-audit");
    }
  });

  it("reports no gap for a report with no issues at all", () => {
    expect(failedAudits(bareReport())).toEqual([]);
    expect(coverageGap(bareReport())).toBeNull();
  });

  it("stays fast on a saturated issue list", () => {
    const issues = Array.from({ length: 5_000 }, (_, index) => ({
      severity: "warning",
      code: index % 3 === 0 ? "audit_failed" : `unrelated_${index}`,
      message: "x".repeat(200),
      path: null,
    }));
    const report = bareReport({ issues });
    const started = performance.now();
    expect(failedAudits(report)).toEqual(["npm audit"]);
    expectWithinBudget(
      performance.now() - started,
      50,
      "failedAudits over a saturated issue list",
    );
  }, STRESS_TIMEOUT_MS);
});
