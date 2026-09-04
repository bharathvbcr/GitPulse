import { describe, expect, it } from "vitest";
import {
  emptyMetricsInput,
  fetchFleetSnapshot,
  metricsFromCoverage,
  metricsFromHealth,
  metricsFromLanguages,
  metricsFromStorage,
  recordFleetMetrics,
  scanRepoFamily,
} from "./client";
import type { ScanFamily } from "./types";

describe("emptyMetricsInput", () => {
  it("nulls every value so a family scan cannot erase another's", () => {
    const empty = emptyMetricsInput();
    for (const [key, value] of Object.entries(empty)) {
      if (key.endsWith("_truncated") || key === "health_complete") {
        expect(value, key).toBe(false);
      } else {
        expect(value, key).toBeNull();
      }
    }
  });
});

describe("metricsFromLanguages", () => {
  const report = (overrides: Record<string, unknown> = {}) => ({
    stats: [
      { language: "Rust", color_hex: "#0", category: "programming", code_lines: 900, file_count: 3, percentage: 60 },
      { language: "TypeScript", color_hex: "#1", category: "programming", code_lines: 500, file_count: 4, percentage: 33 },
      { language: "JSON", color_hex: "#2", category: "data", code_lines: 100, file_count: 1, percentage: 7 },
    ],
    truncated: false,
    scanned_files: 8,
    candidate_files: 8,
    ...overrides,
  });

  it("sums every language and names the largest", () => {
    const metrics = metricsFromLanguages(report());
    expect(metrics.loc).toBe(1500);
    expect(metrics.loc_language).toBe("Rust");
  });

  it("carries the backend's truncation flag through", () => {
    // A capped scan counted part of the tree; the flag is what stops the grid
    // rendering that partial total like a complete one.
    expect(metricsFromLanguages(report({ truncated: true })).loc_truncated).toBe(true);
  });

  it("reports an empty scan as zero lines with no language", () => {
    const metrics = metricsFromLanguages(report({ stats: [] }));
    expect(metrics.loc).toBe(0);
    expect(metrics.loc_language).toBeNull();
  });

  it("ignores a non-finite line count rather than poisoning the sum", () => {
    const metrics = metricsFromLanguages(
      report({
        stats: [
          { language: "Rust", color_hex: "#0", category: "programming", code_lines: Number.NaN, file_count: 1, percentage: 100 },
          { language: "Go", color_hex: "#1", category: "programming", code_lines: 10, file_count: 1, percentage: 0 },
        ],
      }),
    );
    expect(metrics.loc).toBe(10);
  });

  it("touches no other family's fields", () => {
    const metrics = metricsFromLanguages(report());
    expect(metrics.storage_bytes).toBeNull();
    expect(metrics.vulns_total).toBeNull();
    expect(metrics.coverage_pct).toBeNull();
  });
});

describe("metricsFromStorage", () => {
  const report = (overrides: Record<string, unknown> = {}) =>
    ({
      totals: {
        worktree_bytes: 100,
        git_dir_bytes: 40,
        grand_bytes: 140,
        build_artifacts_bytes: 30,
        cache_artifacts_bytes: 20,
      },
      scan: { elapsed_ms: 5, files_visited: 9, permission_denied: 0, truncated: false },
      ...overrides,
    }) as never;

  it("reports the grand total, git internals, and what is reclaimable", () => {
    const metrics = metricsFromStorage(report());
    expect(metrics.storage_bytes).toBe(140);
    expect(metrics.storage_git_bytes).toBe(40);
    // Build output plus caches — the two buckets Storage itself offers to
    // delete. Packfiles are the repository, not junk.
    expect(metrics.storage_reclaimable_bytes).toBe(50);
  });

  it("carries a truncated walk through as a floor", () => {
    const metrics = metricsFromStorage(
      report({ scan: { elapsed_ms: 5, files_visited: 9, permission_denied: 0, truncated: true } }),
    );
    expect(metrics.storage_truncated).toBe(true);
  });
});

describe("metricsFromHealth", () => {
  const report = (overrides: Record<string, unknown> = {}) =>
    ({
      audit: { info: 0, low: 1, moderate: 2, high: 3, critical: 4, unknown: 5, total: 15 },
      audit_complete: true,
      ...overrides,
    }) as never;

  it("carries every severity bucket, including the unranked one", () => {
    const metrics = metricsFromHealth(report());
    expect(metrics).toMatchObject({
      vulns_critical: 4,
      vulns_high: 3,
      vulns_moderate: 2,
      vulns_low: 1,
      vulns_unknown: 5,
      vulns_total: 15,
      health_complete: true,
    });
  });

  it("treats an absent audit_complete as incomplete", () => {
    // Zero findings from an audit that never ran must not read like zero
    // findings from one that did, and an older report carries no such flag.
    const metrics = metricsFromHealth(report({ audit_complete: undefined, audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 } }));
    expect(metrics.vulns_total).toBe(0);
    expect(metrics.health_complete).toBe(false);
  });

  it("defaults the unranked bucket to zero when the scanner omits it", () => {
    const metrics = metricsFromHealth(
      report({ audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 } }),
    );
    expect(metrics.vulns_unknown).toBe(0);
  });
});

describe("metricsFromCoverage", () => {
  it("carries the overall percentage and its truncation flag", () => {
    const metrics = metricsFromCoverage({
      overall: { lines_found: 100, lines_hit: 80, percentage: 80 },
      truncated: true,
    } as never);
    expect(metrics.coverage_pct).toBe(80);
    expect(metrics.coverage_truncated).toBe(true);
  });
});

describe("the IPC seam", () => {
  it("sends the paths as a plain array the command can deserialize", async () => {
    const seen: { cmd: string; args?: Record<string, unknown> }[] = [];
    await fetchFleetSnapshot(["/a", "/b"], async (cmd, args) => {
      seen.push({ cmd, args });
      return null as never;
    });
    expect(seen[0]).toEqual({ cmd: "cmd_fleet_snapshot", args: { repoPaths: ["/a", "/b"] } });
  });

  it("does not hand the caller's array to the backend by reference", async () => {
    const paths = ["/a"];
    let sent: string[] = [];
    await fetchFleetSnapshot(paths, async (_cmd, args) => {
      sent = args?.repoPaths as string[];
      return null as never;
    });
    paths.push("/b");
    expect(sent).toEqual(["/a"]);
  });

  it("records metrics against the repository it scanned", async () => {
    const seen: Record<string, unknown>[] = [];
    await recordFleetMetrics("/a", emptyMetricsInput(), async (_cmd, args) => {
      seen.push(args ?? {});
      return null as never;
    });
    expect(seen[0].repoPath).toBe("/a");
  });

  it("runs each family's own command, and no other", async () => {
    const commands: Record<ScanFamily, string> = {
      loc: "cmd_get_language_stats",
      storage: "cmd_storage_scan",
      health: "cmd_scan_deps_health",
      coverage: "cmd_scan_coverage",
    };
    const payloads: Record<string, unknown> = {
      cmd_get_language_stats: { stats: [], truncated: false, scanned_files: 0, candidate_files: 0 },
      cmd_storage_scan: {
        totals: {
          worktree_bytes: 0,
          git_dir_bytes: 0,
          grand_bytes: 0,
          build_artifacts_bytes: 0,
          cache_artifacts_bytes: 0,
        },
        scan: { elapsed_ms: 0, files_visited: 0, permission_denied: 0, truncated: false },
      },
      cmd_scan_deps_health: {
        audit: { info: 0, low: 0, moderate: 0, high: 0, critical: 0, total: 0 },
        audit_complete: true,
      },
      cmd_scan_coverage: { overall: { lines_found: 0, lines_hit: 0, percentage: 0 }, truncated: false },
    };
    for (const [family, expected] of Object.entries(commands) as [ScanFamily, string][]) {
      const calls: string[] = [];
      await scanRepoFamily(family, "/a", async (cmd) => {
        calls.push(cmd);
        return payloads[cmd] as never;
      });
      expect(calls).toEqual([expected]);
    }
  });
});
