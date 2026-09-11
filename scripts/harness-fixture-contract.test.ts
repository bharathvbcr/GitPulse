import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The stress harness is the only place a Svelte `$effect` actually runs — the
 * unit suite compiles it out — so it is where reactive loops, ResizeObserver
 * gaps and layout faults are caught. That makes its mock IPC layer load-bearing
 * in a way a fixture normally is not: **a harness returning a shape the app no
 * longer reads makes a verification that could not run look like one that ran
 * and passed.**
 *
 * That is not hypothetical. `cmd_get_language_stats` sat in this file returning
 * `{languages, total_code_lines}` long after the real command started returning
 * `{stats, truncated, scanned_files, candidate_files}`; every harness run since
 * had been exercising an empty language bar and reporting it as fine.
 *
 * So the fixtures are checked against the interfaces they stand in for, and the
 * field list is *derived* from `types.ts` rather than restated here — a
 * hand-copied list is the same staleness one level up.
 */

const src = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), "utf8");

const harness = src("../harness/stress.html");
const fleetTypes = src("../src/lib/fleet/types.ts");
const healthTypes = src("../src/lib/health/types.ts");
const codeintelTypes = src("../src/lib/codeintel/types.ts");
const coverageTypes = src("../src/lib/coverage/types.ts");
const terminalTypes = src("../src/lib/terminal/runResult.ts");

/**
 * The harness with its comments removed.
 *
 * The comments in that file deliberately name the *wrong* shapes they exist to
 * warn about, so a raw substring search would find `total_code_lines` in the
 * paragraph explaining why it must never come back.
 */
const fixtures = harness.replace(/\/\/[^\n]*/g, "");

/** Field names of a TS interface, ignoring comments and nested members. */
function fieldsOf(source: string, name: string): string[] {
  const found = new RegExp(`export interface ${name}(?:<[^>]+>)? \\{`).exec(source);
  if (!found) throw new Error(`interface ${name} not found`);
  const start = found.index;
  const body = source.slice(start, source.indexOf("\n}", start));
  const fields: string[] = [];
  for (const line of body.split("\n").slice(1)) {
    const match = /^ {2}(\w+)\??:/.exec(line);
    if (match) fields.push(match[1]);
  }
  return fields;
}

describe("the stress harness stands in for the real IPC layer", () => {
  it("finds the interfaces it is checking against", () => {
    // A rename that silently emptied these lists would turn this whole file
    // into a test that passes by measuring nothing.
    expect(fieldsOf(fleetTypes, "FleetMetrics").length).toBeGreaterThan(20);
    expect(fieldsOf(fleetTypes, "FleetCommitStats").length).toBeGreaterThan(8);
    expect(fieldsOf(fleetTypes, "FleetRepoFacet").length).toBeGreaterThan(8);
    expect(fieldsOf(healthTypes, "NpmManifest").length).toBeGreaterThan(8);
    expect(fieldsOf(codeintelTypes, "CodeintelDeadSymbol").length).toBeGreaterThan(3);
    expect(fieldsOf(codeintelTypes, "CodeintelStatus").length).toBeGreaterThan(6);
    expect(fieldsOf(codeintelTypes, "CodeintelResponse").length).toBeGreaterThan(5);
  });

  for (const name of ["FleetMetrics", "FleetCommitStats", "FleetRepoFacet", "FleetSnapshot"]) {
    it(`supplies every ${name} field the grid may read`, () => {
      const missing = fieldsOf(fleetTypes, name).filter(
        // `field: value` or the ES6 shorthand `field,` / `field }`.
        (field) => !new RegExp(`\\b${field}\\s*[:,}]`).test(fixtures),
      );
      expect(missing, `harness fixture is missing ${name}: ${missing.join(", ")}`).toEqual([]);
    });
  }

  it("returns the language report in the shape the app actually parses", () => {
    // The exact regression above. `stats` is the field the app reads; the old
    // `total_code_lines` shape must not come back.
    expect(fixtures).toContain("cmd_get_language_stats");
    expect(fixtures).toMatch(/stats:/);
    expect(fixtures).not.toContain("total_code_lines");
  });

  it("gives every repository in a sweep the same anchor", () => {
    // Per-repository commit series are summed bucket for bucket, which is only
    // meaningful when they share an anchor. A fixture that varied it per row
    // would exercise only the mismatch path and never the sum.
    expect(fixtures).toContain("anchor_epoch: anchor");
  });

  it("includes a repository with no baseline, so the no-delta path is exercised", () => {
    // A first scan has no direction. If every fixture row had a baseline, the
    // "render no chip at all" branch would never run in the harness.
    expect(fixtures).toMatch(/loc_prev:\s*i === 0 \? null/);
  });

  it("answers every command HealthPanel invokes on mount", () => {
    // invoke returns null for a missing key. HealthPanel then reads
    // `.available` and `.length` off that null and throws; the throw tears
    // the pane's effects down, so a loop would look like a clean run.
    for (const cmd of [
      "cmd_scan_deps_health",
      "cmd_github_dependabot_alerts",
      "cmd_github_code_scanning_alerts",
      "cmd_codeintel_dead_symbols",
      "cmd_codeintel_status",
    ]) {
      expect(fixtures, `HealthPanel invoke ${cmd} is missing from FIXTURES`).toContain(cmd);
    }
  });

  for (const [source, name] of [
    [healthTypes, "NpmManifest"],
    [healthTypes, "DepsHealthReport"],
    [healthTypes, "DependabotReport"],
    [healthTypes, "DependabotAlertInfo"],
    [healthTypes, "CodeScanningReport"],
    [healthTypes, "CodeScanningAlertInfo"],
    [healthTypes, "Vulnerability"],
    [healthTypes, "OutdatedPackage"],
    [healthTypes, "HealthIssue"],
    [healthTypes, "AuditSummary"],
    [healthTypes, "EcosystemHint"],
    [healthTypes, "ScanLimitNotice"],
    [codeintelTypes, "CodeintelDeadSymbol"],
    [codeintelTypes, "CodeintelStatus"],
    [codeintelTypes, "CodeintelResponse"],
    [codeintelTypes, "CodeintelRungHistogram"],
    [coverageTypes, "CoverageFamilyStatus"],
    [coverageTypes, "CoverageReport"],
    [coverageTypes, "FileCoverage"],
    [terminalTypes, "TerminalRunResult"],
    [terminalTypes, "TerminalSpawned"],
  ] as const) {
    it(`supplies every ${name} field Health/Coverage/Terminal may read`, () => {
      const missing = fieldsOf(source, name).filter(
        (field) => !new RegExp(`\\b${field}\\s*[:,}]`).test(fixtures),
      );
      expect(missing, `harness fixture is missing ${name}: ${missing.join(", ")}`).toEqual([]);
    });
  }
});
