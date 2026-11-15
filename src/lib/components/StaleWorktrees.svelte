<script lang="ts">
  import { invoke } from "../ipc/invoke";
  import { repoStore } from "../stores/repoStore";
  import { reportFailure } from "../stores/mutationOutcome";
  import { storageMetric } from "../metrics/repoMetrics";
  import { formatError } from "../ui/formatError";
  import { humanBytes } from "../storage/format";
  import { worktreeSizeText } from "../storage/worktreeSize";
  import { cloneBudgetOf, privateBytesTotal } from "../storage/hygiene/reclaimConfig";
  import type { CleanerState } from "../storage/hygiene/globalTypes";
  import type { StaleWorktreeReport } from "../storage/staleWorktreeTypes";
  import type { StorageReport } from "../storage/types";
  import { Search, Trash2 } from "@lucide/svelte";

  let { repoPath, storage }: { repoPath: string; storage: StorageReport } = $props();

  let staleReport = $state<StaleWorktreeReport | null>(null);
  let finding = $state(false);
  let error = $state<string | null>(null);
  let removeError = $state<string | null>(null);
  /** The row whose Remove… was clicked and now waits for its explicit confirmation. */
  let confirming = $state<string | null>(null);
  let removing = $state<string | null>(null);
  /** The advisory byte limit; null until read, and null again when it could not be. */
  let maxPrivateBytes = $state<number | null>(null);
  let budgetError = $state<string | null>(null);
  let epoch = 0;

  const sums = $derived(privateBytesTotal(storage.worktrees));
  const partialSum = $derived(Math.max(storage.worktrees_listed ?? 0, storage.worktrees.length) > storage.worktrees.length);
  const overBudget = $derived(maxPrivateBytes !== null && maxPrivateBytes > 0 && sums.total > maxPrivateBytes);
  const unexamined = $derived(staleReport ? Math.max(0, staleReport.listed - staleReport.examined) : 0);

  async function loadBudget(mine: number) {
    try {
      const state = await invoke<CleanerState>("cmd_cleaner_state");
      if (mine !== epoch) return;
      maxPrivateBytes = cloneBudgetOf(state.config).max_private_bytes;
      budgetError = null;
    } catch (e) {
      if (mine !== epoch) return;
      maxPrivateBytes = null;
      budgetError = formatError(e);
    }
  }

  // A different repository drops the previous one's findings: they described other worktrees.
  $effect(() => {
    void repoPath;
    epoch++;
    staleReport = null; error = null; removeError = null; confirming = null; removing = null; finding = false;
    void loadBudget(epoch);
    return () => { epoch++; };
  });

  async function find() {
    if (finding || removing) return;
    const mine = epoch; finding = true; error = null; confirming = null;
    try {
      const next = await invoke<StaleWorktreeReport>("cmd_storage_stale_worktrees", { repoPath });
      if (mine === epoch) staleReport = next;
    } catch (e) { if (mine === epoch) error = formatError(e); }
    finally { if (mine === epoch) finding = false; }
  }

  async function remove(path: string) {
    if (removing || confirming !== path) return;
    const target = repoStore.captureTarget();
    if (!target) { removeError = "Couldn't remove the worktree. No repository is open; nothing was run."; return; }
    const mine = epoch; removing = path; removeError = null;
    try {
      const trusted = await repoStore.trustRepo(path);
      if (!trusted) return;
      if (mine !== epoch || !repoStore.targetIsActive(target)) { removeError = "The repository changed while this was waiting; nothing was run."; return; }
      // Never forced: a worktree git refuses to remove stays, and the refusal is shown.
      const outcome = await repoStore.removeWorktree(path, false, target, true);
      if (mine !== epoch) return;
      if (!outcome.ok) {
        removeError = outcome.error ?? "The worktree was not removed.";
        reportFailure(outcome, "Couldn't remove the worktree");
        return;
      }
      confirming = null;
      void storageMetric.refresh(repoPath, { force: true });
      removing = null;
      await find();
    } catch (e) { if (mine === epoch) removeError = formatError(e); }
    finally { if (mine === epoch) removing = null; }
  }
</script>

<section class="gp-page-section" aria-label="Stale linked worktrees">
  <h3 class="gp-label">Stale linked worktrees</h3>
  {#if overBudget && maxPrivateBytes !== null}
    <p role="status" class="gp-notice mb-2" data-tone="warn">
      Linked worktrees hold {partialSum ? "at least " : ""}{humanBytes(sums.total)} that removing them would free, over your advisory limit of {humanBytes(maxPrivateBytes)}.
      {#if partialSum}That sum covers only the {sums.sized} sized worktrees of {storage.worktrees_listed} listed; the rest are not counted.{/if}
      Nothing is removed for exceeding it.
    </p>
  {/if}
  {#if budgetError}<p class="mb-2 text-xs text-textMuted">Couldn't read the advisory byte limit, so none is applied here. {budgetError}</p>{/if}
  <div class="gp-card space-y-2 p-3 text-xs">
    <p class="text-textMuted">A linked worktree is stale only when no process works in it, it is clean and unlocked, and its commits are merged or idle. A check that failed to run keeps it off this list. Removal never forces.</p>
    <button class="gp-btn" data-size="sm" onclick={find} disabled={finding || removing !== null}><Search size={12} />{finding ? "Checking worktrees…" : "Find stale worktrees"}</button>
    {#if error}<p role="alert" class="gp-notice" data-tone="danger">{error}</p>{/if}
    {#if removeError}<p role="alert" class="gp-notice" data-tone="danger">{removeError}</p>{/if}
    {#if staleReport}
      <p class="text-textSecondary" data-testid="stale-coverage">Examined {staleReport.examined} of {staleReport.listed} linked worktrees · idle threshold {staleReport.idle_days} days</p>
      {#if unexamined > 0}<p class="gp-notice" data-tone="warn">{unexamined} linked worktrees were not examined, so this list is not complete.</p>{/if}
      {#each staleReport.issues as issue}<p class="gp-notice break-all" data-tone="warn">{issue}</p>{/each}
      {#if staleReport.worktrees.length === 0}
        <p class="text-textMuted">No linked worktrees were examined.</p>
      {:else}
        <table class="w-full text-left">
          <tbody>
            {#each staleReport.worktrees as row (row.path)}
              <tr class="align-top even:bg-surfaceHover/40">
                <td class="px-2 py-1.5 font-mono text-textPrimary" title={row.path}>{row.name}</td>
                <td class="px-2 py-1.5 text-textSecondary">{row.branch ?? "detached"}</td>
                <td class="px-2 py-1.5 text-textSecondary">{row.stale ? "stale" : row.reasons.join("; ") || "not stale"}</td>
                <td class="px-2 py-1.5 text-right font-mono text-textSecondary">{row.size ? worktreeSizeText(row.size) : row.stale ? "size not measured" : ""}</td>
                <td class="px-2 py-1.5 text-right whitespace-nowrap">
                  {#if row.stale}
                    {#if confirming === row.path}
                      <span class="mr-2 text-warn">Remove {row.name} and its directory?</span>
                      <button class="gp-btn" data-size="sm" data-variant="danger" onclick={() => remove(row.path)} disabled={removing !== null}><Trash2 size={12} />{removing === row.path ? "Removing…" : "Confirm remove"}</button>
                      <button class="gp-btn" data-size="sm" onclick={() => (confirming = null)} disabled={removing !== null}>Cancel</button>
                    {:else}
                      <button class="gp-btn" data-size="sm" onclick={() => { confirming = row.path; removeError = null; }} disabled={removing !== null}>Remove…</button>
                    {/if}
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  </div>
</section>
