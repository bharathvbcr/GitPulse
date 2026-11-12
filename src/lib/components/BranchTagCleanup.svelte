<script lang="ts">
  import { untrack } from "svelte";
  import { invoke } from "../ipc/invoke";
  import { describeFailure } from "../ui/copy/describeFailure";
  import Checkbox from "./Checkbox.svelte";
  import Skeleton from "./Skeleton.svelte";
  import {
    AlertTriangle,
    CheckCircle2,
    ChevronRight,
    GitBranch,
    Tag,
    Trash2,
  } from "@lucide/svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { askConfirm } from "../stores/modalStore";
  import { offerBranchUndo } from "../repos/branchUndo";
  import {
    splitBranchCleanup,
    type BranchCleanupPlan,
    type TagCleanupPlan,
  } from "../ops/model";
  import { manviSectionId } from "../ui/manviFocus";

  /**
   * Branch and tag cleanup, as a disclosure on Work → Overview.
   *
   * It is always drawn, collapsed, and never hidden when nothing was found:
   * the scan is a manual action, so an absent card would read as "checked,
   * clean" when nobody had checked. The count chip appears only after a scan
   * has run, for the same reason — a "0" before then is a claim.
   *
   * The component stays mounted while collapsed so the plans survive closing
   * the disclosure; only its body is conditional. `open` is bindable so a deep
   * link can expand it before scrolling to the anchor.
   */
  let { open = $bindable(false) }: { open?: boolean } = $props();

  type Busy = "branches" | "clean" | "tags" | "tag-clean";

  let cleanup = $state<BranchCleanupPlan | null>(null);
  let selectedBranches = $state<string[]>([]);
  let tagPlan = $state<TagCleanupPlan | null>(null);
  let selectedTags = $state<string[]>([]);
  let busy = $state<Busy | null>(null);
  let notice = $state<string | null>(null);

  /** "Couldn't <verb> <object>. <Reason>." for a notice. */
  function failure(verb: string, object: string, cause: unknown): string {
    const copy = describeFailure({ verb, object }, cause);
    return copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
  }

  // Same shape as cleanupInvariant: every tag must land in exactly one bucket,
  // or the split is not describing the repository and deletion is unsafe.
  let tagInvariant = $derived(
    tagPlan
      ? tagPlan.uncompared_tags + tagPlan.retained_count + tagPlan.deletable_count ===
          tagPlan.total_tags
      : true,
  );

  let cleanupInvariant = $derived(
    cleanup
      ? cleanup.protected_branches + cleanup.unmerged_branches + cleanup.candidates.length ===
          cleanup.total_local_branches
      : true,
  );

  function isSelected(name: string): boolean {
    return selectedBranches.includes(name);
  }

  function toggleSelected(name: string) {
    selectedBranches = isSelected(name)
      ? selectedBranches.filter((item) => item !== name)
      : [...selectedBranches, name];
  }

  async function scanBranches() {
    const repo = $currentRepoPath;
    if (!repo || busy !== null) return;
    busy = "branches";
    notice = null;
    try {
      const next = await invoke<BranchCleanupPlan>("cmd_branch_cleanup_plan", { repoPath: repo });
      if ($currentRepoPath !== repo) return;
      cleanup = next;
      selectedBranches = next.candidates.map((candidate) => candidate.name);
    } catch (error) {
      if ($currentRepoPath === repo) notice = failure("scan", "branches", error);
    } finally {
      busy = null;
    }
  }

  async function scanTags() {
    const repo = $currentRepoPath;
    if (!repo || busy !== null) return;
    busy = "tags";
    notice = null;
    try {
      const next = await invoke<TagCleanupPlan>("cmd_tag_cleanup_plan", { repoPath: repo });
      if ($currentRepoPath !== repo) return;
      tagPlan = next;
      // Nothing pre-selected: unlike a merged branch, a tag is often kept on
      // purpose, so deleting one is a choice the user makes per row.
      selectedTags = [];
    } catch (error) {
      if ($currentRepoPath === repo) notice = failure("scan", "tags", error);
    } finally {
      busy = null;
    }
  }

  function toggleTag(name: string) {
    selectedTags = selectedTags.includes(name)
      ? selectedTags.filter((item) => item !== name)
      : [...selectedTags, name];
  }

  async function cleanTags() {
    if (selectedTags.length === 0) return;
    const repo = $currentRepoPath;
    // The repository these tags belong to, held across the confirm and the loop.
    const target = repoStore.captureTarget();
    if (!repo || !target) return;
    const names = [...selectedTags];
    const confirmed = await askConfirm({
      title: `Delete ${names.length} tag${names.length === 1 ? "" : "s"}?`,
      message: `Delete ${names.length} tag${names.length === 1 ? "" : "s"} whose commits are already in ${tagPlan?.compared_to ?? "the base"}?\n\n${names.join("\n")}`,
      confirmLabel: "Delete",
    });
    if (!confirmed) return;
    busy = "tag-clean";
    notice = null;
    let deleted = 0;
    const failures: string[] = [];
    try {
      for (const name of names) {
        const outcome = await repoStore.deleteTag(name, target);
        if (outcome.ok) deleted += 1;
        else failures.push(`${name}: ${outcome.error ?? "no reason reported"}`);
        // The repository changed: every remaining tag would be refused too.
        if (outcome.refused) break;
      }
      if ($currentRepoPath === repo) {
        notice = failures.length
          ? `Deleted ${deleted} of ${names.length}. ${failures.join("; ")}`
          : `Deleted ${deleted} tag${deleted === 1 ? "" : "s"}.`;
        selectedTags = [];
      }
    } finally {
      busy = null;
    }
    if ($currentRepoPath === repo) await scanTags();
  }

  async function cleanBranches() {
    if (selectedBranches.length === 0) return;
    const repo = $currentRepoPath;
    // The repository these branches belong to, held across the confirm, the
    // loop and the Undo toast.
    const target = repoStore.captureTarget();
    if (!repo || !target || !cleanup) return;
    const selected = cleanup.candidates.filter((c) => selectedBranches.includes(c.name));
    if (selected.length === 0) return;
    const names = selected.map((c) => c.name);
    const confirmed = await askConfirm({
      title: `Delete ${names.length} merged branch${names.length === 1 ? "" : "es"}?`,
      message: `Delete ${names.length} merged local branch${names.length === 1 ? "" : "es"}?\n\n${names.join("\n")}`,
      confirmLabel: "Delete",
    });
    if (!confirmed) {
      return;
    }
    busy = "clean";
    notice = null;
    let deleted = 0;
    const failures: string[] = [];
    // Tips captured from the plan before delete — same restore input the
    // sidebar toast uses. The durable ledger before_ref survives toast death.
    const restoredTips: { name: string; tip: string }[] = [];
    try {
      const { ancestry, byTree } = splitBranchCleanup(selected, selectedBranches);
      let repositoryRefused = false;
      for (const candidate of ancestry) {
        const tip = candidate.tip_commit_id;
        const outcome = await repoStore.deleteBranch(candidate.name, false, target);
        if (outcome.ok) {
          deleted += 1;
          if (tip) restoredTips.push({ name: candidate.name, tip });
        } else {
          failures.push(`${candidate.name}: ${outcome.error ?? "failed"}`);
        }
        // The repository changed: every remaining branch would be refused too.
        if (outcome.refused) {
          repositoryRefused = true;
          break;
        }
      }
      // Squash- and rebase-merged branches: `git branch -d` refuses them, so
      // the gated dead-branch clean deletes them. It re-scans, backs up, and
      // forces only a branch it re-proves merged by tree.
      if (byTree.length > 0 && !repositoryRefused) {
        const outcome = await repoStore.deadbranchClean(
          byTree.map((c) => c.name),
          false,
          true,
          target,
        );
        if (outcome.ok && outcome.output) {
          const gone = new Set(outcome.output.deleted);
          for (const candidate of byTree) {
            if (gone.has(candidate.name)) {
              deleted += 1;
              if (candidate.tip_commit_id) {
                restoredTips.push({ name: candidate.name, tip: candidate.tip_commit_id });
              }
            }
          }
          for (const [name, reason] of outcome.output.failed) failures.push(`${name}: ${reason}`);
          // A branch the clean neither deleted nor reported was not re-proven merged.
          const failedNames = new Set(outcome.output.failed.map(([name]) => name));
          for (const candidate of byTree) {
            if (!gone.has(candidate.name) && !failedNames.has(candidate.name)) {
              failures.push(`${candidate.name}: not deleted (no longer provably merged)`);
            }
          }
        } else {
          failures.push(`${byTree.map((c) => c.name).join(", ")}: ${outcome.error ?? "failed"}`);
        }
      }
      // The Undo is bound to the repository the delete ran in, so it is offered
      // even when another tab is active now. The notice and rescan describe the
      // active repository, and only apply while it is still this one.
      if (restoredTips.length > 0) {
        offerBranchUndo(
          restoredTips.length === 1
            ? `Deleted branch "${restoredTips[0].name}"`
            : `Deleted ${restoredTips.length} merged branches`,
          restoredTips,
          target,
        );
      }
      if ($currentRepoPath === repo) {
        notice = failures.length
          ? `Deleted ${deleted} of ${names.length}. ${failures.join(" ")}`
          : `Deleted ${deleted} merged branch${deleted === 1 ? "" : "es"}.`;
      }
    } finally {
      busy = null;
    }
    if ($currentRepoPath === repo) await scanBranches();
  }

  /** The existing stale-branch dialog, reached by the event every other entry point uses. */
  function openStaleBranches() {
    window.dispatchEvent(new CustomEvent("gitpulse:branch-cleanup"));
  }

  // A plan describes one repository. Keyed on the repository alone: repoStore
  // republishes on every status poll, and resetting per emission would throw
  // the plan away under the reader. A tag plan left over from another
  // repository would offer deletions by name in the wrong one.
  $effect(() => {
    void $currentRepoPath;
    untrack(() => {
      cleanup = null;
      selectedBranches = [];
      tagPlan = null;
      selectedTags = [];
      notice = null;
    });
  });
</script>

<section id={manviSectionId("cleanup")} tabindex="-1" class="mt-4 w-full rounded-xl">
  <button
    type="button"
    class="flex w-full items-center gap-1.5 rounded-xl border border-edge px-3 py-2 text-meta font-medium text-textMuted hover:bg-surfaceHover"
    onclick={() => (open = !open)}
    aria-expanded={open}
    aria-controls="branch-tag-cleanup-body"
  >
    <ChevronRight size={13} class="shrink-0 transition-transform {open ? 'rotate-90' : ''}" />
    Branch and tag cleanup
    <!-- Only after a scan has run: a count before then would claim a check
         nobody made. Zero after a scan is a real answer and is shown. -->
    {#if cleanup}
      <span class="gp-chip ml-auto" data-tone="neutral" data-kind="count" title="Merged local branches the last scan found eligible">
        {cleanup.candidates.length} {cleanup.candidates.length === 1 ? "branch" : "branches"}
      </span>
    {/if}
    {#if tagPlan}
      <span class="gp-chip {cleanup ? '' : 'ml-auto'}" data-tone="neutral" data-kind="count" title="Tags the last scan found redundant with {tagPlan.compared_to ?? 'the base'}">
        {tagPlan.deletable_count} {tagPlan.deletable_count === 1 ? "tag" : "tags"}
      </span>
    {/if}
  </button>

  {#if open}
    <div id="branch-tag-cleanup-body" class="mt-2 space-y-3 text-xs text-textPrimary">
      <div class="flex flex-wrap items-center gap-2">
        <button type="button" class="gp-btn" data-size="sm" onclick={scanBranches} disabled={busy !== null}>
          <GitBranch size={13} /> Scan branches
        </button>
        <button type="button" class="gp-btn" data-size="sm" onclick={openStaleBranches}>
          Stale branches…
        </button>
      </div>

      {#if notice}
        <div class="gp-notice" data-tone="info" role="status">{notice}</div>
      {/if}

      <div class="grid gap-4 xl:grid-cols-2">
        <section class="gp-card p-4">
          <div class="mb-3 flex items-center justify-between">
            <div>
              <h3 class="font-semibold">Branch cleanup</h3>
              <p class="text-textMuted">Dry-run first; current, default, worktree, and unmerged branches stay protected.</p>
            </div>
            {#if cleanup}<span class="gp-chip" data-tone="neutral" data-kind="label">{cleanup.candidates.length} eligible / {cleanup.total_local_branches} local</span>{/if}
          </div>
          {#if busy === "branches" || busy === "clean"}
            <div class="py-3" aria-label="Inspecting Git refs"><Skeleton variant="text" count={3} /></div>
          {:else if cleanup}
            {#if !cleanupInvariant}
              <div class="gp-notice mb-2" data-tone="danger" role="alert"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Cleanup coverage is inconsistent; deletion is disabled.</div>
            {/if}
            <div class="max-h-52 space-y-1 overflow-auto">
              {#each cleanup.candidates as branch, i (`${branch.name}#${i}`)}
                <div class="gp-row">
                  <Checkbox checked={isSelected(branch.name)} ariaLabel="Select {branch.name}" onchange={() => toggleSelected(branch.name)} />
                  <span class="min-w-0 flex-1">
                    <span class="font-mono">{branch.name}</span>
                    <span class="ml-2 font-mono text-textMuted" title={branch.tip_commit_id}>{branch.tip_commit_id.slice(0, 7)}</span>
                    <span class="ml-2 truncate text-textMuted">{branch.last_summary}</span>
                  </span>
                  {#if branch.upstream_gone}<span class="gp-chip" data-tone="warn" data-kind="label">upstream gone</span>{/if}
                </div>
              {:else}
                <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No merged local branches need cleanup.</div>
              {/each}
            </div>
            <div class="gp-separator" aria-hidden="true"></div>
            <div class="mt-3 flex items-center justify-between pt-3 text-textMuted">
              <span>{cleanup.protected_branches} protected · {cleanup.unmerged_branches} unmerged</span>
              {#if cleanup.unchecked_branches > 0}
                <span class="text-warn" title={cleanup.unchecked_reason ?? ""}>
                  {cleanup.unchecked_branches} not checked for squash merges{cleanup.unchecked_reason ? `: ${cleanup.unchecked_reason}` : ""}
                </span>
              {/if}
              <button class="gp-btn" data-variant="danger" data-size="sm" onclick={cleanBranches} disabled={!cleanupInvariant || selectedBranches.length === 0 || busy !== null}>
                <Trash2 size={13} /> Delete {selectedBranches.length} selected
              </button>
            </div>
          {:else}
            <p class="text-textMuted">Not scanned yet. Scan branches builds the plan.</p>
          {/if}
        </section>

        <section class="gp-card p-4">
          <div class="mb-3 flex items-center justify-between">
            <div>
              <h3 class="font-semibold">Tag retention</h3>
              <p class="text-textMuted">A commit held by a tag alone is on no branch, so branch cleanup counts it nowhere.</p>
            </div>
            {#if tagPlan}<span class="gp-chip" data-tone="neutral" data-kind="label">{tagPlan.retained_count} holding work / {tagPlan.total_tags} tags</span>{/if}
          </div>
          {#if busy === "tags" || busy === "tag-clean"}
            <div class="py-3" aria-label="Measuring tags against the base"><Skeleton variant="text" count={3} /></div>
          {:else if tagPlan}
            {#if !tagInvariant}
              <div class="gp-notice mb-2" data-tone="danger" role="alert"><AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" /> Tag coverage is inconsistent; deletion is disabled.</div>
            {/if}
            {#if tagPlan.truncated}
              <div class="gp-notice mb-2" data-tone="warn" role="status">The tag listing was capped, so these counts describe a sample of this repository's tags, not all of them.</div>
            {/if}
            {#if tagPlan.total_tags === 0}
              <div class="flex items-center gap-2 py-5 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> This repository has no tags.</div>
            {:else}
              {#if tagPlan.retained_count > 0}
                <p class="mb-1 text-textSecondary">Holding work {tagPlan.compared_to ?? "the base"} does not have — kept, not deletable.</p>
                <div class="max-h-40 space-y-1 overflow-auto">
                  {#each tagPlan.retained as tag, i (`${tag.name}#${i}`)}
                    <div class="flex items-center gap-2 rounded-lg px-2 py-1.5">
                      <Tag size={13} class="shrink-0 text-warn" />
                      <span class="min-w-0 flex-1 truncate font-mono">{tag.name}</span>
                      <span class="gp-chip" data-tone="warn" data-kind="count" title="{tag.commits_ahead_of_base} commits not in {tagPlan.compared_to ?? 'the base'}">+{tag.commits_ahead_of_base}</span>
                    </div>
                  {/each}
                </div>
                {#if tagPlan.retained.length < tagPlan.retained_count}
                  <div class="mt-1 text-warn">Showing {tagPlan.retained.length} of {tagPlan.retained_count} tags holding work.</div>
                {/if}
              {/if}
              <p class="mt-3 mb-1 text-textSecondary">Every commit already in {tagPlan.compared_to ?? "the base"} — safe to delete.</p>
              <div class="max-h-40 space-y-1 overflow-auto">
                {#each tagPlan.candidates as tag, i (`${tag.name}#${i}`)}
                  <div class="gp-row">
                    <Checkbox checked={selectedTags.includes(tag.name)} ariaLabel="Select {tag.name}" onchange={() => toggleTag(tag.name)} />
                    <span class="min-w-0 flex-1 truncate"><span class="font-mono">{tag.name}</span><span class="ml-2 text-textMuted">{tag.summary}</span></span>
                  </div>
                {:else}
                  <div class="flex items-center gap-2 py-4 text-textMuted"><CheckCircle2 size={15} class="text-ok" /> No tags are redundant with {tagPlan.compared_to ?? "the base"}.</div>
                {/each}
              </div>
              {#if tagPlan.candidates.length < tagPlan.deletable_count}
                <div class="mt-1 text-warn">Showing {tagPlan.candidates.length} of {tagPlan.deletable_count} deletable tags.</div>
              {/if}
            {/if}
            <div class="gp-separator" aria-hidden="true"></div>
            <div class="mt-3 flex items-center justify-between pt-3 text-textMuted">
              <span>
                {#if tagPlan.uncompared_tags > 0}
                  <!-- Not folded into either bucket: these were never measured. -->
                  {tagPlan.uncompared_tags} couldn't be compared
                {:else if tagPlan.compared_to}
                  measured against {tagPlan.compared_to}
                {:else}
                  no base to compare against
                {/if}
              </span>
              <button class="gp-btn" data-variant="danger" data-size="sm" onclick={cleanTags} disabled={!tagInvariant || selectedTags.length === 0 || busy !== null}>
                <Trash2 size={13} /> Delete {selectedTags.length} selected
              </button>
            </div>
          {:else}
            <button class="gp-btn" onclick={scanTags} disabled={busy !== null}><Tag size={13} /> Build tag plan</button>
          {/if}
        </section>
      </div>
    </div>
  {/if}
</section>
