<script lang="ts" module>
  /**
   * At most one Ask Agent panel is open across every mount. Outside-pointer
   * dismissal cannot guarantee that alone: its listener exists only once a
   * panel has mounted, so two opens inside one task stacked two panels.
   */
  let closeOpenMenu: (() => void) | null = null;
</script>

<script lang="ts">
  import { onDestroy, tick } from "svelte";
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
  import { portal } from "../dom/portal";
  import { popover, restoreFocusTo, type DismissReason, type PopoverOptions } from "../ui/popover";
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
    /** Which trigger edge the panel lines up with. */
    align?: "left" | "right";
    /** Preferred side; the panel flips when that side cannot hold it. */
    direction?: "up" | "down";
  } = $props();

  /**
   * One id per mount. Four surfaces mount this menu at once (the sidebar
   * header, the collapsed rail, the commit composer and the diff toolbar), so
   * a shared selector would count another instance's trigger as "inside" and
   * a fixed `id` would point every label at the first note field.
   */
  const uid = $props.id();
  const panelId = `agent-commit-panel-${uid}`;
  const noteId = `agent-commit-note-${uid}`;

  /** How long the success confirmation stays before the panel closes itself. */
  const SUCCESS_CLOSE_MS = 1800;
  const COPIED_MS = 1800;

  let open = $state(false);
  let launcher = $state<PromptLauncher>("agy");
  /** The user's explicit pick; `null` until they make one. */
  let scopeChoice = $state<"staged" | "all" | null>(null);
  let userNote = $state("");
  let previewTab = $state<"prompt" | "cli" | "sdk">("prompt");
  let detailsOpen = $state(false);

  let launching = $state(false);
  let notice = $state<string | null>(null);
  let error = $state<string | null>(null);
  let copied = $state(false);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let closeTimer: ReturnType<typeof setTimeout> | undefined;
  let launchController: AbortController | undefined;
  let destroyed = false;

  let triggerEl = $state<HTMLButtonElement | null>(null);
  let noteEl = $state<HTMLInputElement | null>(null);

  const repoPath = $derived($repoStore.currentPath);
  const statuses = $derived($repoStore.statuses);
  const dirtyCount = $derived(statuses.length);
  const stagedCount = $derived(statuses.filter((s) => s.is_staged).length);
  const conflictedCount = $derived(statuses.filter((s) => s.is_conflicted).length);

  /**
   * Scope is a choice only while some — not all — changes are staged. The
   * pick is derived rather than written by an effect, so a background status
   * refresh cannot overwrite what the user selected; it only decides whether
   * the choice still exists.
   */
  const scopeSelectable = $derived(stagedCount > 0 && stagedCount < dirtyCount);
  const scope = $derived<"staged" | "all">(scopeSelectable ? (scopeChoice ?? "staged") : "all");

  const activeFiles = $derived(
    scope === "staged" ? statuses.filter((s) => s.is_staged) : statuses,
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

  /**
   * The prompt, or why it could not be built. Kept apart so a failure is
   * shown as an error and never handed to an agent as if it were the prompt.
   */
  const prompt = $derived.by((): { text: string; error: string | null } => {
    if (!repoPath) return { text: "", error: AGENT_COMMIT_NO_REPO };
    try {
      return {
        text: formatCommitAgentPrompt({
          repoPath,
          files: activeFiles.map((f) => ({
            path: f.path,
            status: f.status_code,
            isStaged: f.is_staged,
          })),
          onlyStaged: scope === "staged",
          instruction: userNote.trim() || undefined,
        }),
        error: null,
      };
    } catch (err: unknown) {
      return { text: "", error: formatError(err) };
    }
  });

  const cliCommand = $derived(
    prompt.text && repoPath ? getAgentCommitCliCommand(launcher, prompt.text, repoPath) : "",
  );
  const sdkSnippet = $derived(
    prompt.text && repoPath ? getAgentCommitSdkSnippet(launcher, prompt.text, repoPath) : "",
  );
  const previewText = $derived(
    previewTab === "prompt" ? prompt.text : previewTab === "cli" ? cliCommand : sdkSnippet,
  );

  /** Something stopping a launch right now, in the order the user must fix it. */
  const blockedReason = $derived(disabledReason ?? prompt.error);

  /**
   * Placement and dismissal. The panel is portaled to `<body>` and anchored
   * to its trigger, because every surface that mounts this menu clips its
   * overflow — the sidebar body scrolls, the collapsed rail is
   * `overflow-hidden` and only ~48px wide — and an `absolute` panel inside
   * them was cut off at their edges.
   *
   * `revision` names everything that changes the panel's height. An upward
   * panel is placed by its measured height, so a stale measurement would let
   * it grow down over its own trigger.
   */
  const dismissal = $derived<PopoverOptions>({
    anchor: {
      kind: "element",
      element: triggerEl,
      gap: 6,
      place: direction === "up" ? "above" : "below",
      align: align === "right" ? "end" : "start",
    },
    estimate: { width: 320, height: 280 },
    inset: 8,
    revision: [
      detailsOpen,
      previewTab,
      scopeSelectable,
      launching,
      notice,
      error,
      blockedReason,
    ].join("|"),
    dismiss: {
      inside: `[data-agent-commit-menu="${uid}"], [data-agent-commit-panel="${uid}"]`,
      pointer: "pointerdown",
      escape: "capture",
      scroll: true,
      resize: true,
    },
    onDismiss: (reason: DismissReason) => close({ restoreFocus: reason === "escape" }),
  });

  /** This mount's entry in the one-open-menu slot; compared by identity. */
  const closeThis = () => close();

  onDestroy(() => {
    destroyed = true;
    clearTimeout(copyTimer);
    clearTimeout(closeTimer);
    launchController?.abort();
    if (closeOpenMenu === closeThis) closeOpenMenu = null;
  });

  async function openMenu() {
    if (disabledReason) return;
    if (closeOpenMenu !== closeThis) closeOpenMenu?.();
    closeOpenMenu = closeThis;
    clearTimeout(closeTimer);
    // A notice or error belongs to the attempt that raised it, not to the
    // next time the panel opens.
    notice = null;
    error = null;
    open = true;
    await tick();
    if (open && !destroyed) noteEl?.focus({ preventScroll: true });
  }

  function close(options: { restoreFocus?: boolean } = {}) {
    clearTimeout(closeTimer);
    closeTimer = undefined;
    if (closeOpenMenu === closeThis) closeOpenMenu = null;
    open = false;
    notice = null;
    if (options.restoreFocus) restoreFocusTo(triggerEl);
  }

  function toggle() {
    if (open) close();
    else void openMenu();
  }

  async function handleLaunch(targetLauncher: PromptLauncher) {
    if (launching) return;
    // Captured now: the prompt is derived, and a status refresh during the
    // await must not change what this launch is reported as having sent.
    const repo = repoPath;
    const text = prompt.text;
    if (blockedReason || !repo || !text) {
      error = blockedReason ?? AGENT_COMMIT_NO_REPO;
      return;
    }

    launcher = targetLauncher;
    launching = true;
    error = null;
    notice = null;
    clearTimeout(closeTimer);
    launchController = new AbortController();

    try {
      const res = await launchAgentCommit(
        {
          repoPath: repo,
          launcher: targetLauncher,
          prompt: text,
          signal: launchController.signal,
        },
        (openState) => repoStore.setTerminalOpen(openState),
      );

      if (destroyed) return;
      if (res.ok) {
        notice = res.notice ?? "Agent session opened in the terminal.";
        if (open) closeTimer = setTimeout(() => close(), SUCCESS_CLOSE_MS);
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
    if (!text) return;
    error = null;
    const ok = await copyText(text);
    if (destroyed) return;
    if (ok) {
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => {
        copied = false;
      }, COPIED_MS);
    } else {
      error = "Could not copy to clipboard.";
    }
  }
</script>

<div class="relative inline-flex items-center" data-agent-commit-menu={uid}>
  {#if compact}
    <button
      bind:this={triggerEl}
      type="button"
      onclick={toggle}
      aria-expanded={open}
      aria-haspopup="dialog"
      aria-controls={open ? panelId : undefined}
      disabled={Boolean(disabledReason)}
      title={disabledReason ?? "Ask Antigravity or Claude to quickly commit these changes"}
      class="inline-flex items-center gap-1 rounded-full px-1.5 py-0.5 text-[10px] font-medium border border-border/80 bg-surface/70 text-textMuted hover:text-accent hover:border-accent/50 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
    >
      <Sparkles size={11} class="text-accent shrink-0" />
      <span class="font-sans">Ask Agent</span>
    </button>
  {:else}
    <button
      bind:this={triggerEl}
      type="button"
      onclick={toggle}
      aria-expanded={open}
      aria-haspopup="dialog"
      aria-controls={open ? panelId : undefined}
      disabled={Boolean(disabledReason)}
      title={disabledReason ?? "Ask Antigravity or Claude to review changes and commit"}
      class="gp-btn py-0.5! px-2! text-[11px] inline-flex items-center gap-1.5 text-textMuted hover:text-textPrimary disabled:opacity-40 disabled:cursor-not-allowed"
    >
      <Sparkles size={12} class="text-accent shrink-0" />
      <span>Ask Agent to Commit</span>
      <ChevronDown size={11} class="opacity-70" />
    </button>
  {/if}
</div>

{#if open}
  <!-- `use:portal` precedes `use:popover`: the popover must measure the node
       after it has left the clipping pane, not while it is still inside it. -->
  <div
    use:portal={"body"}
    use:popover={dismissal}
    id={panelId}
    data-agent-commit-panel={uid}
    class="fixed gp-menu gp-pop gp-scroll p-3 w-80 max-w-[calc(100vw-16px)] max-h-[calc(100vh-16px)] overflow-y-auto overscroll-contain flex flex-col gap-2.5 shadow-2xl rounded-xl border border-border/80 bg-surface font-sans"
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
        onclick={() => close({ restoreFocus: true })}
        class="gp-icon-btn p-1! text-textMuted hover:text-textPrimary"
        aria-label="Close"
      >
        <X size={12} />
      </button>
    </div>

    <!-- Scope selection (only while some, not all, changes are staged) -->
    {#if scopeSelectable}
      <div class="flex flex-col gap-1">
        <span class="text-[10px] font-medium text-textMuted uppercase tracking-wide">Scope</span>
        <div class="grid grid-cols-2 gap-1 bg-surfaceHover/40 p-0.5 rounded-lg border border-border/50 text-[11px]">
          <button
            type="button"
            aria-pressed={scope === "staged"}
            class="px-2 py-1 rounded text-center font-medium transition-colors {scope === 'staged'
              ? 'bg-accent/15 text-accent border border-accent/30'
              : 'text-textMuted hover:text-textPrimary'}"
            onclick={() => (scopeChoice = "staged")}
          >
            Staged ({stagedCount})
          </button>
          <button
            type="button"
            aria-pressed={scope === "all"}
            class="px-2 py-1 rounded text-center font-medium transition-colors {scope === 'all'
              ? 'bg-accent/15 text-accent border border-accent/30'
              : 'text-textMuted hover:text-textPrimary'}"
            onclick={() => (scopeChoice = "all")}
          >
            All changes ({dirtyCount})
          </button>
        </div>
      </div>
    {/if}

    <!-- Custom note / instruction -->
    <div class="flex flex-col gap-1">
      <label for={noteId} class="text-[10px] font-medium text-textMuted uppercase tracking-wide">
        Note (optional)
      </label>
      <input
        bind:this={noteEl}
        id={noteId}
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
        disabled={launching || Boolean(blockedReason)}
        title={blockedReason ?? undefined}
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
        disabled={launching || Boolean(blockedReason)}
        title={blockedReason ?? undefined}
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
      <div role="status" class="flex items-center gap-2 rounded-lg bg-emerald-500/10 border border-emerald-500/25 px-2.5 py-1.5 text-[11px] text-emerald-400">
        <Check size={13} class="shrink-0" />
        <span>{notice}</span>
      </div>
    {/if}

    {#if error ?? (!notice && blockedReason)}
      <div role="alert" class="flex items-center gap-2 rounded-lg bg-rose-500/10 border border-rose-500/25 px-2.5 py-1.5 text-[11px] text-rose-400">
        <AlertTriangle size={13} class="shrink-0" />
        <span class="break-words min-w-0">{error ?? blockedReason}</span>
      </div>
    {/if}

    <!-- Expandable Developer CLI / SDK commands -->
    <div class="border-t border-border/60 pt-1.5">
      <button
        type="button"
        aria-expanded={detailsOpen}
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
              aria-pressed={previewTab === "prompt"}
              class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'prompt'
                ? 'border-accent text-accent'
                : 'border-transparent text-textMuted hover:text-textPrimary'}"
              onclick={() => (previewTab = "prompt")}
            >
              Prompt
            </button>
            <button
              type="button"
              aria-pressed={previewTab === "cli"}
              class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'cli'
                ? 'border-accent text-accent'
                : 'border-transparent text-textMuted hover:text-textPrimary'}"
              onclick={() => (previewTab = "cli")}
            >
              CLI Command
            </button>
            <button
              type="button"
              aria-pressed={previewTab === "sdk"}
              class="px-2 py-0.5 border-b-2 font-medium transition-colors {previewTab === 'sdk'
                ? 'border-accent text-accent'
                : 'border-transparent text-textMuted hover:text-textPrimary'}"
              onclick={() => (previewTab = "sdk")}
            >
              Python SDK
            </button>

            <button
              type="button"
              disabled={!previewText}
              onclick={() => void handleCopy(previewText)}
              class="ml-auto inline-flex items-center gap-1 text-[10px] text-textMuted hover:text-accent font-mono py-0.5 disabled:opacity-40"
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
          <pre class="w-full max-h-36 overflow-y-auto overscroll-contain p-2 rounded-lg bg-background border border-border/60 font-mono text-[9px] text-textSecondary leading-relaxed whitespace-pre-wrap break-all select-all gp-scroll">{previewText}</pre>
        </div>
      {/if}
    </div>
  </div>
{/if}
