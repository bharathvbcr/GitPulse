import { describe, expect, it } from "vitest";
import {
  healthSummary,
  missingNeeded,
  missingSummary,
  needLabel,
  presetSummary,
  versionText,
} from "./devcouncilSuite";
import type { ComponentStatus, SuiteReport } from "../codeintel/types";

function component(overrides: Partial<ComponentStatus> = {}): ComponentStatus {
  return {
    id: "devmap",
    label: "DevMap",
    need: "required",
    purpose: "Builds the code index.",
    installed: true,
    path: "/usr/local/bin/devmap",
    version: { kind: "reported", version: "devmap 0.2.1" },
    reason: null,
    presets: ["devmap"],
    ...overrides,
  };
}

function report(overrides: Partial<SuiteReport> = {}): SuiteReport {
  return {
    suite: { components: [component()], presets: [], complete: true },
    doctor: {
      available: true,
      binary: "/usr/local/bin/devmap",
      reason: null,
      binary_skew_warning: null,
      duplicate_mcp_registration_warning: null,
      stale_server_warning: null,
      plugin_warning: null,
      missing_binary_warning: null,
      expected_schema_version: 20,
      code_graph_schema_version: 2,
      linked_grammar_count: 33,
      version: "0.2.1",
    },
    doctor_reason: null,
    warnings: [],
    ...overrides,
  };
}

describe("versionText", () => {
  it("shows a reported version as it came back", () => {
    expect(versionText({ kind: "reported", version: "devmap 0.2.1" })).toBe("devmap 0.2.1");
  });

  it("says a component exposes no version rather than showing nothing", () => {
    // `dcstore`, `dcverify` and `dcgrep` reject `--version`. Rendering an empty
    // string here reads as "unknown, probably stale"; it means the opposite —
    // the binary ran and there is nothing to ask it.
    const text = versionText({
      kind: "not_exposed",
      detail: "this component exposes no version flag",
    });
    expect(text).toContain("not reported");
    expect(text).toContain("no version flag");
    expect(text.trim()).not.toBe("");
  });

  it("distinguishes a component that could not run from one with no version", () => {
    const failed = versionText({ kind: "unavailable", detail: "not found" });
    expect(failed).toContain("could not run");
    expect(failed).not.toContain("not reported");
  });
});

describe("missingSummary", () => {
  it("names every needed component that is absent", () => {
    const components = [
      component(),
      component({ id: "dcstore", need: "host_resolved", installed: false }),
      component({ id: "dcgrep", need: "host_resolved", installed: false }),
    ];
    expect(missingNeeded(components).map((c) => c.id)).toEqual(["dcstore", "dcgrep"]);
    expect(missingSummary(components)).toBe("2 components are missing: dcstore, dcgrep");
  });

  it("uses the singular for one", () => {
    const components = [component({ id: "dcstore", need: "host_resolved", installed: false })];
    expect(missingSummary(components)).toBe("1 component is missing: dcstore");
  });

  it("never counts an optional component as missing", () => {
    // GitPulse does not start the Go host, so its absence is not a problem and
    // must not put an amber warning on a healthy installation.
    const components = [component(), component({ id: "devcouncil", need: "optional", installed: false })];
    expect(missingNeeded(components)).toEqual([]);
    expect(missingSummary(components)).toContain("Every component");
  });
});

describe("healthSummary", () => {
  it("reports a clean bill only when doctor actually ran", () => {
    expect(healthSummary(report())).toEqual({ kind: "clean" });
  });

  it("says health was not checked rather than showing no warnings", () => {
    // The invariant: a check that could not run must never produce the same
    // output as a check that ran and found nothing.
    const summary = healthSummary(
      report({ doctor: null, doctor_reason: "no repository is open", warnings: [] }),
    );
    expect(summary).toEqual({ kind: "unchecked", reason: "no repository is open" });
  });

  it("treats an unavailable doctor as unchecked even without a stated reason", () => {
    const summary = healthSummary(
      report({
        doctor: { ...report().doctor!, available: false, reason: null },
        doctor_reason: null,
        warnings: [],
      }),
    );
    expect(summary.kind).toBe("unchecked");
  });

  it("passes warnings through in the order the backend ranked them", () => {
    const warnings = ["two devmap binaries", "stale mcp processes"];
    expect(healthSummary(report({ warnings }))).toEqual({ kind: "warnings", warnings });
  });

  it("prefers the unchecked reason over a warning list it cannot have earned", () => {
    // Defensive: if a backend ever sent both, the reason wins — claiming
    // warnings from a probe that did not run is the failure mode that matters.
    const summary = healthSummary(
      report({ doctor_reason: "devmap is not installed", warnings: ["something"] }),
    );
    expect(summary.kind).toBe("unchecked");
  });
});

describe("presetSummary", () => {
  it("names what a preset is missing", () => {
    expect(
      presetSummary({ id: "analysis", present: 2, total: 4, missing: ["dcgrep", "dcverify"] }),
    ).toBe("2 of 4 installed — missing dcgrep, dcverify");
  });

  it("omits the missing clause when nothing is missing", () => {
    expect(presetSummary({ id: "devmap", present: 1, total: 1, missing: [] })).toBe(
      "1 of 1 installed",
    );
  });
});

describe("needLabel", () => {
  it("labels every need the backend can send", () => {
    for (const need of ["required", "host_resolved", "optional"] as const) {
      expect(needLabel(need)).not.toBe("Unknown");
      expect(needLabel(need).length).toBeGreaterThan(0);
    }
  });

  it("says a host-resolved component is the host's, not GitPulse's", () => {
    expect(needLabel("host_resolved")).toContain("Manvi");
  });
});
