<script lang="ts">
  /**
   * The offer to widen a pre-repository approval to a repository's worktrees.
   *
   * Lives above the branch list rather than inside the worktrees panel, which
   * is where it started. The rows it is about are down there, and putting the
   * offer beside them read better — but the sidebar body is one scroller with
   * no height cap on the branch list, so on a repository with many branches
   * the banner sat below the fold and the refusal that sent people looking for
   * it named a control they could not see. Position beats adjacency here: this
   * is the one thing in the sidebar that explains why the rest is incomplete.
   *
   * It renders nothing at all unless the backend says extending would change
   * something, so the cost of being early in the document is a `{#if}` that is
   * false for every repository that has nothing to fix.
   */
  import { invoke } from "../ipc/invoke";
  import { AlertTriangle } from "@lucide/svelte";
  import { untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { needsExtension, type TrustPreview } from "../repos/repositoryTrust";
  import { announceTrustExtended } from "../repos/trustExtension";

  /**
   * Set when this repository was approved before the repository became the
   * unit of trust, so its siblings are refused and extending would change
   * that.
   */
  let trustExtendable = $state(false);
  let inflight: AsyncGuard | null = null;

  $effect(() => {
    const repo = $currentRepoPath;
    untrack(() => {
      trustExtendable = false;
      if (repo) void loadTrustScope(repo);
    });
    return () => inflight?.cancel();
  });

  /**
   * Asks how far this repository's approval reaches.
   *
   * Non-fatal, and for a stricter reason than most loads here: this decides
   * whether to *offer* something, never whether anything is allowed. An
   * inspection that could not run leaves the offer hidden — proposing a fix
   * for a condition we did not establish would be a guess wearing a button.
   */
  async function loadTrustScope(repo: string) {
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    try {
      const preview = await invoke<TrustPreview>("cmd_repository_trust", { repoPath: repo });
      if (!guard.isLive()) return;
      trustExtendable = needsExtension(preview);
    } catch {
      if (guard.isLive()) trustExtendable = false;
    }
  }

  /** Offers the extension, then tells the rows below to fill themselves in. */
  async function extendTrust() {
    const repo = $repoStore.currentPath;
    if (!repo) return;
    await repoStore.trustRepo(repo);
    // The dialog is awaited, so the tab can change under it; announcing then
    // would reload another repository's panel on this one's decision.
    if ($repoStore.currentPath !== repo) return;
    await loadTrustScope(repo);
    announceTrustExtended();
  }
</script>

{#if trustExtendable}
  <div class="gp-notice" data-tone="warn">
    <AlertTriangle size={11} class="shrink-0 mt-px" style="color: var(--gp-notice-ink)" />
    <div class="min-w-0 flex-1">
      <p class="text-meta leading-snug">
        This repository was approved before GitPulse covered worktrees, so only its main
        checkout can be read. The others are left out of comparisons and collision checks.
      </p>
      <button
        type="button"
        onclick={extendTrust}
        class="gp-btn mt-1"
        data-variant="ghost"
        data-size="sm"
      >Extend trust to every worktree</button>
    </div>
  </div>
{/if}
