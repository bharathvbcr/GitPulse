import { describe, expect, it } from "vitest";
import {
  issueTone,
  severityTone,
  summarizeHealth,
  worstTone,
  type HealthSummaryInput,
  type HealthTone,
} from "./summary";
import type {
  CodeScanningReport,
  DependabotReport,
  DepsHealthReport,
} from "./types";
import type { CodeintelStatus } from "../codeintel/types";

function report(overrides: Partial<DepsHealthReport> = {}): DepsHealthReport {
  return {
    node_version: "22.0.0",
    npm_version: "10.8.0",
    npm_cli_present: true,
    cargo_audit_present: false,
    manifests: [],
    ecosystems: [],
    issues: [],
    vulnerabilities: [],
    audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
    outdated: [],
    truncated: false,
    scanners_ran: ["npm"],
    audit_complete: true,
    ...overrides,
  };
}

function dependabot(overrides: Partial<DependabotReport> = {}): DependabotReport {
  return {
    available: true,
    cli_present: true,
    is_github_remote: true,
    slug: "owner/repo",
    alerts: [],
    truncated: false,
    ...overrides,
  };
}

function codeScanning(
  overrides: Partial<CodeScanningReport> = {},
): CodeScanningReport {
  return {
    available: true,
    cli_present: true,
    is_github_remote: true,
    slug: "owner/repo",
    alerts: [],
    truncated: false,
    ...overrides,
  };
}

const graph: CodeintelStatus = {
  available: true,
  db_path: "/fixture/devmap.sqlite",
  total_files: 10,
  total_symbols: 20,
  total_edges: 30,
};

/** Everything established and clean: the one input that may claim an all-clear. */
function cleanInput(overrides: Partial<HealthSummaryInput> = {}): HealthSummaryInput {
  return {
    report: report(),
    dependabot: dependabot(),
    codeScanning: codeScanning(),
    codegraph: graph,
    deadCode: {
      available: true,
      reason: null,
      total: 0,
      shown: 0,
      truncated: false,
      walkIncomplete: null,
    },
    ...overrides,
  };
}

const facet = (input: HealthSummaryInput, id: string) =>
  summarizeHealth(input).facets.find((f) => f.id === id)!;

describe("summarizeHealth — the all-clear is earned, never assumed", () => {
  it("claims an all-clear only when every facet is established and clean", () => {
    const summary = summarizeHealth(cleanInput());
    expect(summary.tone).toBe("clear");
    expect(summary.clearClaimable).toBe(true);
    expect(summary.caveats).toEqual([]);
    expect(summary.headline).toBe("Every scan completed with no findings.");
  });

  /**
   * The honesty invariant, as a property rather than a list of cases: for
   * every way a single facet can be left unestablished, the whole verdict
   * must stop being claimable. This is the guard that a future facet cannot
   * be added in a way that sums away into a green tick.
   */
  it.each<[string, Partial<HealthSummaryInput>]>([
    ["the audit never ran", { report: report({ scanners_ran: [], audit_complete: false }) }],
    ["the audit was partial", { report: report({ audit_complete: false }) }],
    ["Dependabot was never fetched", { dependabot: null }],
    ["Dependabot could not be reached", { dependabot: dependabot({ available: false, error: "gh exited 1" }) }],
    ["code scanning was never fetched", { codeScanning: null }],
    ["code scanning could not be reached", { codeScanning: codeScanning({ available: false, error: "no access" }) }],
    ["npm is missing so nothing could be checked for updates", { report: report({ npm_cli_present: false }) }],
    ["there is no code graph", { codegraph: null }],
    ["the dead-code query did not run", { deadCode: { available: false, reason: "budget", total: 0, shown: 0, truncated: false, walkIncomplete: null } }],
    ["the dead-code walk was incomplete", { deadCode: { available: true, reason: null, total: 0, shown: 0, truncated: false, walkIncomplete: "3 unresolved sites" } }],
    ["the dead-code query stopped at its budget", { deadCode: { available: true, reason: null, total: 0, shown: 0, truncated: true, walkIncomplete: null } }],
  ])("refuses an all-clear when %s", (_why, override) => {
    const summary = summarizeHealth(cleanInput(override));
    expect(summary.clearClaimable).toBe(false);
    expect(summary.tone).not.toBe("clear");
    expect(summary.caveats.length).toBeGreaterThan(0);
    // Every caveat names a reason; none is an empty string standing in for one.
    for (const caveat of summary.caveats) expect(caveat.trim().length).toBeGreaterThan(0);
  });

  it("never invents a cause it was not given", () => {
    const summary = summarizeHealth(
      cleanInput({ dependabot: dependabot({ available: false, error: null }) }),
    );
    const alerts = summary.facets.find((f) => f.id === "dependabot")!;
    expect(alerts.tone).toBe("unknown");
    expect(alerts.caveat).toContain("reason not reported");
  });

  it("does not let a finding outrank an unestablished check into an all-clear", () => {
    // A critical finding *and* an unfetched Dependabot: the headline names the
    // finding, but the caveats still carry the thing nobody checked.
    const summary = summarizeHealth(
      cleanInput({
        report: report({
          audit: { info: 0, low: 0, moderate: 0, high: 1, critical: 0, total: 1 },
        }),
        dependabot: null,
      }),
    );
    expect(summary.tone).toBe("critical");
    expect(summary.clearClaimable).toBe(false);
    expect(summary.caveats.join(" ")).toContain("Dependabot alerts have not been fetched");
  });
});

describe("summarizeHealth — GitHub alert states are all distinguishable", () => {
  /**
   * "Checked, nothing open" and "never checked" used to render as the same
   * empty space, so a clean local audit read as an all-clear for a repository
   * whose GitHub alerts nobody had looked at. These are the four states, and
   * no two of them may share a value *or* a tone.
   */
  it.each([
    ["dependabot", (v: DependabotReport | null) => cleanInput({ dependabot: v })],
    ["code-scanning", (v: DependabotReport | null) => cleanInput({ codeScanning: v as never })],
  ])("%s separates unchecked, unavailable, clear and open", (id, build) => {
    const unchecked = facet(build(null), id);
    const unavailable = facet(build(dependabot({ available: false, error: "gh missing" })), id);
    const clear = facet(build(dependabot()), id);
    const open = facet(
      build(
        dependabot({
          alerts: [{ severity: "high" } as never],
        }),
      ),
      id,
    );

    expect(unchecked.value).toBe("not checked");
    expect(unchecked.tone).toBe("unknown");
    expect(unavailable.value).toBe("unavailable");
    expect(unavailable.tone).toBe("unknown");
    expect(clear.value).toBe("0 open");
    expect(clear.tone).toBe("clear");
    expect(open.value).toBe("1 open");
    expect(open.tone).toBe("critical");

    const values = [unchecked, unavailable, clear, open].map((f) => f.value);
    expect(new Set(values).size).toBe(4);
  });

  it("marks a truncated alert list as a floor", () => {
    const f = facet(
      cleanInput({
        dependabot: dependabot({
          truncated: true,
          alerts: [{ severity: "low" } as never],
        }),
      }),
      "dependabot",
    );
    expect(f.value).toBe("1+ open");
    expect(f.caveat).toContain("more remain");
  });

  it("reads GitHub severities whatever case they arrive in", () => {
    // `github/mod.rs` stores `security_vulnerability.severity` verbatim, so
    // "HIGH" and "Critical" are both live inputs.
    for (const severity of ["high", "HIGH", "Critical", " critical "]) {
      const f = facet(
        cleanInput({ dependabot: dependabot({ alerts: [{ severity } as never] }) }),
        "dependabot",
      );
      expect(f.tone, `severity ${JSON.stringify(severity)} was demoted`).toBe("critical");
    }
  });
});

describe("summarizeHealth — the local audit", () => {
  it("says an audit that never ran did not run, rather than reporting zero", () => {
    const f = facet(
      cleanInput({ report: report({ scanners_ran: [], audit_complete: false }) }),
      "vulnerabilities",
    );
    expect(f.value).toBe("did not run");
    expect(f.tone).toBe("unknown");
    expect(f.caveat).toContain("no finding count exists");
  });

  it("marks a partial audit's counts as a floor even when it found things", () => {
    const f = facet(
      cleanInput({
        report: report({
          audit_complete: false,
          audit: { info: 0, low: 0, moderate: 0, high: 1, critical: 0, total: 1 },
        }),
      }),
      "vulnerabilities",
    );
    expect(f.value).toContain("1 high");
    expect(f.value).toContain("partial");
    expect(f.caveat).toContain("a floor");
    // A partial scan that happens to have findings is still a partial scan.
    expect(f.tone).toBe("critical");
  });

  it("names the scanners behind a short coverage claim", () => {
    const f = facet(
      cleanInput({
        report: report({
          audit_complete: false,
          scanners_ran: [],
          cargo_audit_present: false,
          ecosystems: [
            { family: "cargo", manifests: ["Cargo.lock"], note: "checked with cargo audit" },
          ],
        }),
      }),
      "vulnerabilities",
    );
    expect(f.caveat).toContain("cargo");
  });

  it("counts a finding the severity buckets do not explain", () => {
    // pip-audit and govulncheck publish no severity; a total the buckets do
    // not account for must not fall through to `clear`.
    const f = facet(
      cleanInput({
        report: report({
          audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 4 },
        }),
      }),
      "vulnerabilities",
    );
    expect(f.tone).toBe("warn");
  });
});

describe("summarizeHealth — capped scans", () => {
  it("states the exact retained and observed counts for every notice", () => {
    const summary = summarizeHealth(
      cleanInput({
        report: report({
          truncated: true,
          limit_notices: [
            { resource: "cargo ecosystem artifacts", kept: 24, total: 36, inventory_only: true },
          ],
        }),
      }),
    );
    expect(summary.caveats).toContain("cargo ecosystem artifacts: retained 24 of 36");
  });

  it("separates a display cap from a coverage cap", () => {
    const inventory = summarizeHealth(
      cleanInput({
        report: report({
          truncated: true,
          limit_notices: [{ resource: "x", kept: 1, total: 2, inventory_only: true }],
        }),
      }),
    );
    const coverage = summarizeHealth(
      cleanInput({
        report: report({
          truncated: true,
          limit_notices: [{ resource: "x", kept: 1, total: 2 }],
        }),
      }),
    );
    expect(inventory.caveats.join(" ")).toContain("Inventory display was capped");
    expect(coverage.caveats.join(" ")).toContain("some findings may be omitted");
  });

  /**
   * The cross-reference this replaces said audit coverage was "reported
   * separately above" — and "above" was the header summary, which was being
   * truncated mid-word at the time. A pointer to something the reader cannot
   * see is worse than no pointer.
   */
  it("does not point at a place on screen for the coverage answer", () => {
    const summary = summarizeHealth(
      cleanInput({
        report: report({
          truncated: true,
          limit_notices: [{ resource: "x", kept: 1, total: 2, inventory_only: true }],
        }),
      }),
    );
    expect(summary.caveats.join(" ")).not.toMatch(/\babove\b/);
    expect(summary.caveats.join(" ")).toContain("Vulnerabilities facet");
  });
});

describe("tone algebra", () => {
  it("ranks an unestablished check above both clear and note", () => {
    expect(worstTone(["clear", "unknown"])).toBe("unknown");
    expect(worstTone(["note", "unknown"])).toBe("unknown");
    expect(worstTone(["unknown", "warn"])).toBe("warn");
    expect(worstTone(["warn", "critical"])).toBe("critical");
  });

  it("is clear only for an all-clear set", () => {
    expect(worstTone([])).toBe("clear");
    expect(worstTone(["clear", "clear"])).toBe("clear");
    expect(worstTone(["clear", "note"])).toBe("note");
  });

  it("maps advisory severities through the shared normalizer", () => {
    const expected: [string, HealthTone][] = [
      ["critical", "critical"],
      ["high", "critical"],
      ["error", "critical"],
      ["moderate", "warn"],
      ["medium", "warn"],
      ["warning", "warn"],
      ["low", "warn"],
      ["info", "note"],
      ["", "note"],
    ];
    for (const [severity, tone] of expected) {
      expect(severityTone(severity), `severity ${severity}`).toBe(tone);
    }
  });

  it("does not read a lint-level 'error' as a high-severity advisory", () => {
    // `normalizeSeverity` maps "error" to "high" because CodeQL writes it on
    // advisories. `HealthIssue.severity` is a different field with the same
    // words in it, so it gets its own mapping rather than that one.
    expect(issueTone("error")).toBe("critical");
    expect(issueTone("warning")).toBe("warn");
    expect(issueTone("info")).toBe("note");
    expect(issueTone("lifecycle_scripts")).toBe("note");
  });
});

describe("summarizeHealth — every facet has a jump target", () => {
  it("names only section ids the catalog carries", async () => {
    const { HEALTH_SECTION_IDS } = await import("./sections");
    const summary = summarizeHealth(cleanInput());
    for (const f of summary.facets) {
      expect(HEALTH_SECTION_IDS, `facet ${f.id} is not a section`).toContain(f.id);
    }
  });

  it("gives every non-clear facet a reason", () => {
    const summary = summarizeHealth(
      cleanInput({ dependabot: null, codeScanning: null, codegraph: null }),
    );
    for (const f of summary.facets) {
      if (f.tone === "clear") continue;
      expect(f.caveat, `facet ${f.id} is not clear and says nothing`).toBeTruthy();
    }
  });
});
