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
import {
  dependabotBadgeClass,
  formatAuditCounts,
  normalizeSeverity,
  severityClass,
  updateKind,
} from "./format";
import { formatHealthReport, observedTotal, skippedAudits } from "./report";
import type { DependabotReport, DepsHealthReport } from "./types";

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
      expect(() => formatHealthReport(report, s, dependabot)).not.toThrow();
      expect(() => skippedAudits(report)).not.toThrow();
      expect(() => normalizeSeverity(s)).not.toThrow();
      expect(() => severityClass(s)).not.toThrow();
      expect(() => dependabotBadgeClass(dependabot.alerts)).not.toThrow();
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
    for (const s of ["critical", "high", "medium", "moderate", "low", "info", "bogus"]) {
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
    expect(performance.now() - started).toBeLessThan(1_000);
    expect(text.split("\n").length).toBeGreaterThan(400);
  });
});
