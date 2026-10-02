import { describe, expect, it } from "vitest";
import { parseSecretsReport, type SecretFinding, type SecretsReport } from "./types";
import {
  LOCATION_COPY,
  LOCATION_ORDER,
  capNote,
  distinctSecrets,
  filterFindings,
  groupSizes,
  isStale,
  locationChips,
  scopeNotes,
  scanFailureCopy,
  shouldCache,
  STALE_AFTER_MS,
  verdict,
} from "./summary";

function report(overrides: Partial<SecretsReport> = {}): SecretsReport {
  return {
    ok: true,
    error: null,
    diagnostic: null,
    kingfisher_present: true,
    kingfisher_version: "2.7.0",
    nested_repos_scanned: true,
    completeness: "complete",
    findings: [],
    findings_total: 0,
    findings_unreadable: 0,
    findings_truncated: false,
    git_status_known: true,
    max_file_size_mb: 256,
    scanned_at_ms: 1_000_000,
    duration_ms: 10,
    ...overrides,
  };
}

function row(overrides: Partial<SecretFinding> = {}): SecretFinding {
  return {
    rule_id: "betterleaks.github-pat",
    rule_name: "github-pat",
    path: "a.sh",
    line: 1,
    confidence: "high",
    location: "tracked",
    secret_group: 1,
    ...overrides,
  };
}

describe("verdict", () => {
  it("says clean only for a complete, lossless, empty scan", () => {
    expect(verdict(report())).toMatchObject({ tone: "ok", title: "No secrets reported" });
    // Every way a scan can fall short of that must change the headline.
    const shortfalls: Partial<SecretsReport>[] = [
      { completeness: "partial" },
      { completeness: "unverified" },
      { findings_unreadable: 1 },
      { findings_truncated: true },
      { ok: false, error: "kingfisher timed out" },
      { ok: false, error: null },
    ];
    for (const shortfall of shortfalls) {
      const v = verdict(report(shortfall));
      expect(v.title, JSON.stringify(shortfall)).not.toBe("No secrets reported");
      expect(v.tone, JSON.stringify(shortfall)).not.toBe("ok");
    }
  });

  it("states that a partial empty scan is not clean", () => {
    const v = verdict(report({ completeness: "partial" }));
    expect(v.title).toBe("No findings, but the scan was partial");
    expect(v.detail).toContain("This is not a clean result.");
  });

  it("marks a lossy count as a floor", () => {
    const rows = [row()];
    expect(verdict(report({ findings: rows, findings_total: 1 })).title).toBe("1 finding");
    expect(
      verdict(report({ findings: rows, findings_total: 1, findings_unreadable: 2 })).title,
    ).toBe("1 finding+");
    expect(
      verdict(report({ findings: rows, findings_total: 1, findings_unreadable: 2 })).detail,
    ).toContain("2 findings could not be read and are not listed.");
    expect(verdict(report({ findings: rows, findings_total: 1, findings_truncated: true })).title).toBe(
      "1 finding+",
    );
  });

  it("keeps a failed scan's findings in view", () => {
    const v = verdict(
      report({ ok: false, error: "Kingfisher reported that the scan failed.", findings: [row()], findings_total: 1 }),
    );
    expect(v.title).toBe("Secrets scan failed partway");
    expect(v.tone).toBe("danger");
  });

  it("names missing git status", () => {
    const v = verdict(report({ findings: [row({ location: "unknown" })], findings_total: 1, git_status_known: false }));
    expect(v.detail).toContain("Git status could not be read");
  });
});

describe("locations", () => {
  it("has copy for every location and orders the actionable ones first", () => {
    expect(Object.keys(LOCATION_COPY).sort()).toEqual([...LOCATION_ORDER].sort());
    expect(LOCATION_ORDER.slice(0, 2)).toEqual(["git_metadata", "tracked"]);
    expect(LOCATION_ORDER.indexOf("unknown")).toBeLessThan(LOCATION_ORDER.indexOf("ignored"));
  });

  it("chips count only present locations, in order, and filtering matches them", () => {
    const rows = [
      row({ location: "ignored", path: "build/x" }),
      row({ location: "tracked" }),
      row({ location: "ignored", path: "build/y" }),
    ];
    expect(locationChips(rows)).toEqual([
      { location: "tracked", count: 1 },
      { location: "ignored", count: 2 },
    ]);
    for (const chip of locationChips(rows)) {
      expect(filterFindings(rows, chip.location)).toHaveLength(chip.count);
    }
    expect(filterFindings(rows, "all")).toHaveLength(3);
  });
});

describe("value groups", () => {
  it("counts locations per value and distinct values, treating group 0 as unknown", () => {
    const rows = [
      row({ secret_group: 1 }),
      row({ secret_group: 1, path: "b.sh" }),
      row({ secret_group: 2, path: "c.sh" }),
      row({ secret_group: 0, path: "d.sh" }),
      row({ secret_group: 0, path: "e.sh" }),
    ];
    expect(groupSizes(rows)).toEqual(new Map([[1, 2], [2, 1]]));
    expect(distinctSecrets(rows)).toBe(4);
  });
});

describe("cap and scope", () => {
  it("states both numbers when the backend capped the rows", () => {
    expect(capNote(report({ findings: [row()], findings_total: 1 }))).toBeNull();
    expect(capNote(report({ findings: [row()], findings_total: 20_001 }))).toBe(
      "Showing the 1 most actionable of 20,001 findings.",
    );
  });

  it("names every measured exclusion", () => {
    const notes = scopeNotes(report()).join(" ");
    expect(notes).toContain("commit history is not scanned");
    expect(notes).toContain("256 MB");
    expect(notes).toContain("Symbolic links are not followed");
    expect(notes).toContain("kingfisher:ignore");
    expect(notes).toContain("Nested repositories");
  });
});

describe("staleness and caching", () => {
  it("refreshes an old or untimed report", () => {
    expect(isStale(report({ scanned_at_ms: 1_000 }), 1_000 + STALE_AFTER_MS - 1)).toBe(false);
    expect(isStale(report({ scanned_at_ms: 1_000 }), 1_000 + STALE_AFTER_MS)).toBe(true);
    expect(isStale(report({ scanned_at_ms: 0 }), 1)).toBe(true);
  });

  it("never lets a late older scan, or a scan that never ran, replace the cache", () => {
    const newer = report({ scanned_at_ms: 2_000 });
    expect(shouldCache(undefined, newer)).toBe(true);
    expect(shouldCache(newer, report({ scanned_at_ms: 1_000 }))).toBe(false);
    expect(shouldCache(newer, report({ scanned_at_ms: 3_000 }))).toBe(true);
    expect(shouldCache(newer, report({ ok: false, scanned_at_ms: 0 }))).toBe(false);
  });
});

describe("scanFailureCopy", () => {
  it("joins the reason with the diagnostic, and stands alone when there is none", () => {
    const text = scanFailureCopy(
      report({
        ok: false,
        error: "kingfisher did not finish within 300s",
        diagnostic: "kingfisher: 2.7.0\nstdout_bytes_captured: 0",
      }),
    );
    expect(text).toBe(
      "kingfisher did not finish within 300s\nkingfisher: 2.7.0\nstdout_bytes_captured: 0",
    );
    expect(scanFailureCopy(report({ ok: false, error: "kingfisher could not be started" }))).toBe(
      "kingfisher could not be started",
    );
    expect(scanFailureCopy(null)).toBe("The scanner did not complete.");
  });
});

describe("parseSecretsReport", () => {
  it("strips a planted token from the error and the diagnostic before the panel can show them", () => {
    const token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    const parsed = parseSecretsReport({
      ...report(),
      ok: false,
      error: `kingfisher failed near ${token}`,
      diagnostic: `binary: /tmp/${token}\nstdout_bytes_captured: 0`,
    });
    const text = JSON.stringify(parsed);
    expect(text).not.toContain(token);
    expect(parsed.error).toContain("kingfisher failed near");
    expect(parsed.diagnostic).toContain("stdout_bytes_captured: 0");
  });

  it("redacts a token that crosses the diagnostic length cap before cutting", () => {
    const token = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    const parsed = parseSecretsReport({
      ...report(),
      ok: false,
      // A slash is a token boundary. The 10 characters of the token that fit
      // under the 4000-character cap are what a cap-first parser would keep.
      diagnostic: `${"/".repeat(3_990)}${token}`,
    });
    const text = parsed.diagnostic ?? "";
    expect(text).not.toContain(token.slice(0, 10));
    expect(text.length).toBeLessThanOrEqual(4_000);
  });

  it("keeps only allowlisted finding fields", () => {
    const parsed = parseSecretsReport({
      ...report(),
      findings: [
        {
          ...row(),
          snippet: "ghp_SHOULD_NOT_APPEAR",
          secret: "ghp_SHOULD_NOT_APPEAR",
          fingerprint: "123",
        },
      ],
      findings_total: 1,
    });
    expect(parsed.findings).toHaveLength(1);
    expect(parsed.diagnostic).toBeNull();
    const text = JSON.stringify(parsed);
    for (const leaked of ["ghp_SHOULD_NOT_APPEAR", "snippet", "fingerprint"]) {
      expect(text).not.toContain(leaked);
    }
  });

  it("fails closed on drifted payloads", () => {
    // ok without a findings array is unreadable, not clean.
    const noFindings = parseSecretsReport({ ok: true, completeness: "complete" });
    expect(noFindings.ok).toBe(false);
    expect(verdict(noFindings).tone).not.toBe("ok");
    // An unknown completeness is unverified, never complete.
    expect(parseSecretsReport({ ...report(), completeness: "done" }).completeness).toBe("unverified");
    expect(parseSecretsReport({ ...report(), completeness: undefined }).completeness).toBe("unverified");
    // ok must be literally true.
    expect(parseSecretsReport({ ...report(), ok: "true" }).ok).toBe(false);
    expect(() => parseSecretsReport(null)).toThrow();
    expect(() => parseSecretsReport([])).toThrow();
  });

  it("counts unreadable rows instead of dropping them", () => {
    const parsed = parseSecretsReport({
      ...report(),
      findings: [row(), { path: "x" }, { rule_id: "r" }, null, "row"],
      findings_total: 5,
      findings_unreadable: 1,
    });
    expect(parsed.findings).toHaveLength(1);
    expect(parsed.findings_unreadable).toBe(5);
    expect(verdict(parsed).title).toBe("5 findings+");
  });

  it("coerces unknown locations to unknown and bad numbers to zero", () => {
    const parsed = parseSecretsReport({
      ...report(),
      findings: [{ ...row(), location: "committed", line: -4, secret_group: Number.NaN }],
      findings_total: Number.POSITIVE_INFINITY,
    });
    expect(parsed.findings[0]).toMatchObject({ location: "unknown", line: 0, secret_group: 0 });
    expect(parsed.findings_total).toBe(1);
  });
});
