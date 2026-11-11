import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { ledgerStatus } from "../ledger/types";
import type { HarnessStatus } from "../stores/harnessStore";
import {
  activityHistoryPresentations,
  ADOPT_LEDGER_LABEL,
  adoptionOffered,
  createRepositoryRefreshTimer,
} from "../harness/policyPresentation";
import PolicyActivity from "./PolicyActivity.svelte";
import { policyActivityVisible } from "../harness/policyPresentation";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "PolicyActivity.svelte"),
  "utf8",
);

const harness = (over: Partial<HarnessStatus>): HarnessStatus =>
  ({ available: true, error: "", error_code: "", ...over }) as HarnessStatus;

describe("PolicyActivity rendering", () => {
  it("is present and collapsed by default", () => {
    const { body } = render(PolicyActivity, { props: {} });
    expect(body).toContain("Policy activity");
    expect(body).toContain('aria-expanded="false"');
    expect(body).not.toContain("Agent activity");
  });

  it("renders the journal and its history status when open, not just in source", () => {
    const { body } = render(PolicyActivity, { props: { open: true } });
    expect(body).toContain("Agent activity (0)");
    expect(body).toContain("Activity history status");
    expect(body).toContain("No activity rows are currently available.");
  });

  it("anchors the card for deep links and takes focus with the scroll", () => {
    const anchor = 'id={manviSectionId("activity")}';
    expect(source).toContain(anchor);
    expect(source.slice(source.indexOf(anchor)).slice(0, 200)).toContain('tabindex="-1"');
  });
});

describe("policyActivityVisible", () => {
  it("is hidden only when Manvi is known to be absent and nothing was journaled", () => {
    expect(policyActivityVisible(harness({ available: false, error_code: "not_installed" }), 0, null)).toBe(false);
  });

  it.each([
    ["connected", harness({}), 0, null],
    // A status that was never probed is unknown, not "no Manvi".
    ["not yet probed", null, 0, null],
    ["a gate that is failing", harness({ available: false, error_code: "timeout" }), 0, null],
    ["a failed status check", harness({ available: false, error_code: "not_installed" }), 0, "ipc failed"],
    ["rows recorded without Manvi", harness({ available: false, error_code: "not_installed" }), 3, null],
  ] as const)("stays visible for %s", (_, status, actions, error) => {
    expect(policyActivityVisible(status, actions, error)).toBe(true);
  });
});

describe("PolicyActivity grant ledger", () => {
  it("renders the persisted MANVI grant shape and truthful recovery guidance", () => {
    expect(source).toContain("grant.grantor.id");
    expect(source).toContain("grant.scope.rules");
    expect(source).toContain("grant.scope.paths");
    expect(source).toContain("grant.scope.once");
    expect(source).toContain("Current Manvi CLI has no grant-revocation command");
    expect(source).toContain("{grants.path}");
    expect(source).not.toContain(["manvi", "grants", "revoke"].join(" "));
    expect(source).not.toContain('"any policy rule"');
    expect(source).not.toContain('grant.grantor.authority || "unknown"');
  });

  it("renders grant IPC failures instead of hiding the authority section", () => {
    expect(source).toContain("let grantsLoadError = $state<string | null>(null);");
    expect(source).toContain('grantsLoadError = failure("refresh", "grant history", redactDiagnosticText(formatError(error)));');
    expect(source).toContain("{#if grantsLoadError}");
    expect(source).toContain("The last verified snapshot remains below.");
  });

  it("refreshes grants and expiry labels while the disclosure stays open", () => {
    expect(source).toContain("grantRefreshTimer.update(repo)");
    expect(source).toContain("onDestroy(() => grantRefreshTimer.dispose())");
    expect(source).toContain("void refreshGrants(repo, false)");
    expect(source).toContain("selectActiveGrants(grants?.grants ?? [], grantClock)");
    expect(source).toContain("grantLifecycle(grant, grantClock)");
    expect(source).toContain("createAdaptiveTimer");
    expect(source).not.toMatch(/window\.setInterval\(/);
  });

  it("reads and polls grants only while the body is showing", () => {
    // A collapsed card has no list to keep current; closing must stop the timer.
    expect(source).toContain("if (!repo || !showing) {");
    expect(source).toContain("grantRefreshTimer.update(null);");
  });

  it("ticks every 10s however often the store republishes the same repository", () => {
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
  });

  it("shows retained valid grants alongside a refused-entry warning", () => {
    expect(source).toContain("{#if grants?.error}");
    expect(source).toContain("{#if grants?.grants.length === 0}");
    expect(source).not.toContain("{:else if grants.grants.length === 0}");
  });

  it("rejects a stale A grant response after switching to B", () => {
    expect(source).toContain("const grantRequests = beginGeneration();");
    expect(source).toContain("const generation = grantRequests.next();");
    expect(source).toContain(
      "if (!grantRequests.isCurrent(generation) || $currentRepoPath !== repo) return;",
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

  it("never shows one repository's ledger under another when the disclosure reopens", () => {
    // Loading state is requested whenever the ledger on hand was read for a different repository.
    expect(source).toContain("void refreshGrants(repo, grantsRepo !== repo);");
    expect(source).toContain("grantsRepo = repo;");
  });
});

describe("PolicyActivity journal", () => {
  it("keys journal rows by their repository-aware durable identity", () => {
    // Index-suffixed: identity can repeat when the same journal entry is
    // projected twice across reconnects — Svelte 5 rejects duplicate keys.
    expect(source).toContain("{#each recentActions as action, i (`${action.identity}#${i}`)}");
  });

  it("surfaces refresh, durable-ledger, and catch-up completeness independently", () => {
    const healthyLedger = ledgerStatus({
      recording: true,
      path: "/repo/.devcouncil/ledger.sqlite",
      dropped: 0,
      error: "",
      error_code: "",
    });

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
        refused_outside_corpus: 0,
        truncated: false,
        error: "",
      }),
    ).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ label: "Couldn't check Manvi status", tone: "error" }),
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
          refused_outside_corpus: 0,
          truncated: false,
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

  it("offers adoption whenever the history reports an error, without reading which", () => {
    const ledger = (error: string) =>
      ledgerStatus({ recording: false, path: "", dropped: 0, error, error_code: "x" });
    expect(adoptionOffered(null, null)).toBe(false);
    expect(adoptionOffered(ledger(""), { error: "" })).toBe(false);
    expect(adoptionOffered(ledger("the ledger could not be opened"), null)).toBe(true);
    expect(adoptionOffered(null, { error: "repository_mismatch" })).toBe(true);
    expect(source).toContain('invoke<string[]>("cmd_adopt_ledger"');
    expect(ADOPT_LEDGER_LABEL).toBe("Adopt this ledger");
    expect(source).toContain(`{adopting ? "Adopting…" : ADOPT_LEDGER_LABEL}`);
  });

  it("copies and clears the journal", () => {
    expect(source).toContain("copyText(lines.join(");
    expect(source).toContain("harnessStore.clearActions()");
  });
});
