<script lang="ts">
  import HealthSection from "./HealthSection.svelte";
  import HealthTable from "./HealthTable.svelte";
  import { healthSection } from "../../health/sections";
  import type { SectionCount } from "../../health/counts";
  import type { HealthTone } from "../../health/summary";
  import type { CodeScanningReport, DependabotReport } from "../../health/types";

  /**
   * Dependabot and code scanning alerts.
   *
   * These were two snippets in `HealthPanel.svelte` implementing the same
   * five-state contract — fetch failed, no `gh` CLI, no alerts, truncated,
   * alerts — in two hand-written copies, roughly eighty lines each. The copies
   * had already been fixed twice in lockstep: once when a failed IPC call was
   * being misreported as a missing CLI, and once when the "install gh" hint
   * needed gating on `is_github_remote`. A third divergence was a matter of
   * time.
   *
   * The branch order is the contract, and it is load-bearing: `error` must be
   * tested before `cli_present`, because a request that failed before it could
   * ask *anything* also reports no CLI, and reporting that as "install the
   * GitHub CLI" sends the reader to fix a tool that is already installed.
   * `requestFailed` is what distinguishes the two, and it gates the hint
   * rather than the branch.
   *
   * The two callers differ only in their columns and row markup, which arrive
   * as `columns` and the `row` snippet.
   */

  /**
   * The two wire types, not a structural copy of their common fields. A
   * component that redeclares a serde payload shape is how a backend field
   * rename reaches the UI as `undefined` with nothing to catch it, which is
   * why `wire-type-locality-contract` refuses one.
   */
  type AlertsReport = DependabotReport | CodeScanningReport;

  let {
    /** Catalog section id: "dependabot" or "code-scanning". */
    id,
    /**
     * Null when nothing has been fetched yet. That is a distinct state, and
     * rendering it as the blank space an empty result produced is how a clean
     * local audit came to read as an all-clear for a repository whose GitHub
     * alerts nobody had ever looked at.
     */
    report,
    /**
     * The fetch itself failed (IPC, spawn, network), as opposed to GitHub
     * answering with a refusal. Only a *successful* request that found no CLI
     * justifies telling the reader to install one.
     */
    requestFailed,
    /** How many alerts the report carries, with the truncation disclosed. */
    count,
    tone = null,
    /** Lowercase noun for prose: "Dependabot alerts", "code scanning alerts". */
    noun,
    columns,
    row,
  }: {
    id: string;
    report: AlertsReport | null;
    requestFailed: boolean;
    count: SectionCount;
    tone?: HealthTone | null;
    noun: string;
    columns: readonly { label: string; width?: string; hideLabel?: boolean }[];
    row: import("svelte").Snippet;
  } = $props();

  const spec = $derived(healthSection(id));
  const empty = $derived(count.shown === 0);
</script>

<HealthSection {id} count={empty ? null : count} {tone}>
  {#if !report}
    <p class="text-textMuted">
      Not checked. Use <span class="font-medium text-textSecondary">Check GitHub alerts</span>
      above to fetch them — no result has been loaded for this repository, which is
      not the same as there being none.
    </p>
  {:else if report.error}
    <div
      role="alert"
      class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-200"
    >
      Could not fetch {noun}: {report.error}
    </div>
    {#if !requestFailed && !report.cli_present && report.is_github_remote}
      <p class="text-textMuted">
        Install the <span class="font-mono">gh</span> CLI and run
        <span class="font-mono">gh auth login</span> before checking again.
      </p>
    {/if}
  {:else if !report.cli_present}
    <p class="text-textMuted">
      Install the <span class="font-mono">gh</span> CLI and run
      <span class="font-mono">gh auth login</span> to fetch {noun} for
      {report.slug || "this repository"}.
    </p>
  {:else if empty}
    <p class="text-textMuted">No open {noun} on {report.slug || "this repository"}.</p>
  {:else}
    <HealthTable caption={`${spec?.heading ?? id}: ${noun}`} {columns}>
      {@render row()}
    </HealthTable>
  {/if}
</HealthSection>
