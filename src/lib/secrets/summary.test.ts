import { describe, expect, it } from "vitest";
import { parseSecretsReport, type SecretFinding, type SecretsReport } from "./types";
import {
  LOCATION_COPY,
  LOCATION_ORDER,
  capNote,
  distinctSecrets,
  engineLabel,
  filterFindings,
  groupSizes,
  isStale,
  isSuperseded,
  locationLabel,
  locationChips,
  secondOpinionNote,
  scopeNotes,
  skippedNote,
  scanFailureCopy,
  shouldCache,
  STALE_AFTER_MS,
  SUPERSEDED_ERROR,
  verdict,
} from "./summary";

const IN_HOUSE = "dc-redact 0.2.5 (devcouncil@b9b2314d)";

/**
 * A clean in-process scan that Kingfisher corroborated: the only fixture the
 * green headline is reachable from.
 */
function report(overrides: Partial<SecretsReport> = {}): SecretsReport {
  return {
    ok: true,
    error: null,
    diagnostic: null,
    kingfisher_present: true,
    kingfisher_version: "2.7.0",
    kingfisher_version_rejected: false,
    nested_repos_scanned: false,
    excluded_paths: [],
    completeness: "complete",
    findings: [],
    findings_total: 0,
    findings_unreadable: 0,
    findings_truncated: false,
    git_status_known: true,
    max_file_size_mb: 32,
    scanned_at_ms: 1_000_000,
    duration_ms: 10,
    engine: IN_HOUSE,
    skipped_total: 0,
    skipped: [],
    second_opinion: { state: "ran", completeness: "complete", version: "2.7.0", findings_total: 0, confirmed: 0, added: 0 },
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
      { second_opinion: { state: "not_run" } },
      { second_opinion: { state: "not_installed" } },
      { second_opinion: { state: "unavailable", reason: "kingfisher did not finish within 300s" } },
      { second_opinion: { state: "ran", completeness: "partial", version: null, findings_total: 0, confirmed: 0, added: 0 } },
      { second_opinion: { state: "ran", completeness: "unverified", version: null, findings_total: 0, confirmed: 0, added: 0 } },
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
    ).toContain("2 findings couldn't be read and are not listed.");
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
    expect(v.detail).toContain("Git status couldn't be read");
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
    expect(notes).toContain("32 MB");
    expect(notes).toContain("Symbolic links are not followed");
    expect(notes).toContain("kingfisher:ignore");
    expect(notes).toContain("nested repositories");
    expect(notes).not.toContain("Not scanned");
  });

  it("names every tree the scan left out", () => {
    const notes = scopeNotes(
      report({ excluded_paths: [".claude/worktrees/lane/", "src-tauri/target/", "tmp/"] }),
    );
    const left = notes.find((note) => note.startsWith("Not scanned"));
    expect(left).toBeDefined();
    for (const path of [".claude/worktrees/lane/", "src-tauri/target/", "tmp/"]) {
      expect(left).toContain(path);
    }
  });

  it("says how many trees it does not list when there are many", () => {
    const many = Array.from({ length: 30 }, (_, i) => `p${i}/target/`);
    const left = scopeNotes(report({ excluded_paths: many })).find((note) =>
      note.startsWith("Not scanned"),
    );
    expect(left).toContain("p0/target/");
    expect(left).toContain("and 20 more");
  });
});

describe("staleness and caching", () => {
  it("refreshes an old or untimed report", () => {
    expect(isStale(report({ scanned_at_ms: 1_000 }), 1_000 + STALE_AFTER_MS - 1)).toBe(false);
    expect(isStale(report({ scanned_at_ms: 1_000 }), 1_000 + STALE_AFTER_MS)).toBe(true);
    expect(isStale(report({ scanned_at_ms: 0 }), 1)).toBe(true);
  });

  it("tells a replaced scan from a failed one, so only the failure holds automatic scans off", () => {
    expect(isSuperseded(report({ ok: false, error: SUPERSEDED_ERROR }))).toBe(true);
    expect(isSuperseded(report({ ok: false, error: "kingfisher crashed (SIGBUS)" }))).toBe(false);
    expect(isSuperseded(report({ ok: true, error: SUPERSEDED_ERROR }))).toBe(false);
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
    expect(scanFailureCopy(report({ ok: false, error: "kingfisher couldn't be started" }))).toBe(
      "kingfisher couldn't be started",
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

  it("coerces unknown locations to unknown, bad counts to zero and a bad line to none", () => {
    const parsed = parseSecretsReport({
      ...report(),
      findings: [{ ...row(), location: "committed", line: -4, secret_group: Number.NaN }],
      findings_total: Number.POSITIVE_INFINITY,
    });
    expect(parsed.findings[0]).toMatchObject({ location: "unknown", line: null, secret_group: 0 });
    expect(parsed.findings_total).toBe(1);
  });

  it("shows no line number for a finding without one", () => {
    const parsed = parseSecretsReport({
      ...report(),
      findings: [
        { ...row(), line: null },
        { ...row({ path: "b.sh" }), line: 0 },
        { ...row({ path: "c.sh" }), line: 7 },
      ],
    });
    expect(parsed.findings.map((f) => f.line)).toEqual([null, null, 7]);
    expect(parsed.findings.map(locationLabel)).toEqual(["a.sh", "b.sh", "c.sh:7"]);
  });

  it("carries a refused scan version through the IPC boundary", () => {
    expect(parseSecretsReport({ ...report(), kingfisher_version_rejected: true })
      .kingfisher_version_rejected).toBe(true);
    expect(parseSecretsReport(report()).kingfisher_version_rejected).toBe(false);
  });
});

/** What the in-process scanner returns: no version, no confidence, no groups. */
function inHouse(overrides: Partial<SecretsReport> = {}): SecretsReport {
  return report({
    engine: IN_HOUSE,
    kingfisher_present: false,
    kingfisher_version: null,
    second_opinion: { state: "not_installed" },
    ...overrides,
  });
}

describe("the in-process engine", () => {
  it("is named in the header, with Kingfisher added once it has run", () => {
    expect(engineLabel(inHouse())).toBe(IN_HOUSE);
    expect(engineLabel(report())).toBe(`${IN_HOUSE} + Kingfisher 2.7.0`);
    expect(engineLabel(report({ engine: "" , second_opinion: { state: "not_run" } }))).toBe("unknown engine");
  });

  it("is not green on its own: a complete empty scan says only that no known shape matched", () => {
    for (const second_opinion of [
      { state: "not_installed" },
      { state: "not_run" },
      { state: "unavailable", reason: "kingfisher output was truncated" },
    ] as const) {
      const v = verdict(inHouse({ second_opinion }));
      expect(v.tone).toBe("neutral");
      expect(v.title).toBe("No known secret shapes matched");
      expect(v.title).not.toContain("No secrets reported");
      expect(v.detail).toContain(IN_HOUSE);
    }
  });

  it("points at Rescan when Kingfisher is installed and at installing it when it is not", () => {
    expect(verdict(inHouse({ kingfisher_present: true, second_opinion: { state: "not_run" } })).detail).toContain(
      "Rescan also runs Kingfisher for a second opinion.",
    );
    expect(verdict(inHouse({ second_opinion: { state: "not_installed" } })).detail).toContain(
      "Kingfisher isn't installed.",
    );
    expect(
      verdict(inHouse({ second_opinion: { state: "unavailable", reason: "kingfisher did not finish within 300s." } }))
        .detail,
    ).toContain("Kingfisher's second opinion was unavailable: kingfisher did not finish within 300s. Rescan to try again.");
  });

  it("is green only when the second opinion ran complete and both found nothing", () => {
    expect(verdict(report())).toMatchObject({ tone: "ok", title: "No secrets reported" });
    // Both ran clean, but Kingfisher reported a row: not clean.
    expect(
      verdict(report({ findings_total: 1, findings: [row()] })).tone,
    ).toBe("danger");
  });

  it("states what a second opinion added, and when it was unavailable, beside the findings", () => {
    const ran = report({
      findings: [row()],
      findings_total: 3,
      second_opinion: { state: "ran", completeness: "complete", version: "2.7.0", findings_total: 3, confirmed: 1, added: 2 },
    });
    expect(verdict(ran).detail).toContain("Kingfisher 2.7.0 reported 3 findings: 1 already listed here, 2 added.");
    const down = report({
      findings: [row()],
      findings_total: 1,
      second_opinion: { state: "unavailable", reason: "kingfisher crashed (SIGBUS)" },
    });
    expect(verdict(down).title).toBe("1 finding");
    expect(verdict(down).detail).toContain("Kingfisher's second opinion was unavailable: kingfisher crashed (SIGBUS).");
    expect(secondOpinionNote(report({ second_opinion: { state: "ran", completeness: "partial", version: null, findings_total: 0, confirmed: 0, added: 0 } }))).toContain(
      "It didn't read every file.",
    );
  });

  it("never calls a scan that left nested repositories unread clean, and names them", () => {
    const v = verdict(inHouse({ completeness: "partial", excluded_paths: ["vendor/inner/"] }));
    expect(v.tone).toBe("warn");
    expect(v.detail).toContain("vendor/inner/");
    expect(v.detail).toContain("This is not a clean result.");
  });

  it("never calls a scan with skipped inputs clean, and names them", () => {
    const skipped = [{ path: "big.bin", reason: "larger than the scan reads", bytes: 40_000_000 }];
    const partial = inHouse({ completeness: "partial", skipped_total: 3, skipped });
    const v = verdict(partial);
    expect(v.tone).toBe("warn");
    expect(v.title).toContain("partial");
    expect(v.detail).toContain("3 inputs not read: big.bin (larger than the scan reads), and 2 more.");
    expect(v.detail).toContain("not a clean result");
    expect(skippedNote(partial)).toContain("big.bin");
    expect(skippedNote(inHouse())).toBeNull();
  });

  it("states the narrower scope instead of Kingfisher's", () => {
    const notes = scopeNotes(inHouse({ excluded_paths: ["vendor/inner/"] })).join("\n");
    expect(notes).toContain("Ignored files");
    expect(notes).toContain("far fewer credential shapes");
    expect(notes).toContain("nested repositories and submodules");
    expect(notes).not.toContain("skipped without being reported");
    expect(notes).not.toContain("kingfisher:ignore");
  });

  it("describes Kingfisher's scope only once it has run, including that it reads ignored files", () => {
    const ran = scopeNotes(report()).join("\n");
    expect(ran).toContain("no option to honour .gitignore");
    expect(ran).toContain("kingfisher:ignore");
    const alone = scopeNotes(inHouse()).join("\n");
    expect(alone).not.toContain("honour .gitignore");
    expect(scopeNotes(inHouse({ second_opinion: { state: "unavailable", reason: "x" } })).join("\n")).toContain(
      "did not finish; nothing here comes from it",
    );
  });

  it("parses the second opinion strictly: absent is not run, unknown is unavailable, never ran", () => {
    const base = { ok: true, completeness: "complete", findings: [] };
    expect(parseSecretsReport(base).second_opinion).toEqual({ state: "not_run" });
    expect(parseSecretsReport({ ...base, second_opinion: { state: "not_installed" } }).second_opinion).toEqual({
      state: "not_installed",
    });
    expect(parseSecretsReport({ ...base, second_opinion: { state: "ran", completeness: "bogus" } }).second_opinion.state).toBe(
      "unavailable",
    );
    expect(parseSecretsReport({ ...base, second_opinion: "ran" }).second_opinion.state).toBe("unavailable");
    expect(parseSecretsReport({ ...base, second_opinion: { state: "mystery" } }).second_opinion.state).toBe("unavailable");
    expect(
      parseSecretsReport({
        ...base,
        second_opinion: { state: "ran", completeness: "complete", version: "2.7.0", findings_total: 4, confirmed: 1, added: -3 },
      }).second_opinion,
    ).toEqual({ state: "ran", completeness: "complete", version: "2.7.0", findings_total: 4, confirmed: 1, added: 0 });
    const token = "ghp_" + "aB3dE5gH7jK9mN1pQ3sT5vW7yZ9bC1dE3fG5";
    const redacted = parseSecretsReport({ ...base, second_opinion: { state: "unavailable", reason: `failed near ${token}` } })
      .second_opinion;
    expect(JSON.stringify(redacted)).not.toContain(token);
  });

  it("parses a report that lists skips as partial even if it declared complete", () => {
    const parsed = parseSecretsReport({
      ok: true,
      engine: IN_HOUSE,
      completeness: "complete",
      findings: [],
      skipped_total: 1,
      skipped: [{ path: "a", reason: "could not be read", bytes: null }],
    });
    expect(parsed.completeness).toBe("partial");
    expect(parsed.engine).toBe(IN_HOUSE);
    expect(parsed.skipped).toEqual([{ path: "a", reason: "could not be read", bytes: null }]);
    expect(verdict(parsed).tone).toBe("warn");
  });
});
