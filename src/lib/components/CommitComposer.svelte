<script lang="ts">
  import { untrack } from "svelte";
  import { repoStore } from "../stores/repoStore";
  import {
    harnessStore,
    verdictDetail,
    verdictLabel,
    type AiGeneration,
    type PolicyVerdict,
  } from "../stores/harnessStore";
  import { Send, Sparkles, AlertTriangle, ShieldCheck, ShieldAlert, Loader } from "@lucide/svelte";
  import MarkdownBody from "./MarkdownBody.svelte";
  import { formatError } from "../ui/formatError";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { previewStore, previewSummary } from "../codeintel/previewStore";
  import { getImpactLayeredMany, cancelCodeintelQuery, newCodeintelCancelToken } from "../codeintel/client";
  import {
    composeLayeredImpacts,
    emptyComposedBlast,
    type ComposedBlastRadius,
  } from "../codeintel/blastCompose";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import BlastRadiusPanel from "./BlastRadiusPanel.svelte";
  import { tooltipWalkIncomplete } from "../codeintel/walkIncomplete";
  import { capFanout, omittedLayeredImpact } from "../codeintel/fanout";

  let stagedFiles = $derived($repoStore.statuses.filter((s) => s.is_staged));
  let dirtyCount = $derived($repoStore.statuses.length);
  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);
  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let commitMessage = $derived($repoStore.commitDraft);
  let isAmending = $derived($repoStore.isAmending);
  let includeUnstaged = $state(false);
  let isGenerating = $state(false);
  let generation = $state<AiGeneration | null>(null);
  let aiError = $state<string | null>(null);
  let commitError = $state<string | null>(null);
  let lastVerdict = $state<PolicyVerdict | null>(null);

  let quickCommit = $derived(includeUnstaged && !isAmending);
  let commitCount = $derived(quickCommit ? dirtyCount : stagedFiles.length);
  let commitDisabled = $derived(
    !commitMessage.trim() ||
      conflictedCount > 0 ||
      (quickCommit ? dirtyCount === 0 : stagedFiles.length === 0 && !isAmending),
  );

  // A1: one preview batch for staged paths — DiffFileRail reads the same store.
  let previewPaths = $derived(stagedFiles.map((s) => s.path));
  let previewPathsKey = $derived(previewPaths.slice().sort().join("\0"));

  $effect(() => {
    const repo = $repoStore.currentPath;
    void previewPathsKey;
    untrack(() => {
      void previewStore.refresh(repo, previewPaths);
    });
  });

  // A3: blast radius over the staged set (layered — no min_rung).
  let blast = $state<ComposedBlastRadius | null>(null);
  let blastLoading = $state(false);
  let blastGuard: AsyncGuard | null = null;

  $effect(() => {
    const repo = $repoStore.currentPath;
    void previewPathsKey;
    untrack(() => {
      const paths = previewPaths;
      blastGuard?.cancel();
      if (!repo || paths.length === 0) {
        blast = null;
        blastLoading = false;
        return;
      }
      const guard = createAsyncGuard();
      const cancelToken = newCodeintelCancelToken();
      blastGuard = {
        isLive: () => guard.isLive(),
        cancel: () => {
          guard.cancel();
          void cancelCodeintelQuery(cancelToken);
        },
      };
      blastLoading = true;
      const { kept, omitted } = capFanout(paths);
      void getImpactLayeredMany(repo, kept, 800, cancelToken)
        .then((results) => {
          if (!guard.isLive()) return;
          blast = composeLayeredImpacts(
            [...results, ...omitted.map(omittedLayeredImpact)],
            paths,
          );
          blastLoading = false;
        })
        .catch(() => {
          if (!guard.isLive()) return;
          blast = emptyComposedBlast("layered impact request failed");
          blastLoading = false;
        });
    });
  });

  $effect(() => () => blastGuard?.cancel());

  async function generateMessage() {
    const path = $repoStore.currentPath;
    if (!path || isGenerating) return;
    isGenerating = true;
    aiError = null;
    generation = null;
    try {
      const result = await harnessStore.generateCommitMessage(path);
      // The user may have switched repositories while generation ran; writing
      // the draft now would land it in the active session — the wrong repo.
      if ($repoStore.currentPath !== path) return;
      generation = result;
      repoStore.setCommitDraft(result.text);
    } catch (err: unknown) {
      aiError = formatError(err);
    } finally {
      isGenerating = false;
    }
  }

  async function finishCommit(outcome: { ok: boolean; error?: string; policy?: PolicyVerdict | null }) {
    lastVerdict = outcome.policy ?? null;
    if (!outcome.ok) {
      // A refused commit keeps its message: the user has to change the action,
      // not retype the description of it.
      commitError = outcome.error ?? "The commit did not run.";
      return;
    }
    repoStore.setCommitDraft("");
    repoStore.setAmending(false);
    generation = null;
  }

  async function handleCommit(forceQuick = false) {
    const message = commitMessage.trim();
    const useQuick = (forceQuick || includeUnstaged) && !isAmending;
    if (!message) return;
    if (conflictedCount > 0) return;
    if (useQuick ? dirtyCount === 0 : stagedFiles.length === 0 && !isAmending) return;
    commitError = null;
    if (useQuick) {
      await finishCommit(await repoStore.quickCommit(message));
      return;
    }
    await finishCommit(await repoStore.commit(message, isAmending));
  }

  function onMessageKeydown(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if (!(event.metaKey || event.ctrlKey) || event.key !== "Enter") return;
    event.preventDefault();
    if (event.shiftKey && !isAmending) {
      includeUnstaged = true;
      void handleCommit(true);
      return;
    }
    void handleCommit();
  }

  /** "1.4s", "820ms" — a local model's latency is worth showing plainly. */
  function duration(ms: number): string {
    return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${ms}ms`;
  }
</script>

<div class="p-3 border-t border-border/60 gp-section-edge bg-surface flex flex-col gap-2 shrink-0">
  <div class="flex items-center justify-between">
    <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Commit</span>
    <button
      onclick={generateMessage}
      disabled={isGenerating || stagedFiles.length === 0 || !aiReady}
      title={aiReady
        ? "Write a message for the staged diff with the local model"
        : ($harnessStore.ai?.detail ?? "No local model server is running")}
      class="gp-chip bg-accent/15 text-accent border-accent/40 hover:bg-accent/25 disabled:opacity-40 disabled:cursor-not-allowed transition-colors"
    >
      {#if isGenerating}
        <Loader size={12} class="animate-spin" />
        <span>Writing…</span>
      {:else}
        <Sparkles size={12} />
        <span>Generate</span>
      {/if}
    </button>
  </div>

  {#if stagedFiles.length > 0}
    <!-- A1: what this commit breaks — every PreviewReport honesty field. -->
    <div
      class="flex flex-col gap-1 rounded-lg border border-border/60 bg-background/60 p-2"
      data-testid="commit-preview-breaks"
    >
      <div class="flex items-center justify-between gap-2">
        <span class="text-[10px] font-bold uppercase tracking-wider text-textMuted"
          >What this commit breaks</span
        >
        {#if $previewStore.loading}
          <Loader size={11} class="animate-spin text-textMuted" />
        {/if}
      </div>
      {#if $previewSummary}
        <p
          class="text-[11px] leading-snug {$previewSummary.unreliableFiles > 0 ||
          $previewSummary.unavailableFiles > 0 ||
          $previewSummary.brokenCallerTotal > 0
            ? 'text-amber-500'
            : 'text-textSecondary'}"
        >
          {$previewSummary.headline}
        </p>
        {#if $previewSummary.outcomeReason}
          <p class="text-[10px] text-textMuted">{$previewSummary.outcomeReason}</p>
        {/if}
        <ul class="flex max-h-28 flex-col gap-1 overflow-y-auto gp-scroll">
          {#each $previewSummary.files as file (file.file_path)}
            <li class="rounded border border-border/40 px-1.5 py-1 font-mono text-[9px] leading-relaxed text-textMuted">
              <div class="truncate text-textSecondary" title={file.file_path}>{file.file_path}</div>
              <div>
                parse={file.parse_status}
                · against={file.compared_against}
                · indexed={file.file_is_indexed ? "yes" : "no"}
                · delta={file.delta_available ? "yes" : "no"}
              </div>
              {#if file.degraded_reason}
                <div class="text-amber-500">degraded: {file.degraded_reason}</div>
              {/if}
              <div>
                bodies_not_compared={file.bodies_not_compared}
                · ambiguous_callers={file.ambiguous_callers}
                · broken={file.broken_shown}/{file.broken_total}{file.broken_truncated
                  ? " (truncated)"
                  : ""}
              </div>
              {#if file.walk_incomplete}
                {@const walk = tooltipWalkIncomplete([file.walk_incomplete])}
                <div class="line-clamp-2 text-amber-500" title={walk}>
                  walk incomplete: {walk}
                </div>
              {/if}
              {#if !file.available}
                <div class="text-amber-500">unavailable: {file.reason}</div>
              {:else if file.unreliable}
                <div class="text-amber-500">unreliable preview — not "nothing breaks"</div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <BlastRadiusPanel {blast} loading={blastLoading} title="Staged blast radius" />
  {/if}

  <textarea
    value={commitMessage}
    oninput={(e) => repoStore.setCommitDraft((e.currentTarget as HTMLTextAreaElement).value)}
    onkeydown={onMessageKeydown}
    placeholder="Commit message (e.g. feat: add auth)..."
    rows="3"
    class="w-full bg-background border border-border/80 rounded-xl p-2.5 text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-hidden focus:border-accent/60 resize-none font-mono transition-colors"
  ></textarea>

  {#if generation}
    <!-- Provenance for the suggestion: which model wrote it, against how much
         context, from how much of the diff, and how long it took. -->
    <div class="text-[10px] text-textMuted leading-relaxed">
      <span class="font-mono">{generation.model}</span>
      · {generation.context_source}
      {#if generation.prompt_tokens > 0}
        · {generation.prompt_tokens} prompt tokens
      {/if}
      · {duration(generation.elapsed_ms)}
      {#if !generation.budget.planned_by_harness}
        · <span class="text-amber-400">budget estimated locally</span>
      {/if}
    </div>
    {#each generation.warnings as warning}
      <div class="text-[10px] text-amber-400 flex items-start gap-1">
        <AlertTriangle size={11} class="mt-px shrink-0" />
        <span>{warning}</span>
      </div>
    {/each}
  {/if}

  {#if aiError}
    <div class="text-[10px] text-rose-400 flex items-start gap-1">
      <AlertTriangle size={11} class="mt-px shrink-0" />
      <span>{aiError}</span>
    </div>
  {/if}

  {#if commitError}
    <div class="text-[10px] text-rose-400 whitespace-pre-wrap font-mono leading-relaxed">{commitError}</div>
  {:else if lastVerdict}
    <div
      class="text-[10px] flex flex-col gap-0.5 {lastVerdict.status === 'unchecked'
        ? 'text-amber-400'
        : 'text-textMuted'}"
    >
      <div class="flex items-start gap-1">
        {#if lastVerdict.checked}
          <ShieldCheck size={11} class="mt-px shrink-0" />
        {:else}
          <ShieldAlert size={11} class="mt-px shrink-0" />
        {/if}
        <span>{verdictLabel(lastVerdict)}</span>
      </div>
      <MarkdownBody
        source={verdictDetail(lastVerdict)}
        class="text-[10px] text-textMuted pl-4"
      />
    </div>
  {/if}

  <div class="flex items-center justify-between gap-2">
    <div class="flex items-center gap-3 min-w-0">
      <label class="flex items-center gap-1.5 text-[11px] text-textMuted cursor-pointer">
        <input
          type="checkbox"
          checked={isAmending}
          onchange={(e) => repoStore.setAmending((e.currentTarget as HTMLInputElement).checked)}
          class="rounded accent-accent"
        />
        <span>Amend</span>
      </label>
      <label
        class="flex items-center gap-1.5 text-[11px] text-textMuted cursor-pointer {isAmending
          ? 'opacity-40 cursor-not-allowed'
          : ''}"
        title="Stage remaining files and commit them together (quick commit)"
      >
        <input
          type="checkbox"
          checked={includeUnstaged}
          disabled={isAmending}
          aria-label="Include unstaged files in this commit"
          onchange={(e) => (includeUnstaged = (e.currentTarget as HTMLInputElement).checked)}
          class="rounded accent-accent"
        />
        <span>Include unstaged</span>
      </label>
    </div>
    <button
      onclick={() => void handleCommit()}
      disabled={commitDisabled}
      title={quickCommit
        ? "Stage all changes and commit (Cmd/Ctrl+Shift+Enter)"
        : "Commit staged files (Cmd/Ctrl+Enter)"}
      class="gp-btn-primary"
    >
      <Send size={12} />
      <span>{quickCommit ? "Commit all" : "Commit"} ({commitCount})</span>
    </button>
  </div>
</div>
