import { describe, expect, it } from "vitest";
import {
  parseCodeScanningReport,
  parseDependabotReport,
  parseDepsHealthReport,
} from "./types";

describe("parseDepsHealthReport", () => {
  it("throws on null rather than looking like a repo with no packages", () => {
    expect(() => parseDepsHealthReport(null)).toThrow(/no payload/);
  });

  it("throws when manifests is missing, not treating that as zero packages", () => {
    expect(() =>
      parseDepsHealthReport({
        npm_cli_present: true,
        cargo_audit_present: true,
        ecosystems: [],
        issues: [],
        vulnerabilities: [],
        outdated: [],
        audit: { total: 0 },
        truncated: false,
      }),
    ).toThrow(/omitted manifests/);
  });

  it("defaults a missing lifecycle_scripts list to empty instead of crashing render", () => {
    const parsed = parseDepsHealthReport({
      npm_cli_present: true,
      cargo_audit_present: true,
      manifests: [{ path: "package.json", name: "app" }],
      ecosystems: [{ family: "node", manifests: ["package.json"], note: "npm" }],
      issues: [],
      vulnerabilities: [],
      outdated: [],
      audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
      truncated: false,
    });
    expect(parsed.manifests[0].lifecycle_scripts).toEqual([]);
  });

  it("throws when lifecycle_scripts is present but not a string list", () => {
    expect(() =>
      parseDepsHealthReport({
        npm_cli_present: true,
        cargo_audit_present: true,
        manifests: [{ path: "package.json", lifecycle_scripts: [1] }],
        ecosystems: [],
        issues: [],
        vulnerabilities: [],
        outdated: [],
        audit: { total: 0 },
        truncated: false,
      }),
    ).toThrow(/lifecycle_scripts/);
  });
});

describe("parseDependabotReport / parseCodeScanningReport", () => {
  it("throws on null so a missing fixture cannot look like zero alerts", () => {
    expect(() => parseDependabotReport(null)).toThrow(/no payload/);
    expect(() => parseCodeScanningReport(null)).toThrow(/no payload/);
  });

  it("throws when available is true without an alerts array", () => {
    expect(() =>
      parseDependabotReport({
        available: true,
        truncated: false,
      }),
    ).toThrow(/without alerts/);
  });

  it("normalises an unavailable report to an empty alerts list", () => {
    const parsed = parseDependabotReport({
      available: false,
      error: "no analysis found (HTTP 1)",
      unavailable_reason: "product_disabled",
    });
    expect(parsed.available).toBe(false);
    expect(parsed.alerts).toEqual([]);
    expect(parsed.unavailable_reason).toBe("product_disabled");
  });
});
