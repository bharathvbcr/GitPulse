<script lang="ts">
  import { onDestroy } from "svelte";
  import {
    AlertTriangle,
    Bot,
    Check,
    ChevronDown,
    Code,
    Copy,
    LoaderCircle,
    Sparkles,
    Terminal,
    X,
  } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { copyText } from "../desktop/clipboard";
  import { popover, type PopoverOptions } from "../ui/popover";
  import { LAYERS } from "../ui/layers";
  import { formatError } from "../ui/formatError";
  import {
    formatCommitAgentPrompt,
    getAgentCommitCliCommand,
    getAgentCommitSdkSnippet,
    launchAgentCommit,
    AGENT_COMMIT_CONFLICTS,
    AGENT_COMMIT_CLEAN,
    AGENT_COMMIT_NO_REPO,
  } from "../commit/agentCommit";
  import type { PromptLauncher } from "../terminal/launchRequests";

  let {
    compact = false,
    align = "left",
    direction = "down",
  }: {
    compact?: boolean;
    align?: "left" | "right";
    direction?: "up" | "down";
  } = $props();

  let open = $state(false);
  let launcher = $state<PromptLauncher>("agy");
  let scope = $state<"staged" | "all">("all");
  let userNote = $state("");
  let previewTab = $state<"prompt" | "cli" | "sdk">("prompt");
  let detailsOpen = $state(false);

  let launching = $state(false);
  let notice = $state<string | null>(null);
  let error = $state<string | null>(null);
  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let launchController: AbortController | undefined;
  let destroyed = false;

  const repoPath = $derived($repoStore.currentPath);
  const statuses = $derived($repoStore.statuses);
  const dirtyCount = $derived(statuses.length);
  const stagedCount = $derived(statuses.filter((s) => s.is_staged).length);
  const conflictedCount = $derived(statuses.filter((s) => s.is_conflicted).length);

  // Initialize scope based on staged files: if some files are staged, default to staged.
  $effect(() => {
    if (stagedCount > 0 && dirtyCount > stagedCount) {
      scope = "staged";
    } else {
      scope = "all";
    }
  });

  const activeFiles = $derived(
    scope === "staged"
      ? statuses.filter((s) => s.is_staged)
      : statuses,
  );

  const disabledReason = $derived(
    !repoPath
      ? AGENT_COMMIT_NO_REPO
      : conflictedCount > 0
        ? AGENT_COMMIT_CONFLICTS
        : dirtyCount === 0
          ? AGENT_COMMIT_CLEAN
          : null,
  );

  const promptText = $derived.by(() => {
    if (!repoPath) return "";
    try {
      return formatCommitAgentPrompt({
        repoPath,
        files: activeFiles.map((f) => ({
          path: f.path,
          status: f.status_code,
          isStaged: f.is_staged,
        })),
        onlyStaged: scope === "staged",
        instruction: userNote.trim() || undefined,
      });
    } catch (err: unknown) {
      return formatError(err);
    }
  });

  const cliCommand = $derived.by(() => {
    if (!promptText || !repoPath) return "";
    return getAgentCommitCliCommand(launcher, promptText, repoPath);
  });

  const sdkSnippet = $derived.by(() => {
    if (!promptText || !repoPath) return "";
    return getAgentCommitSdkSnippet(launcher, promptText, repoPath);
  });

  const dismissal: PopoverOptions = {
    dismiss: {
      inside: "[data-agent-commit-menu]",
      pointer: "pointerdown",
      escape: "capture",
      scroll: true,
      resize: true,
    },
    onDismiss: () => {
      open = false;
    },
  };

  onDestroy(() => {
    destroyed = true;
    clearTimeout(copyTimer);
    launchController?.abort();
  });

  async function handleLaunch(targetLauncher: PromptLauncher) {
    if (launching || !repoPath) return;
    if (conflictedCount > 0) {
      error = AGENT_COMMIT_CONFLICTS;
      return;
    }
    if (dirtyCount === 0) {
      error = AGENT_COMMIT_CLEAN;
      return;
    }

    launcher = targetLauncher;
    launching = true;
    error = null;
    notice = null;
    launchController = new AbortController();

    try {
      const res = await launchAgentCommit(
        {
          repoPath,
          launcher: targetLauncher,
          prompt: promptText,
          signal: launchController.signal,
        },
        (openState) => repoStore.setTerminalOpen(openState),
      );

      if (destroyed) return;
      if (res.ok) {
        notice = res.notice ?? "Agent session opened in the terminal.";
        // Auto-close menu on success after a short confirmation
        setTimeout(() => {
          if (!destroyed && notice) {
            open = false;
            notice = null;
          }
        }, 1800);
      } else {
        error = res.error ?? "Failed to launch agent session.";
      }
    } catch (err: unknown) {
      if (!destroyed) error = formatError(err);
    } finally {
      if (!destroyed) launching = false;
    }
  }

  async function handleCopy(text: string) {
    if (!text || copied) return;
    error = null;
    const ok = await copyText(text);
    if (destroyed) return;
    if (ok) {
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copied = false;
      }, 1800);
    } else {
      error = "Could not copy to clipboard.";
    }
  }
</script>

<div class="relative inline-flex items-center" data-agent-commit-menu>
  {#if compact}
    <button
      type="button"
      onclick={() => (open = !open)}
      aria-expanded={open}
      aria-haspopup="dialog"
      disabled={Boolean(disabledReason)}
      title={disabledReason ?? "Ask Antigravity or Claude to quickly commit these changes"}
      class="inline-flex items-center gap-1 rounded-full px-1.5 py-0.5 text-[10px] font-medium border border-border/80 bg-surface/70 text-textMuted hover:text-accent hover:border-accent/50 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
    >
      <Sparkles size={11} class="text-accent shrink-0" />
      <span class="font-sans">Ask Agent</span>
    </button>
  {:else}
    <button
      type="button"
      onclick={() => (open = !open)}
      aria-expanded={open}
      aria-haspopup="dialog"
      disabled={Boolean(disabledReason)}
      title={disabledReason ?? "Ask Antigravity or Claude to review changes and commit"}
      class="gp-btn py-0.5! px-2! text-[11px] inline-flex items-center gap-1.5 text-textMuted hover:text-textPrimary disabled:opacity-40 disabled:cursor-not-allowed"
    >
      <Sparkles size={12} class="text-accent shrink-0" />
      <span>Ask Agent to Commit</span>
      <ChevronDown size={11} class="opacity-70" />
    </button>
  {/if}

  {#if open}
    <div
      use:popover={dismissal}
      class="absolute {align === 'right' ? 'right-0' : 'left-0'} {direction === 'up'
        ? 'bottom-full mb-1.5'
        : 'top-full mt-1.5'} gp-menu gp-pop p-3 w-80 max-w-[92vw] flex flex-col gap-2.5 shadow-2xl rounded-xl border border-border/80 bg-surface font-sans"
      style="z-index: {LAYERS.MENU}"
      role="dialog"
      aria-label="Ask coding agent to commit"
      data-testid="agent-commit-menu"
    >
      <!-- Header -->
      <div class="flex items-center justify-between border-b border-border/60 pb-1.5">
        <div class="flex items-center gap-1.5">
          <Sparkles size={13} class="text-accent" />
          <span class="text-xs font-semibold text-textPrimary">Ask Agent to Commit</span>
        </div>
        <button
          type="button"
          onclick={() => (open = false)}
          class="gp-icon-btn p-1! text-textMuted hover:text-textPrimary"
          aria-label="Close"
        >
          <X size={12} />
        </button>
      </div>

      <!-- Scope selection (if there are staged files) -->
      {#if stagedCount > 0 && stagedCount < dirtyCount}
        <div class="flex flex-col gap-1">
          <span class="text-[10px] font-medium text-textMuted uppercase tracking-wide">Scope</span>
          <div class="grid grid-cols-2 gap-1 bg-surfaceHover/40 p-0.5 rounded-lg border border-border/50 text-[11px]">
            <button
              type="button"
              class="px-2 py-1 rounded text-center font-medium transition-colors {scope === 'staged'
                ? 'bg-accent/15 text-accent border border-accent/30'
                : 'text-textMuted hover:text-textPrimary'}"
              onclick={() => (scope = "staged")}
            >
              Staged ({stagedCount})
            </button>
            <button
              type="button"
              class="px-2 py-1 rounded text-center font-medium transition-colors {scope === 'all'
                ? 'bg-accent/15 text-accent border border-accent/30'
                : 'text-textMuted hover:text-textPrimary'}"
              onclick={() => (scope = "all")}
            >
              All changes ({dirtyCount})
            </button>
          </div>
        </div>
      {/if}

      <!-- Custom note / instruction -->
      <div class="flex flex-col gap-1">
        <label for="agent-commit-note" class="text-[10px] font-medium text-textMuted uppercase tracking-wide">
          Note (optional)
        </label>
        <input
          id="agent-commit-note"
          bind:value={userNote}
          type="text"
          placeholder="e.g. Focus on UI fix, follow conventional commit..."
          class="w-full bg-background border border-border/70 rounded-lg px-2.5 py-1 text-xs text-textPrimary placeholder:text-textMuted/60 focus:outline-hidden focus:border-accent/60 font-sans"
        />
      </div>

      <!-- Primary Action Buttons -->
      <div class="flex flex-col gap-1.5 pt-0.5">
        <button
          type="button"
          disabled={launching}
          onclick={() => void handleLaunch("agy")}
          class="w-full flex items-center justify-between px-3 py-1.5 rounded-xl border border-accent/40 bg-accent/10 hover:bg-accent/20 text-accent font-medium text-xs transition-colors disabled:opacity-50"
        >
          <div class="flex items-center gap-2">
            <Sparkles size={13} class="shrink-0" />
            <span>Ask Antigravity (agy)</span>
          </div>
          <Terminal size={12} class="opacity-70 shrink-0" />
        </button>

        <button
          type="button"
          disabled={launching}
          onclick={() => void handleLaunch("claude")}
          class="w-full flex items-center justify-between px-3 py-1.5 rounded-xl border border-border/80 bg-surfaceHover/50 hover:bg-surfaceHover text-textPrimary font-medium text-xs transition-colors disabled:opacity-50"
        >
          <div class="flex items-center gap-2">
            <Bot size={13} class="text-amber-500 shrink-0" />
            <span>Ask Claude Code</span>
          </div>
          <Terminal size={12} class="opacity-70 shrink-0" />
        </button>
      </div>

      <!-- Notice / Error banners -->
      {#if launching}
        <div class="flex items-center gap-2 rounded-lg bg-surfaceHover/60 px-2.5 py-1.5 text-[11px] text-textMuted">
          <LoaderCircle size={13} class="animate-spin text-accent shrink-0" />
          <span>Opening session in terminal dock…</span>
        </div>
      {/if}

      {#if notice}
        <div class="flex items-center gap-2 rounded-lg bg-emerald-500/10 border border-emerald-500/25 px-2.5 py-1.5 text-[11px] text-emerald-400">
          <Check size={13} class="shrink-0" />
          <span>{notice}</span>
        </div>
      {/if}

      {#if error}
        <div class="flex items-center gap-2 rounded-lg bg-rose-500/10 border border-rose-500/25 px-2.5 py-1.5 text-[11px] text-rose-400">
          <AlertTriangle size={13} class="shrink-0" />
          <span class="break-words">{error}</span>
        </div>
      {/if}

      <!-- Expandable Developer CLI / SDK commands -->
      <div class="border-t border-border/60 pt-1.5">
        <button
          type="button"
          onclick={() => (detailsOpen = !detailsOpen)}
          class="flex items-center justify-between w-full text-[10px] text-textMuted hover:text-textPrimary transition-colors py-0.5"
        >
          <span class="flex items-center gap-1">
            <Code size={11} />
            <span>CLI & Developer SDK</span>
          </span>
          <ChevronDown size={11} class="transition-transform {detailsOpen ? 'rotate-180' : ''}" />
        </button>

        {#if detailsOpen}
          <div class="flex flex-col gap-1.5 mt-1.5">
            <!-- Tabs -->
            <div class="flex border-b border-border/50 text-[10px]">
              <button
                type="button"
                class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'prompt'
                  ? 'border-accent text-accent'
                  : 'border-transparent text-textMuted hover:text-textPrimary'}"
                onclick={() => (previewTab = 'prompt')}
              >
                Prompt
              </button>
              <button
                type="button"
                class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'cli'
                  ? 'border-accent text-accent'
                  : 'border-transparent text-textMuted hover:text-textPrimary'}"
                onclick={() => (previewTab = 'cli')}
              >
                CLI Command
              </button>
              <button
                type="button"
                class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'sdk'
                  ? 'border-accent text-accent'
                  : 'border-transparent text-textMuted hover:text-textPrimary'}"
                onclick={() => (previewTab = 'sdk')}
              >
                Python SDK
              </button>

              <button
                type="button"
                onclick={() =>
                  void handleCopy(
                    previewTab === 'prompt'
                      ? promptText
                      : previewTab === 'cli'
                        ? cliCommand
                        : sdkSnippet,
                  )}
                class="ml-auto inline-flex items-center gap-1 text-[10px] text-textMuted hover:text-accent font-mono py-0.5"
                title="Copy current content"
              >
                {#if copied}
                  <Check size={10} class="text-emerald-400" />
                  <span class="text-emerald-400">Copied</span>
                {:else}
                  <Copy size={10} />
                  <span>Copy</span>
                {/if}
              </button>
            </div>

            <!-- Content preview -->
            <pre class="w-full max-h-36 overflow-y-auto p-2 rounded-lg bg-background border border-border/60 font-mono text-[9px] text-textSecondary leading-relaxed whitespace-pre-wrap select-all gp-scroll">{previewTab === 'prompt'
                ? promptText
                : previewTab === 'cli'
                  ? cliCommand
                  : sdkSnippet}</pre>
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>
