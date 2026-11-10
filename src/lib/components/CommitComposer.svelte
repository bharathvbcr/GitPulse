<script lang="ts">
  import { tick, untrack } from "svelte";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import {
    harnessStore,
    verdictDetail,
    verdictLabel,
    type AiGeneration,
    type PolicyVerdict,
  } from "../stores/harnessStore";
  import { Send, Sparkles, AlertTriangle, ShieldCheck, ShieldAlert, Loader } from "@lucide/svelte";
  import AgentCommitMenu from "./AgentCommitMenu.svelte";
  import Checkbox from "./Checkbox.svelte";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import HookRunNotice from "./HookRunNotice.svelte";
  import IdentityPrompt from "./IdentityPrompt.svelte";
  import { ipcErrorKind, type IpcErrorKind } from "../ipc/invoke";
  import MarkdownBody from "./MarkdownBody.svelte";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { repoScope } from "../repos/repoScope.svelte";
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
  // The rail already derives its per-file verdict from these; the composer
  // used to re-render the raw report beside it, which is how the two came to
  // describe the same preview differently.
  import { fileGlance, markerForHonesty } from "../codeintel/previewSummary";
  import { formatPathParts } from "../files/formatPath";
  import { capFanout, omittedLayeredImpact } from "../codeintel/fanout";
  import {
    appleIntelligenceStatus,
    appleReady,
    type AppleIntelligenceStatus,
  } from "../ai/appleIntelligence";

  let stagedFiles = $derived($repoStore.statuses.filter((s) => s.is_staged));
  let dirtyCount = $derived($repoStore.statuses.length);
  let conflictedCount = $derived($repoStore.statuses.filter((s) => s.is_conflicted).length);
  let aiReady = $derived($harnessStore.ai?.ready ?? false);
  let apple = $state<AppleIntelligenceStatus | null>(null);
  $effect(() => {
    let live = true;
    void appleIntelligenceStatus().then((status) => {
      if (live) apple = status;
    });
    return () => {
      live = false;
    };
  });
  // Its own store: a keystroke must not republish the repository state.
  const commitDraft = repoStore.commitDraft;
  let commitMessage = $derived($commitDraft);
  let isAmending = $derived($repoStore.isAmending);
  let includeUnstaged = $state(false);
  let isGenerating = $state(false);
  let generation = $state<AiGeneration | null>(null);
  let aiError = $state<string | null>(null);
  let messageEl = $state<HTMLTextAreaElement | null>(null);
  /** Per instance: a sidebar can keep more than one composer mounted, and ids must not collide. */
  const instanceId = $props.id();
  const keysId = `${instanceId}-commit-message-keys`;
  let commitError = $state<{ message: string; kind: IpcErrorKind } | null>(null);
  let isCommitting = $state(false);
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

  // The effects below key on the shared repository path and the staged-path
  // key, both compared by value, so a repoStore publish that changed neither
  // does not re-issue the preview batch or the layered walk.
  $effect(() => {
    const repo = $currentRepoPath;
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
    const repo = $currentRepoPath;
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

  /**
   * Mounted once per window, so a tab switch re-points the store (the draft and
   * the amend flag are session fields, and follow it) and nothing else. What is
   * local is the previous repository's last generation, error, verdict, the
   * "include unstaged" choice, an in-flight commit or generation and the blast
   * radius over its staged set: all of it described a commit that is not the
   * one on screen now. `apple` is a property of the host, not of a repository.
   */
  const tabScope = repoScope(
    () => $currentRepoPath,
    () => {
      includeUnstaged = false;
      isGenerating = false;
      generation = null;
      aiError = null;
      commitError = null;
      isCommitting = false;
      lastVerdict = null;
      // Cancelled once, here, and let go: the blast effect cancels whatever
      // guard it finds when it re-runs for the new repository, and a guard
      // that is still held would be cancelled (and its query cancelled over
      // IPC) a second time.
      blastGuard?.cancel();
      blastGuard = null;
      blast = null;
      blastLoading = false;
    },
  );

  async function generateMessage() {
    const path = $repoStore.currentPath;
    if (!path || isGenerating) return;
    const epoch = tabScope.epoch;
    isGenerating = true;
    aiError = null;
    generation = null;
    try {
      const result = await harnessStore.generateCommitMessage(path);
      // The user may have switched repositories while generation ran; writing
      // the draft now would land it in the active session — the wrong repo.
      // The epoch also catches A -> B -> A, which a path comparison cannot.
      if (!tabScope.isCurrent(epoch)) return;
      generation = result;
      commitDraft.set(result.text);
    } catch (err: unknown) {
      if (!tabScope.isCurrent(epoch)) return;
      const failure = describeFailure({ verb: "write", object: "a commit message" }, err);
      aiError = failure.detail ? `${failure.headline} ${failure.detail}` : failure.headline;
    } finally {
      if (tabScope.isCurrent(epoch)) isGenerating = false;
    }
  }

  async function finishCommit(
    outcome: { ok: boolean; error?: string; cause?: unknown; policy?: PolicyVerdict | null },
    epoch: number,
  ) {
    // The commit ran against the repository it was started in. When the person
    // has since switched, its verdict and error describe a commit that is not
    // on screen, and clearing the draft would clear the other repository's.
    if (!tabScope.isCurrent(epoch)) return;
    lastVerdict = outcome.policy ?? null;
    if (!outcome.ok) {
      // A refused commit keeps its message: the user has to change the action,
      // not retype the description of it.
      commitError = { message: outcome.error ?? "The commit did not run.", kind: ipcErrorKind(outcome.cause) };
      return;
    }
    commitDraft.set("");
    repoStore.setAmending(false);
    generation = null;
  }

  async function handleCommit(forceQuick = false) {
    const message = commitMessage.trim();
    const useQuick = (forceQuick || includeUnstaged) && !isAmending;
    if (!message) return;
    if (conflictedCount > 0) return;
    if (useQuick ? dirtyCount === 0 : stagedFiles.length === 0 && !isAmending) return;
    const epoch = tabScope.epoch;
    commitError = null;
    isCommitting = true;
    try {
      await finishCommit(
        useQuick ? await repoStore.quickCommit(message) : await repoStore.commit(message, isAmending),
        epoch,
      );
    } finally {
      if (tabScope.isCurrent(epoch)) isCommitting = false;
    }
  }

  /** The identity form unmounts under the focused control; the message box is where the next move is. */
  async function focusMessage() {
    await tick();
    messageEl?.focus({ preventScroll: true });
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

<!-- Full width and flush to the sidebar's foot: rounded top corners and the
     surface step mark it off the list above (no rule), and the sidebar's own
     rounded edge clips its bottom. -->
<div class="rounded-t-xl p-3 bg-surface flex flex-col gap-2 shrink-0">
  <div class="flex items-center justify-between">
    <span class="gp-label">Commit</span>
    <button
      type="button"
      onclick={generateMessage}
      disabled={isGenerating || stagedFiles.length === 0}
      title={stagedFiles.length === 0
        ? "Stage a change to describe"
        : aiReady
          ? "Write a message for the staged diff with the local model"
          : appleReady(apple)
            ? "Write the message on-device. The patch is classified in the app; Apple Intelligence only phrases the subject."
            : "Write a message from the staged diff. No model is running, so GitPulse assembles it from the change."}
      class="gp-btn"
      data-size="sm"
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
      class="flex flex-col gap-1 rounded-lg bg-background p-2"
      data-testid="commit-preview-breaks"
    >
      <div class="flex items-center justify-between gap-2">
        <span class="gp-label">What this commit breaks</span>
        {#if $previewStore.loading}
          <span class="text-micro text-textMuted" role="status">Checking…</span>
        {/if}
      </div>
      {#if $previewSummary}
        <p
          class="text-meta leading-snug {$previewSummary.unreliableFiles > 0 ||
          $previewSummary.unavailableFiles > 0 ||
          $previewSummary.brokenCallerTotal > 0
            ? 'text-warn'
            : 'text-textSecondary'}"
        >
          {$previewSummary.headline}
        </p>
        {#if $previewSummary.outcomeReason}
          <p class="text-micro text-textMuted">{$previewSummary.outcomeReason}</p>
        {/if}
        <!-- One row per file: the filename, and the first thing the preview
             actually found. The per-field telemetry that used to fill six
             monospace lines per file is still reachable — it is the row's
             tooltip and the disclosure below — but it no longer outweighs the
             finding it was supposed to support. -->
        <ul class="flex max-h-32 flex-col gap-px overflow-y-auto gp-scroll">
          {#each $previewSummary.files as file (file.file_path)}
            {@const marker = markerForHonesty(file)}
            <li
              class="flex items-baseline gap-1.5 rounded px-1 py-0.5 text-micro leading-snug hover:bg-surfaceHover/50"
              title={marker.title}
            >
              <span
                class="mt-px size-1.5 shrink-0 rounded-full {marker.kind === 'breaks'
                  ? 'bg-danger'
                  : marker.kind === 'clean'
                    ? 'bg-ok'
                    : 'bg-warn'}"
                aria-hidden="true"
              ></span>
              <span class="min-w-0 shrink truncate font-mono text-textSecondary">
                {formatPathParts(file.file_path).name}
              </span>
              <span
                class="min-w-0 flex-1 truncate text-right {marker.kind === 'clean'
                  ? 'text-textMuted'
                  : marker.kind === 'breaks'
                    ? 'text-danger'
                    : 'text-warn'}"
              >
                {fileGlance(file)}
              </span>
            </li>
          {/each}
        </ul>

        <!-- The raw report, for the reader who needs the exact fields. Kept
             verbatim: compressing the summary is not licence to lose the
             evidence behind it. -->
        {#if $previewSummary.files.length > 0}
          <details class="mt-1">
            <summary class="cursor-pointer text-micro text-textMuted hover:text-textPrimary">
              Preview details
            </summary>
            <ul class="mt-1 flex max-h-40 flex-col gap-1 overflow-y-auto gp-scroll">
              {#each $previewSummary.files as file (file.file_path)}
                <li
                  class="rounded bg-surface px-1.5 py-1 font-mono text-micro leading-relaxed text-textMuted"
                >
                  <div class="truncate text-textSecondary" title={file.file_path}>
                    {file.file_path}
                  </div>
                  <div>
                    parse={file.parse_status}
                    · against={file.compared_against}
                    · indexed={file.file_is_indexed ? "yes" : "no"}
                    · delta={file.delta_available ? "yes" : "no"}
                  </div>
                  {#if file.degraded_reason}
                    <div class="text-warn">degraded: {file.degraded_reason}</div>
                  {/if}
                  <div>
                    bodies not compared={file.bodies_not_compared}
                    · ambiguous callers={file.ambiguous_callers}
                    · broken={file.broken_shown}/{file.broken_total}{file.broken_truncated
                      ? " (truncated)"
                      : ""}
                  </div>
                  {#if file.walk_incomplete}
                    {@const walk = tooltipWalkIncomplete([file.walk_incomplete])}
                    <div class="line-clamp-2 text-warn" title={walk}>
                      walk incomplete: {walk}
                    </div>
                  {/if}
                  {#if !file.available}
                    <div class="text-warn">unavailable: {file.reason}</div>
                  {:else if file.unreliable}
                    <div class="text-warn">unreliable preview — not "nothing breaks"</div>
                  {/if}
                </li>
              {/each}
            </ul>
          </details>
        {/if}
      {/if}
    </div>

    <BlastRadiusPanel
      {blast}
      loading={blastLoading}
      title={stagedFiles.length === 1
        ? "Blast radius · 1 staged file"
        : `Blast radius · all ${stagedFiles.length} staged files`}
    />
  {/if}

  <textarea
    bind:this={messageEl}
    value={commitMessage}
    oninput={(e) => commitDraft.set((e.currentTarget as HTMLTextAreaElement).value)}
    onkeydown={onMessageKeydown}
    aria-label="Commit message"
    aria-describedby={keysId}
    placeholder="Commit message (e.g. feat: add auth)…"
    rows="3"
    class="gp-field-multi w-full resize-none font-mono"
  ></textarea>
  <span id={keysId} class="sr-only">Cmd/Ctrl Enter commits staged files. Cmd/Ctrl Shift Enter stages everything and commits.</span>

  {#if generation}
    <!-- Provenance for the suggestion: which model wrote it, against how much
         context, from how much of the diff, and how long it took. -->
    <div class="text-micro text-textMuted leading-relaxed">
      <span class="font-mono">{generation.model}</span>
      · {generation.context_source}
      {#if generation.prompt_tokens > 0}
        · {generation.prompt_tokens} prompt tokens
      {/if}
      · {duration(generation.elapsed_ms)}
      {#if !generation.budget.planned_by_harness}
        · <span class="text-warn">budget estimated locally</span>
      {/if}
    </div>
    {#each generation.warnings as warning}
      <div class="text-micro text-warn flex items-start gap-1">
        <AlertTriangle size={11} class="mt-px shrink-0" />
        <span>{warning}</span>
      </div>
    {/each}
  {/if}

  {#if aiError}
    <div role="alert" class="text-micro text-danger flex items-start gap-1">
      <AlertTriangle size={11} class="mt-px shrink-0" />
      <span>{aiError}</span>
    </div>
  {/if}

  {#if isCommitting}
    <HookRunNotice />
  {/if}

  {#if commitError?.kind === "identity_missing"}
    <IdentityPrompt
      onSaved={() => { commitError = null; void handleCommit(); void focusMessage(); }}
      onDismiss={() => { commitError = null; void focusMessage(); }}
    />
  {:else if commitError}
    <div class="text-micro text-danger whitespace-pre-wrap font-mono leading-relaxed">{commitError.message}</div>
  {:else if lastVerdict}
    <div
      class="text-micro flex flex-col gap-0.5 {lastVerdict.status === 'unchecked'
        ? 'text-warn'
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
        class="text-micro text-textMuted pl-4"
      />
    </div>
  {/if}

  <!-- The two toggles keep their own text on one line and the ROW wraps when
       the sidebar is too narrow for all three controls. Without this, "Include
       unstaged" broke across two lines inside its own label and dragged the
       commit button's row height with it. -->
  <div class="flex flex-wrap items-center justify-between gap-x-2 gap-y-1.5">
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1.5 min-w-0">
      <span class="shrink-0 whitespace-nowrap">
        <Checkbox
          label="Amend"
          checked={isAmending}
          onchange={(next) => repoStore.setAmending(next)}
        />
      </span>
      <span
        class="shrink-0 whitespace-nowrap"
        title="Stage remaining files and commit them together (quick commit)"
      >
        <Checkbox
          label="Include unstaged"
          ariaLabel="Include unstaged files in this commit"
          checked={includeUnstaged}
          disabled={isAmending}
          onchange={(next) => (includeUnstaged = next)}
        />
      </span>
    </div>
    <div class="flex items-center gap-1.5 shrink-0">
      <AgentCommitMenu compact={true} align="right" direction="up" />
      <button
        type="button"
        onclick={() => void handleCommit()}
        disabled={commitDisabled}
        title={quickCommit
          ? `Stage all changes and commit (${shortcutTextLabel("⌘⇧↵", $hostPlatform.os)})`
          : `Commit staged files (${shortcutTextLabel("⌘↵", $hostPlatform.os)})`}
        class="gp-btn shrink-0 whitespace-nowrap"
        data-variant="primary"
      >
        <Send size={12} />
        <span>{quickCommit ? "Commit all" : "Commit"} ({commitCount})</span>
      </button>
    </div>
  </div>
</div>
