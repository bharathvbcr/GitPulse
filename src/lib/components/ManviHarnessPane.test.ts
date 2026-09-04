import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import ManviHarnessPane, {
  activityHistoryPresentations,
  createRepositoryRefreshTimer,
  scopedRunnerPresentation,
} from "./ManviHarnessPane.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "ManviHarnessPane.svelte"),
  "utf8",
);

describe("ManviHarnessPane capability truth", () => {
  it("distinguishes the user PTY from scoped model-authored execution", () => {
    const { body } = render(ManviHarnessPane);
    expect(body).toContain("Capability boundary");
    expect(body).toContain("Interactive shell");
    expect(body).toContain("User only");
    expect(body).toContain("Scoped action runner");
    expect(body).toContain("purpose allowlist");
  });

  it("does not claim the embedded sidecar exposes native agent tools", () => {
    expect(source).toContain("policy and local-model planes only");
    expect(source).toContain("No autonomous PTY or app-control API");
  });

  it("describes the scoped runner from the current permission state", () => {
    expect(scopedRunnerPresentation("connected")).toMatchObject({
      label: "Guarded",
      tone: "ready",
    });
    expect(scopedRunnerPresentation("unguarded")).toMatchObject({
      label: "Not checked",
      tone: "warning",
    });
    expect(scopedRunnerPresentation("blocked")).toMatchObject({
      label: "Blocked",
      tone: "error",
    });
    expect(scopedRunnerPresentation("not-probed")).toMatchObject({
      label: "Status unknown",
      tone: "neutral",
    });
    expect(scopedRunnerPresentation("connected", "refresh failed")).toMatchObject({
      label: "Status stale",
      tone: "error",
    });
    expect(source).not.toContain(">Available</span>");
  });

  it("links the actual health, coverage, terminal and CI surfaces", () => {
    for (const tab of ["health", "coverage", "terminal", "github"]) {
      expect(source).toContain(`openCapability("${tab}")`);
    }
    expect(source).toContain("cargo-llvm-cov");
    expect(source).toContain("several minutes");
  });

  it("renders the persisted MANVI grant shape and truthful recovery guidance", () => {
    expect(source).toContain("grant.grantor.id");
    expect(source).toContain("grant.scope.rules");
    expect(source).toContain("grant.scope.paths");
    expect(source).toContain("grant.scope.once");
    expect(source).toContain("Current MANVI CLI has no grant-revocation command");
    expect(source).toContain("{grants.path}");
    expect(source).not.toContain(["manvi", "grants", "revoke"].join(" "));
    expect(source).not.toContain('"any policy rule"');
    expect(source).not.toContain('grant.grantor.authority || "unknown"');
  });

  it("renders grant IPC failures instead of hiding the authority section", () => {
    expect(source).toContain("let grantsLoadError = $state<string | null>(null);");
    expect(source).toContain("grantsLoadError = redactDiagnosticText(formatError(error));");
    expect(source).toContain("{#if grantsLoadError}");
    expect(source).toContain("Grant history could not be refreshed");
  });

  it("refreshes grants and expiry labels while the pane remains open", () => {
    vi.useFakeTimers();
    try {
      const refreshed: string[] = [];
      const timer = createRepositoryRefreshTimer(
        10_000,
        (repo) => refreshed.push(repo),
        {
          setInterval: (callback, delay) => globalThis.setInterval(callback, delay),
          clearInterval: (handle) =>
            globalThis.clearInterval(handle as ReturnType<typeof globalThis.setInterval>),
        },
      );
      timer.update("/repo");

      // RepoStore can publish the same active path every ~6s while status is
      // changing. Those publications must not postpone the 10s grant tick.
      for (let elapsed = 6_000; elapsed <= 30_000; elapsed += 6_000) {
        vi.advanceTimersByTime(6_000);
        timer.update("/repo");
      }
      expect(refreshed).toEqual(["/repo", "/repo", "/repo"]);

      timer.update("/other");
      vi.advanceTimersByTime(9_999);
      expect(refreshed).toHaveLength(3);
      vi.advanceTimersByTime(1);
      expect(refreshed.at(-1)).toBe("/other");
      timer.update(null);
      vi.advanceTimersByTime(20_000);
      expect(refreshed).toHaveLength(4);
      timer.dispose();
    } finally {
      vi.useRealTimers();
    }

    expect(source).toContain("grantRefreshTimer.update(repo)");
    expect(source).toContain("onDestroy(() => grantRefreshTimer.dispose())");
    expect(source).toContain("void refreshGrants(repo, false)");
    expect(source).toContain("selectActiveGrants(grants?.grants ?? [], grantClock)");
    expect(source).toContain("grantLifecycle(grant, grantClock)");
  });

  it("shows retained valid grants alongside a refused-entry warning", () => {
    expect(source).toContain("{#if grants?.error}");
    expect(source).toContain("{#if grants?.grants.length === 0}");
    expect(source).not.toContain("{:else if grants.grants.length === 0}");
  });

  it("keys journal rows by their repository-aware durable identity", () => {
    expect(source).toContain("{#each recentActions as action (action.identity)}");
  });

  it("rejects a stale A grant response after switching to B", () => {
    expect(source).toContain("const grantRequests = beginGeneration();");
    expect(source).toContain("const generation = grantRequests.next();");
    expect(source).toContain(
      "if (!grantRequests.isCurrent(generation) || $repoStore.currentPath !== repo) return;",
    );
    const refresh = source.slice(
      source.indexOf("async function refreshGrants"),
      source.indexOf("$effect", source.indexOf("async function refreshGrants")),
    );
    const clearIdx = refresh.indexOf("grants = null;");
    const invokeIdx = refresh.indexOf('invoke<GrantView>("cmd_grants_view"');
    expect(clearIdx).toBeGreaterThan(-1);
    expect(invokeIdx).toBeGreaterThan(clearIdx);
  });

  it("surfaces refresh, durable-ledger, and catch-up completeness independently", () => {
    const healthyLedger = {
      recording: true,
      path: "/repo/.devcouncil/ledger.sqlite",
      dropped: 0,
      error: "",
      error_code: "",
    };

    expect(activityHistoryPresentations(null, null, null)).toEqual([
      expect.objectContaining({ label: "Activity history not checked", tone: "neutral" }),
      expect.objectContaining({ label: "Catch-up not checked", tone: "neutral" }),
    ]);
    expect(
      activityHistoryPresentations("status IPC failed", healthyLedger, {
        recorded: 2,
        transcripts: 1,
        skipped_lines: 0,
        reflog_entries: 1,
        error: "",
      }),
    ).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ label: "Latest MANVI status request failed", tone: "error" }),
        expect.objectContaining({ label: "Activity history recording", tone: "ready" }),
        expect.objectContaining({ label: "Catch-up complete", tone: "ready" }),
      ]),
    );
    expect(
      activityHistoryPresentations(
        null,
        { ...healthyLedger, recording: false, dropped: 3, error: "disk full" },
        {
          recorded: 0,
          transcripts: 0,
          skipped_lines: 4,
          reflog_entries: 0,
          error: "transcript unreadable",
        },
      ),
    ).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ label: "Activity history incomplete", tone: "error" }),
        expect.objectContaining({ label: "Catch-up incomplete", tone: "warning" }),
      ]),
    );
    expect(source).toContain("activityHistoryPresentations(");
  });
});
