<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { repoStore } from "../stores/repoStore";
  import { graphStore, normalizeDiffPayload } from "../stores/graphStore";
  import { harnessStore, type AiGeneration } from "../stores/harnessStore";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import {
    GitCommit,
    User,
    Calendar,
    FileCode,
    Plus,
    Minus,
    ShieldCheck,
    ShieldAlert,
    Sparkles,
    Loader,
    AlertTriangle,
    Copy,
    Check,
  } from "lucide-svelte";
  import EmptyState from "./EmptyState.svelte";
  import { formatDate, shortHash } from "../format";
  import { formatError } from "../ui/formatError";
  import { copyText } from "../desktop/clipboard";
  import { toastStore } from "../stores/toastStore";
  // The canonical wire types, not local copies: both were declared here as
  // well, and the CommitDetails one had silently fallen four fields behind.
  // `import type` is erased, so this pulls in no store at runtime.
  import type { CommitDetails as CommitDetailsPayload } from "../stores/graphStore";
  import FreshnessBadge from "./FreshnessBadge.svelte";
  import type { ProvenanceFreshness } from "../provenance/types";


  let selectedCommit = $derived(
    $graphStore.rows.find((r) => r.id === $repoStore.selectedCommitId) || $graphStore.selectedCommit
  );

  let details = $state<CommitDetailsPayload | null>(null);

  // Agent refactors produce commits with tens of thousands of files; the
  // list renders a window of them and reveals more on demand instead of
  // mounting the whole set at once.
  const FILE_LIST_STEP = 200;
  let fileListLimit = $state(FILE_LIST_STEP);
  let fileList = $derived(details?.changed_files.slice(0, fileListLimit) ?? []);
  let hiddenFileCount = $derived((details?.changed_files.length ?? 0) - fileList.length);
  $effect(() => {
    void currentCommitId;
    fileListLimit = FILE_LIST_STEP;
  });
  // When the backend capped the list, the header reports the true file count
  // instead of the size of the capped slice.
  let filesListTruncated = $derived(details?.files_list_truncated === true);
  let changedFileCount = $derived(
    filesListTruncated && typeof details?.files_total_count === "number"
      ? details.files_total_count
      : details?.changed_files.length ?? 0
  );

  // Same story for the inline preview: a capped slice keeps the pane
  // responsive, and the full diff opens in the virtualized Diff view. The
  // normalizer keeps a legacy string payload and a new payload object alike
  // from reaching `.split` unvalidated.
  const PREVIEW_LINE_CAP = 2_000;
  let selectedDiffText = $derived(normalizeDiffPayload($repoStore.selectedDiff).content);
  let previewLines = $derived.by(() => {
    if (!selectedDiffText) return [];
    return selectedDiffText.split("\n").slice(0, PREVIEW_LINE_CAP);
  });
  let previewTruncated = $derived(selectedDiffText.split("\n").length > PREVIEW_LINE_CAP);

  // Explain-this-commit, answered by a model on this machine.
  let explanation = $state<AiGeneration | null>(null);
  let explanationFor = $state<string | null>(null);
  let isExplaining = $state(false);
  let explainError = $state<string | null>(null);

  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let currentCommitId = $derived($repoStore.selectedCommitId || $graphStore.selectedCommit?.id || null);
  let explainGuard: AsyncGuard | null = null;
  const explainTarget = { repo: null as string | null, id: null as string | null };

  let copiedSha = $state(false);
  let copiedMessage = $state(false);

  async function copySha(id: string) {
    await copyText(id);
    copiedSha = true;
    toastStore.info(`Copied full SHA: ${id.slice(0, 8)}`, undefined, 2000);
    setTimeout(() => (copiedSha = false), 1500);
  }

  async function copyMessage(msg: string) {
    await copyText(msg);
    copiedMessage = true;
    toastStore.info("Copied commit message", undefined, 2000);
    setTimeout(() => (copiedMessage = false), 1500);
  }

  async function explainCommit() {
    const path = $repoStore.currentPath;
    const id = currentCommitId;
    if (!path || !id) return;
    explainGuard?.cancel();
    const guard = createAsyncGuard();
    explainGuard = guard;
    isExplaining = true;
    explainError = null;
    explanation = null;
    try {
      const result = await harnessStore.explainCommit(path, id);
      if (!guard.isLive()) return;
      if (currentCommitId !== id || $repoStore.currentPath !== path) return;
      explanation = result;
      explanationFor = id;
    } catch (err: unknown) {
      if (!guard.isLive()) return;
      if (currentCommitId !== id || $repoStore.currentPath !== path) return;
      explainError = formatError(err);
    } finally {
      if (guard.isLive()) isExplaining = false;
    }
  }

  $effect(() => {
    return () => explainGuard?.cancel();
  });

  $effect(() => {
    const id = currentCommitId;
    const repo = $repoStore.currentPath;
    if (explainTarget.repo === repo && explainTarget.id === id) return;
    explainTarget.repo = repo;
    explainTarget.id = id;
    explainGuard?.cancel();
    isExplaining = false;
  });

  $effect(() => {
    const id = $repoStore.selectedCommitId || $graphStore.selectedCommit?.id;
    if (!id) {
      details = null;
      return;
    }
    // The graph store owns detail fetching (selectCommit and the auto-select
    // on load both populate it). Fetching here as well doubled every backend
    // traversal per click; this pane just mirrors the store, showing blank
    // until the one in-flight fetch lands.
    const cached = $graphStore.selectedCommitDetails;
    details = cached?.id === id ? cached : null;
  });

  /**
   * Git-native provenance for the selected commit.
   *
   * The single-commit measurement rather than the batch the branch and PR
   * lists use: those badge many rows and skip measuring commits that carry no
   * note, because a commit nobody verified has no verification whose age could
   * decay. Here one commit is in front of the reader and "how far has the base
   * moved since this" is a fair question whether or not anything was recorded.
   */
  let provenance = $state<ProvenanceFreshness | null>(null);
  /** Monotonic: clicking through history faster than the fetch returns must
   *  never leave an earlier commit's provenance on a later one. */
  let provenanceToken = 0;

  $effect(() => {
    const repo = $repoStore.currentPath;
    const id = $repoStore.selectedCommitId || $graphStore.selectedCommit?.id;
    const mine = ++provenanceToken;
    provenance = null;
    if (!repo || !id) return;
    invoke<ProvenanceFreshness>("cmd_provenance_freshness", {
      repoPath: repo,
      commitSha: id,
      baseBranch: null,
    })
      .then((f) => {
        if (mine === provenanceToken) provenance = f;
      })
      .catch(() => {
        // Left null, which draws nothing. The badge is an addition to this
        // pane, not a claim about it: a failed lookup must not put an error
        // where a verification would go.
      });
  });

  function gpgLabel(status: string): { text: string; ok: boolean } {
    if (status === "G") return { text: "Verified", ok: true };
    if (status === "N" || !status) return { text: "Unsigned", ok: false };
    return { text: "Unverified", ok: false };
  }
</script>

{#if selectedCommit}
  {@const sig = gpgLabel(details?.gpg_status || "")}
  <div class="h-64 border-t border-border/60 bg-surface flex flex-col font-sans select-none overflow-hidden">
    <div class="px-4 py-2.5 border-b border-border/60 flex items-center justify-between bg-surfaceHover/30">
      <div class="flex items-center gap-3 min-w-0">
        <GitCommit size={16} class="text-accent shrink-0" />
        <button
          type="button"
          onclick={() => copySha(selectedCommit.id)}
          class="font-mono text-xs font-semibold text-accent hover:underline flex items-center gap-1 group/sha"
          title="Click to copy full commit SHA ({selectedCommit.id})"
        >
          <span>{shortHash(selectedCommit.id, 8)}</span>
          {#if copiedSha}
            <Check size={11} class="text-emerald-500" />
          {:else}
            <Copy size={10} class="text-textMuted/60 opacity-0 group-hover/sha:opacity-100 transition-opacity" />
          {/if}
        </button>

        <div class="flex items-center gap-1.5 min-w-0 group/msg">
          <span class="text-xs font-medium text-textPrimary truncate select-text">{selectedCommit.summary}</span>
          <button
            type="button"
            onclick={() => copyMessage(selectedCommit.summary)}
            class="opacity-0 group-hover/msg:opacity-100 text-textMuted hover:text-textPrimary transition-opacity p-0.5"
            title="Copy commit message"
          >
            {#if copiedMessage}
              <Check size={11} class="text-emerald-500" />
            {:else}
              <Copy size={11} />
            {/if}
          </button>
        </div>
      </div>
      <div class="flex items-center gap-4 text-[11px] text-textMuted shrink-0">
        <span class="flex items-center gap-1.5 select-text"><User size={13} /> {selectedCommit.author_name}</span>
        <span class="flex items-center gap-1.5 select-text"><Calendar size={13} /> {details?.author_date || formatDate(selectedCommit.timestamp)}</span>
        <span class="flex items-center gap-1 {sig.ok ? 'text-green-400' : 'text-textMuted'}">
          {#if sig.ok}
            <ShieldCheck size={13} />
          {:else}
            <ShieldAlert size={13} />
          {/if}
          {sig.text}
        </span>
        <FreshnessBadge freshness={provenance} />
      </div>
    </div>

    <div class="flex-1 grid grid-cols-3 divide-x divide-border min-h-0">
      <div class="p-2 overflow-y-auto space-y-1">
        <div class="text-[10px] font-semibold uppercase text-textMuted px-2 py-1">
          Changed Files ({changedFileCount})
          {#if details}
            <span class="normal-case font-mono text-green-400 ml-1">+{details.total_additions}</span>
            <span class="normal-case font-mono text-red-400">-{details.total_deletions}</span>
          {/if}
        </div>
        {#if details?.body}
          <div class="px-2 py-1 text-[11px] text-textMuted whitespace-pre-wrap select-text">{details.body}</div>
        {/if}
        {#if details && details.co_authors.length > 0}
          <div class="px-2 py-1 text-[11px] text-textMuted select-text">Co-authors: {details.co_authors.join(", ")}</div>
        {/if}

        <!-- Local explanation of the commit. Nothing leaves the machine: the
             diff goes to a model server on loopback and the answer comes back. -->
        <div class="px-2 py-1.5">
          <button
            onclick={explainCommit}
            disabled={isExplaining || !aiReady || !currentCommitId}
            title={aiReady
              ? "Explain this commit with the local model"
              : ($harnessStore.ai?.detail ?? "No local model server is running")}
            class="gp-chip bg-accent/15 text-accent border-accent/40 hover:bg-accent/25 disabled:opacity-40 disabled:cursor-not-allowed transition-colors !py-1"
          >
            {#if isExplaining}
              <Loader size={12} class="animate-spin" />
              <span>Reading the diff…</span>
            {:else}
              <Sparkles size={12} />
              <span>Explain this commit</span>
            {/if}
          </button>

          {#if explainError}
            <div class="mt-1.5 text-[11px] text-rose-400">{explainError}</div>
          {/if}

          {#if explanation && explanationFor === currentCommitId}
            <div class="mt-2 rounded-xl border border-border/70 bg-background/60 p-3 space-y-1.5 shadow-card">
              <div class="text-[11px] text-textPrimary whitespace-pre-wrap leading-relaxed">
                {explanation.text}
              </div>
              <div class="text-[10px] text-textMuted">
                <span class="font-mono">{explanation.model}</span> · {explanation.context_source}
              </div>
              {#each explanation.warnings as warning}
                <div class="text-[10px] text-amber-400 flex items-start gap-1">
                  <AlertTriangle size={11} class="mt-px shrink-0" />
                  <span>{warning}</span>
                </div>
              {/each}
            </div>
          {/if}
        </div>
        {#if filesListTruncated}
          <div class="px-2 pb-1 text-[10px] text-textMuted">
            +only first {(details?.changed_files.length ?? 0).toLocaleString()} listed
          </div>
        {/if}
        {#each fileList as f (f.path)}
          <button
            class="w-full px-2 py-1.5 rounded-full hover:bg-surfaceHover flex items-center justify-between text-xs text-left transition-colors"
            onclick={() => {
              if (currentCommitId) repoStore.selectCommitFileDiff(currentCommitId, f.path);
            }}
          >
            <div class="flex items-center gap-2 truncate">
              <FileCode size={13} class="text-textMuted" />
              <span class="truncate text-textPrimary">{f.path}</span>
            </div>
            <div class="flex items-center gap-1.5 text-[10px] font-mono shrink-0">
              {#if f.additions > 0}
                <span class="text-green-400 flex items-center"><Plus size={10} />{f.additions}</span>
              {/if}
              {#if f.deletions > 0}
                <span class="text-red-400 flex items-center"><Minus size={10} />{f.deletions}</span>
              {/if}
            </div>
          </button>
        {/each}
        {#if hiddenFileCount > 0}
          <button
            class="w-full px-2 py-1.5 rounded-xl border border-dashed border-border/80 text-[11px] text-textMuted hover:text-textPrimary hover:border-accent/50 transition-colors"
            onclick={() => (fileListLimit = details!.changed_files.length)}
          >
            Show {hiddenFileCount.toLocaleString()} more changed file{hiddenFileCount === 1 ? "" : "s"}
          </button>
        {/if}
      </div>

      <div class="col-span-2 p-3 font-mono text-xs overflow-auto bg-background/50 flex flex-col">
        {#if previewLines.length > 0}
          <pre class="whitespace-pre text-textPrimary text-[11px] leading-relaxed select-text">{previewLines.join("\n")}</pre>
          {#if previewTruncated}
            <button
              class="mt-2 self-start px-2.5 py-1 rounded-full border border-border/80 text-[11px] font-sans text-textMuted hover:text-textPrimary hover:border-accent/60 transition-colors"
              onclick={() => repoStore.setActiveTab("diff")}
            >
              Preview capped at {PREVIEW_LINE_CAP.toLocaleString()} lines — open the full diff view
            </button>
          {/if}
        {:else}
          <EmptyState icon={GitCommit} title="No commit selected" hint="Select a commit to load its diff." compact />
        {/if}
      </div>
    </div>
  </div>
{/if}
