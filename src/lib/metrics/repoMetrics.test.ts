import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../ipc/invoke", () => ({ invoke: vi.fn() }));
vi.mock("../diagnostics/report", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../diagnostics/report")>()),
  reportPanelError: vi.fn(),
}));

const { invoke } = await import("../ipc/invoke");
const { reportPanelError } = await import("../diagnostics/report");
const { coverageMetric, locMetric, repoMetrics, storageMetric, totalCodeLines } = await import("./repoMetrics");

const invokeMock = vi.mocked(invoke);
const reportMock = vi.mocked(reportPanelError);

const REPO = "/work/alpha";

beforeEach(() => {
  invokeMock.mockReset();
  reportMock.mockReset();
});

// `dispose()` is permanent for a metric, and these are module singletons, so a
// test cleans up by forgetting the repositories it touched instead.
afterEach(() => {
  for (const metric of [locMetric, coverageMetric, storageMetric]) {
    for (const repo of [...metric.trackedRepos]) metric.forget(repo);
  }
});

describe("what each metric measures", () => {
  it.each([
    ["loc", locMetric, "cmd_get_language_stats"],
    ["coverage", coverageMetric, "cmd_scan_coverage"],
    ["storage", storageMetric, "cmd_storage_scan"],
  ] as const)("%s runs its own backend command for the repository", async (_name, metric, command) => {
    invokeMock.mockResolvedValue({ stats: [], truncated: false, scan: { truncated: false } });
    await metric.refresh(REPO, { force: true });
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith(command, { repoPath: REPO });
    expect(metric.snapshot(REPO).state).toBe("ready");
    expect(metric.snapshot(REPO).stale).toBeNull();
  });

  it("registers exactly the three metrics the repo-changed listener revalidates", () => {
    expect(repoMetrics.metrics.map((metric) => metric.name)).toEqual(["loc", "coverage", "storage"]);
  });
});

describe("a bounded scan never reads as a total", () => {
  it("marks a truncated language report partial", async () => {
    invokeMock.mockResolvedValue({ stats: [], truncated: true });
    await locMetric.refresh(REPO, { force: true });
    expect(locMetric.snapshot(REPO).stale).toBe("partial");
  });

  it("marks a truncated coverage report partial", async () => {
    invokeMock.mockResolvedValue({ truncated: true });
    await coverageMetric.refresh(REPO, { force: true });
    expect(coverageMetric.snapshot(REPO).stale).toBe("partial");
  });

  it("reads truncation for storage from the nested scan, not the report", async () => {
    invokeMock.mockResolvedValue({ truncated: true, scan: { truncated: false } });
    await storageMetric.refresh(REPO, { force: true });
    expect(storageMetric.snapshot(REPO).stale).toBeNull();

    invokeMock.mockResolvedValue({ scan: { truncated: true } });
    await storageMetric.refresh(REPO, { force: true });
    expect(storageMetric.snapshot(REPO).stale).toBe("partial");
  });

  it("does not treat a missing storage scan as partial", async () => {
    invokeMock.mockResolvedValue({});
    await storageMetric.refresh(REPO, { force: true });
    expect(storageMetric.snapshot(REPO).stale).toBeNull();
  });
});

describe("failures are recorded under the panel that used to own the fetch", () => {
  it.each([
    [locMetric, "pulse"],
    [coverageMetric, "coverage"],
    [storageMetric, "storage"],
  ] as const)("%s reports to %s", async (metric, source) => {
    invokeMock.mockRejectedValue(new Error("scan exploded"));
    await metric.refresh(REPO, { force: true });
    expect(reportMock).toHaveBeenCalledTimes(1);
    expect(reportMock).toHaveBeenCalledWith(source, "scan exploded");
    const snapshot = metric.snapshot(REPO);
    expect(snapshot.state).toBe("failed");
    expect(snapshot.error).toBe("scan exploded");
  });

  it("redacts credentials a failed git command carried before they reach the banner", async () => {
    invokeMock.mockRejectedValue(new Error("fatal: unable to access 'https://user:hunter2secret@example.com/r.git/'"));
    await locMetric.refresh(REPO, { force: true });
    const message = locMetric.snapshot(REPO).error ?? "";
    expect(message).not.toContain("hunter2secret");
    expect(reportMock.mock.calls[0]?.[1]).not.toContain("hunter2secret");
  });

  it("keeps the last good value, marked as predating the failed refresh", async () => {
    invokeMock.mockResolvedValueOnce({ stats: [{ code_lines: 7 }] });
    await locMetric.refresh(REPO, { force: true });
    invokeMock.mockRejectedValueOnce(new Error("later failure"));
    await locMetric.refresh(REPO, { force: true });
    const snapshot = locMetric.snapshot(REPO);
    expect(snapshot.value).toEqual({ stats: [{ code_lines: 7 }] });
    expect(snapshot.stale).toBe("refresh-failed");
  });
});

describe("a change only costs a refresh when the metric is derived from it", () => {
  const gitStateOnly = { kinds: ["git_state"] as const, paths: [], paths_truncated: false };
  const worktreeEdit = { kinds: ["worktree"] as const, paths: ["a.rs"], paths_truncated: false };

  it("leaves line counts and coverage current across a git-state-only change", async () => {
    invokeMock.mockResolvedValue({ stats: [], truncated: false });
    await locMetric.refresh(REPO, { force: true });
    await coverageMetric.refresh(REPO, { force: true });
    locMetric.invalidate(REPO, { ...gitStateOnly, kinds: [...gitStateOnly.kinds] });
    coverageMetric.invalidate(REPO, { ...gitStateOnly, kinds: [...gitStateOnly.kinds] });
    expect(locMetric.snapshot(REPO).stale).toBeNull();
    expect(coverageMetric.snapshot(REPO).stale).toBeNull();
  });

  it("marks them stale the instant a worktree file changes", async () => {
    invokeMock.mockResolvedValue({ stats: [], truncated: false });
    await locMetric.refresh(REPO, { force: true });
    locMetric.invalidate(REPO, { ...worktreeEdit, kinds: [...worktreeEdit.kinds] });
    expect(locMetric.snapshot(REPO).stale).toBe("repository-changed");
  });

  it("includes git state in what storage covers", async () => {
    invokeMock.mockResolvedValue({ scan: { truncated: false } });
    await storageMetric.refresh(REPO, { force: true });
    storageMetric.invalidate(REPO, { ...gitStateOnly, kinds: [...gitStateOnly.kinds] });
    expect(storageMetric.snapshot(REPO).stale).toBe("repository-changed");
  });
});

describe("only as many repositories are tracked as the panel cache holds", () => {
  it("drops the oldest once more than eight have been measured", async () => {
    invokeMock.mockResolvedValue({ stats: [], truncated: false });
    for (let index = 0; index < 12; index += 1) {
      await locMetric.refresh(`/work/repo-${index}`, { force: true });
    }
    expect(locMetric.trackedRepos.length).toBeLessThanOrEqual(8);
  });
});

describe("totalCodeLines", () => {
  it("is null when nothing was measured", () => {
    expect(totalCodeLines(null)).toBeNull();
    expect(totalCodeLines({} as never)).toBeNull();
    expect(totalCodeLines({ stats: "not-an-array" } as never)).toBeNull();
  });

  it("sums code lines and counts a missing figure as zero", () => {
    expect(totalCodeLines({ stats: [{ code_lines: 10 }, { code_lines: 5 }, {}] } as never)).toBe(15);
    expect(totalCodeLines({ stats: [] } as never)).toBe(0);
  });
});
