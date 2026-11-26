<script lang="ts">
  /**
   * Writes the next turn. Supports Enter-to-send (Shift+Enter for new line) or
   * Cmd/Ctrl+Enter-to-send (Enter for new line), plus `/`, `@`, and `&`
   * trigger menus above the composer. While a turn runs, what is sent waits
   * in a visible queue (removable) and goes as one turn when the agent is
   * free; nothing is pasted into a busy agent. Stop interrupts the turn; Esc
   * closes an open trigger menu first, or stops when the composer is empty.
   *
   * The text is the thread's draft, held by the store, so switching threads
   * neither carries it to another thread nor loses it.
   */
  import { tick } from "svelte";
  import { ArrowUp, Square, X, ShieldAlert } from "@lucide/svelte";
  import { hostPlatform } from "../../stores/platformStore";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { listRepoFiles, peekRepoListing } from "../../files/repoListing";
  import { agentModelCatalogs, loadAgentModels } from "../../terminal/agentModelCatalog";
  import {
    applyTriggerReplacement,
    buildAgentModelOptions,
    detectTrigger,
    filterAgentModels,
    filterRepoFiles,
    filterSlashCommands,
    type AgentModelOption,
    type SlashCommandItem,
    type TriggerKind,
  } from "../../mavi/composerTriggers";
  import MaviTriggerMenu from "./MaviTriggerMenu.svelte";

  let {
    disabled = false,
    working,
    queue,
    pendingApprovals,
    draft = "",
    onDraft,
    modeLabel,
    placeholder = "Ask for a change… / commands · @ files · & agents",
    sendKey = "enter",
    textSize = "md",
    repoPath = null,
    touchedFiles = [],
    pendingHandoff = null,
    onSend,
    onStop,
    onUnqueue,
    onReviewApproval,
    onSlashAction,
    onPickAgentModel,
    onConfirmHandoff,
    onDismissHandoff,
  }: {
    disabled?: boolean;
    working: boolean;
    queue: readonly string[];
    /** Open requests not on screen now; the bar points at them. */
    pendingApprovals: number;
    draft?: string;
    onDraft?: (text: string) => void;
    modeLabel: string;
    placeholder?: string;
    sendKey?: "enter" | "mod_enter";
    textSize?: "sm" | "md" | "lg";
    repoPath?: string | null;
    touchedFiles?: readonly string[];
    pendingHandoff?: { agent: "claude" | "agy"; model: string | null } | null;
    onSend: (text: string) => Promise<void>;
    onStop: () => Promise<void>;
    onUnqueue: (index: number) => void;
    onReviewApproval: () => void;
    onSlashAction?: (actionId: string) => void;
    onPickAgentModel?: (agent: "claude" | "agy", model: string | null) => void;
    onConfirmHandoff?: () => void;
    onDismissHandoff?: () => void;
  } = $props();

  // Justified: the draft is read once; this composer is keyed by thread, so a
  // new thread mounts a new composer with its own draft.
  // svelte-ignore state_referenced_locally
  let text = $state(draft);
  let caret = $state(0);
  let dismissedKey = $state<string | null>(null);
  let selectedIndex = $state(0);
  let error = $state<string | null>(null);
  let sending = $state(false);
  let field: HTMLTextAreaElement | undefined = $state();

  let repoFiles = $state<readonly string[]>([]);
  let filesLoading = $state(false);
  let loadedRepoPath = "";
  let modelsRequested = false;

  let textClass = $derived(textSize === "sm" ? "text-xs" : textSize === "lg" ? "text-base" : "text-sm");
  let modChord = $derived(shortcutTextLabel("⌘↵", $hostPlatform.os));
  let hintText = $derived.by(() => {
    if (sendKey === "mod_enter") {
      return working ? `${modChord} queues · Enter new line · Esc stops` : `${modChord} sends · Enter new line`;
    }
    return working ? "Enter queues · Shift+Enter new line · Esc stops" : "Enter sends · Shift+Enter new line";
  });
  let sendTitle = $derived.by(() => {
    const keyLabel = sendKey === "mod_enter" ? modChord : "Enter";
    return working ? `Queue for when this turn ends (${keyLabel})` : `Send (${keyLabel})`;
  });

  let rawTrigger = $derived(disabled ? null : detectTrigger(text, caret));
  let activeTrigger = $derived(
    rawTrigger && `${rawTrigger.kind}:${rawTrigger.start}` !== dismissedKey ? rawTrigger : null,
  );

  let claudeCatalogState = $derived($agentModelCatalogs.claude);
  let agyCatalogState = $derived($agentModelCatalogs.agy);
  let modelsLoading = $derived(
    claudeCatalogState?.status === "loading" || agyCatalogState?.status === "loading",
  );
  let agentModelOptions = $derived.by(() => {
    const claudeModels =
      claudeCatalogState?.status === "ready"
        ? claudeCatalogState.catalog.models
        : claudeCatalogState?.previous?.models;
    const agyModels =
      agyCatalogState?.status === "ready"
        ? agyCatalogState.catalog.models
        : agyCatalogState?.previous?.models;
    return buildAgentModelOptions({ claude: claudeModels, agy: agyModels });
  });

  let slashCommands = $derived(
    activeTrigger?.kind === "/" ? filterSlashCommands(activeTrigger.query, { inThread: true, working }) : [],
  );
  let matchedFiles = $derived(
    activeTrigger?.kind === "@" ? filterRepoFiles(repoFiles, activeTrigger.query, touchedFiles) : [],
  );
  let matchedModels = $derived(
    activeTrigger?.kind === "&" ? filterAgentModels(agentModelOptions, activeTrigger.query) : [],
  );
  let itemCount = $derived(
    activeTrigger?.kind === "/"
      ? slashCommands.length
      : activeTrigger?.kind === "@"
        ? matchedFiles.length
        : activeTrigger?.kind === "&"
          ? matchedModels.length
          : 0,
  );
  let effectiveSelectedIndex = $derived(
    itemCount > 0 ? Math.min(selectedIndex, itemCount - 1) : 0,
  );

  let lastTriggerSignature = "";
  $effect(() => {
    const sig = activeTrigger ? `${activeTrigger.kind}:${activeTrigger.start}:${activeTrigger.query}` : "";
    if (sig !== lastTriggerSignature) {
      lastTriggerSignature = sig;
      selectedIndex = 0;
    }
  });

  $effect(() => {
    if (!rawTrigger) dismissedKey = null;
  });

  $effect(() => {
    const path = repoPath;
    if (path !== loadedRepoPath) {
      loadedRepoPath = "";
      repoFiles = path ? (peekRepoListing(path) ?? []) : [];
    }
    if (activeTrigger?.kind === "@" && path && loadedRepoPath !== path) {
      loadedRepoPath = path;
      filesLoading = true;
      void listRepoFiles(path)
        .then((files) => {
          if (repoPath === path) repoFiles = files;
        })
        .catch(() => {
          /* keep whatever listing is already held */
        })
        .finally(() => {
          if (repoPath === path) filesLoading = false;
        });
    }
  });

  $effect(() => {
    if (activeTrigger?.kind === "&" && !modelsRequested) {
      modelsRequested = true;
      void loadAgentModels("claude");
      void loadAgentModels("agy");
    }
  });

  function syncCaret() {
    if (!field) return;
    caret = field.selectionStart ?? text.length;
  }

  /** Grows the box with its text, up to a limit, then it scrolls. */
  function fit() {
    if (!field) return;
    field.style.height = "auto";
    field.style.height = `${Math.min(field.scrollHeight, 240)}px`;
  }

  async function applyReplacement(replacement: string) {
    if (!activeTrigger) return;
    const res = applyTriggerReplacement(text, activeTrigger, replacement);
    text = res.text;
    caret = res.caret;
    dismissedKey = null;
    onDraft?.(text);
    await tick();
    if (field) {
      field.focus();
      field.setSelectionRange(res.caret, res.caret);
    }
    fit();
  }

  function handleSelectCommand(cmd: SlashCommandItem) {
    if (cmd.kind === "action") {
      if (cmd.id === "clear") {
        text = "";
        caret = 0;
        dismissedKey = null;
        onDraft?.("");
        void tick().then(() => {
          field?.focus();
          fit();
        });
        return;
      }
      void applyReplacement("");
      onSlashAction?.(cmd.id);
      return;
    }
    void applyReplacement(cmd.insertText ?? "");
  }

  function handleSelectFile(path: string) {
    void applyReplacement(`@${path} `);
  }

  function handleSelectAgentModel(opt: AgentModelOption) {
    if (onPickAgentModel) {
      void applyReplacement("");
      onPickAgentModel(opt.agent, opt.model);
    } else {
      const mention = `&${opt.agent}${opt.model ? `:${opt.model}` : ""} `;
      void applyReplacement(mention);
    }
  }

  function selectActiveItem() {
    if (!activeTrigger || itemCount === 0) return;
    if (activeTrigger.kind === "/") {
      const cmd = slashCommands[effectiveSelectedIndex];
      if (cmd) handleSelectCommand(cmd);
    } else if (activeTrigger.kind === "@") {
      const path = matchedFiles[effectiveSelectedIndex];
      if (path) handleSelectFile(path);
    } else {
      const opt = matchedModels[effectiveSelectedIndex];
      if (opt) handleSelectAgentModel(opt);
    }
  }

  async function insertTrigger(ch: TriggerKind) {
    if (disabled) return;
    const start = field?.selectionStart ?? text.length;
    const end = field?.selectionEnd ?? start;
    const needSpace = start > 0 && !/\s/.test(text[start - 1]);
    const inserted = `${needSpace ? " " : ""}${ch}`;
    text = `${text.slice(0, start)}${inserted}${text.slice(end)}`;
    const nextCaret = start + inserted.length;
    caret = nextCaret;
    dismissedKey = null;
    onDraft?.(text);
    await tick();
    if (field) {
      field.focus();
      field.setSelectionRange(nextCaret, nextCaret);
    }
    fit();
  }

  async function send() {
    const value = text.trim();
    if (!value || sending || disabled) return;
    sending = true;
    error = null;
    try {
      await onSend(value);
      // Only what was sent is cleared: words typed while it went stay.
      if (text.trim() === value) {
        text = "";
        caret = 0;
      }
      onDraft?.(text);
      queueMicrotask(fit);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      sending = false;
    }
  }

  async function stop() {
    error = null;
    try {
      await onStop();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  function onKeydown(event: KeyboardEvent) {
    // keyCode 229 is the Enter that confirms an IME composition in WebKit.
    if (event.isComposing || event.keyCode === 229) return;

    if (activeTrigger) {
      if (event.key === "ArrowDown" && itemCount > 0) {
        event.preventDefault();
        selectedIndex = (effectiveSelectedIndex + 1) % itemCount;
        return;
      }
      if (event.key === "ArrowUp" && itemCount > 0) {
        event.preventDefault();
        selectedIndex = (effectiveSelectedIndex - 1 + itemCount) % itemCount;
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        dismissedKey = `${activeTrigger.kind}:${activeTrigger.start}`;
        return;
      }
      if (
        itemCount > 0 &&
        (event.key === "Tab" ||
          (event.key === "Enter" && !event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey))
      ) {
        event.preventDefault();
        selectActiveItem();
        return;
      }
    }

    if (event.key === "Enter") {
      if (sendKey === "mod_enter") {
        if ((event.metaKey || event.ctrlKey) && !event.shiftKey && !event.altKey) {
          event.preventDefault();
          void send();
        }
      } else if (!event.shiftKey && !event.metaKey && !event.ctrlKey && !event.altKey) {
        event.preventDefault();
        void send();
      }
    } else if (event.key === "Escape" && working && !text) {
      event.preventDefault();
      void stop();
    }
  }

  export function focus() {
    field?.focus();
  }
</script>

<div class="mavi-composer-wrap">
  <div class="mavi-lane">
    {#if pendingApprovals > 0}
      <button class="mavi-dock gp-notice w-full text-left flex items-center gap-2.5" data-tone="warn" onclick={onReviewApproval}>
        <ShieldAlert size={14} class="text-warn shrink-0" aria-hidden="true" />
        <span class="text-xs font-medium text-textPrimary min-w-0 flex-1">
          {pendingApprovals === 1 ? "The agent is waiting for your answer." : `${pendingApprovals} requests are waiting for your answer.`}
        </span>
        <span class="gp-chip shrink-0" data-tone="warn">Review</span>
      </button>
    {/if}
    {#if queue.length}
      <div class="mavi-queue gp-card">
        <div class="flex items-center justify-between text-micro text-textMuted mb-1.5">
          <span class="font-medium">Queued ({queue.length})</span>
          <span>Sends when this turn ends</span>
        </div>
        <ul class="mavi-queue-list" aria-label="Queued messages">
          {#each queue as item, i (i)}
            <li class="mavi-queue-item flex items-center gap-2 text-meta">
              <span class="gp-chip shrink-0" data-kind="count" data-tone="neutral">#{i + 1}</span>
              <span class="truncate flex-1 text-textPrimary">{item}</span>
              <button
                class="gp-icon-btn shrink-0"
                data-size="sm"
                aria-label="Remove queued message #{i + 1}"
                title="Remove from queue"
                onclick={() => onUnqueue(i)}
              >
                <X size={11} aria-hidden="true" />
              </button>
            </li>
          {/each}
        </ul>
      </div>
    {/if}
    {#if pendingHandoff}
      <div class="mavi-handoff gp-notice flex items-center gap-2" data-tone="info">
        <span class="text-xs text-textPrimary min-w-0 flex-1 truncate">
          {pendingHandoff.agent === "agy" ? "Hand off to Antigravity" : "New Claude Code thread"}{pendingHandoff.model ? ` · ${pendingHandoff.model}` : ""}
        </span>
        <button type="button" class="gp-btn shrink-0" data-size="sm" onclick={() => onConfirmHandoff?.()}>Open</button>
        <button
          type="button"
          class="gp-icon-btn shrink-0"
          data-size="sm"
          aria-label="Dismiss handoff"
          title="Dismiss"
          onclick={() => onDismissHandoff?.()}
        >
          <X size={11} aria-hidden="true" />
        </button>
      </div>
    {/if}
    <div class="relative">
      <MaviTriggerMenu
        trigger={activeTrigger}
        commands={slashCommands}
        files={matchedFiles}
        {filesLoading}
        agentModels={matchedModels}
        {modelsLoading}
        selectedIndex={effectiveSelectedIndex}
        onSelectCommand={handleSelectCommand}
        onSelectFile={handleSelectFile}
        onSelectAgentModel={handleSelectAgentModel}
        onHoverIndex={(index) => (selectedIndex = index)}
        onClose={() => {
          if (activeTrigger) dismissedKey = `${activeTrigger.kind}:${activeTrigger.start}`;
        }}
      />
      <div class="mavi-composer gp-card" class:mavi-composer-disabled={disabled}>
        <textarea
          bind:this={field}
          bind:value={text}
          rows="1"
          class="mavi-input {textClass} text-textPrimary"
          {placeholder}
          aria-label="Message to the agent"
          {disabled}
          onkeydown={onKeydown}
          onkeyup={syncCaret}
          onclick={syncCaret}
          onfocus={syncCaret}
          onblur={() => {
            if (activeTrigger) dismissedKey = `${activeTrigger.kind}:${activeTrigger.start}`;
          }}
          oninput={() => {
            syncCaret();
            fit();
            onDraft?.(text);
          }}
        ></textarea>
        <div class="flex items-center gap-1.5 pt-1">
          <span class="gp-chip shrink-0" data-kind="label" title="Permission mode, chosen when the thread started">{modeLabel}</span>
          <div class="flex items-center gap-0.5 shrink-0">
            <button
              type="button"
              class="gp-icon-btn"
              data-size="sm"
              {disabled}
              aria-label="Insert slash command"
              title="Insert / command"
              onmousedown={(e) => e.preventDefault()}
              onclick={() => void insertTrigger("/")}
            >
              <span class="font-mono text-xs font-medium" aria-hidden="true">/</span>
            </button>
            <button
              type="button"
              class="gp-icon-btn"
              data-size="sm"
              {disabled}
              aria-label="Mention a file"
              title="Mention a file (@)"
              onmousedown={(e) => e.preventDefault()}
              onclick={() => void insertTrigger("@")}
            >
              <span class="font-mono text-xs font-medium" aria-hidden="true">@</span>
            </button>
            <button
              type="button"
              class="gp-icon-btn"
              data-size="sm"
              {disabled}
              aria-label="Choose agent or model"
              title="Choose agent or model (&amp;)"
              onmousedown={(e) => e.preventDefault()}
              onclick={() => void insertTrigger("&")}
            >
              <span class="font-mono text-xs font-medium" aria-hidden="true">&amp;</span>
            </button>
          </div>
          <span class="text-micro text-textMuted truncate">{hintText}</span>
          <span class="ml-auto"></span>
          {#if working}
            <button class="gp-btn shrink-0" data-variant="secondary" onclick={stop} title="Stop this turn">
              <Square size={11} aria-hidden="true" /> Stop
            </button>
          {/if}
          <button
            class="gp-btn shrink-0"
            data-variant="primary"
            disabled={disabled || sending || !text.trim()}
            onclick={send}
            title={sendTitle}
          >
            <ArrowUp size={12} aria-hidden="true" /> {working ? "Queue" : "Send"}
          </button>
        </div>
        {#if error}
          <p class="text-meta text-danger pt-1.5" role="alert">{error}</p>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  .mavi-composer-wrap {
    padding: 8px 0 14px;
    background: linear-gradient(to bottom, transparent, rgb(var(--c-bg)) 18px);
  }
  .mavi-lane {
    max-width: var(--mavi-lane-max, 52rem);
    margin: 0 auto;
    padding: 0 20px;
  }
  .mavi-composer {
    padding: 10px 14px;
    transition: border-color var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-composer:focus-within {
    border-color: var(--field-focus-edge, rgb(var(--c-accent) / 0.65));
  }
  .mavi-composer-disabled {
    opacity: 0.6;
  }
  .mavi-input {
    display: block;
    width: 100%;
    resize: none;
    background: transparent;
    border: 0;
    outline: none;
    padding: 2px 2px 8px;
    min-height: 2.25em;
    -webkit-text-fill-color: rgb(var(--c-text));
  }
  .mavi-input::placeholder {
    color: rgb(var(--c-text-faint));
    -webkit-text-fill-color: rgb(var(--c-text-faint));
  }
  .mavi-queue {
    padding: 8px 10px;
    margin-bottom: 8px;
    background: rgb(var(--c-surface) / 0.65);
  }
  .mavi-queue-list {
    display: grid;
    gap: 4px;
  }
  .mavi-queue-item {
    padding: 2px 4px;
    border-radius: var(--gp-r-sm);
  }
  .mavi-dock {
    margin: 0 0 8px;
    padding: 8px 12px;
    transition: border-color var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-dock:hover {
    border-color: rgb(var(--c-warn) / 0.65);
  }
  .mavi-handoff {
    margin: 0 0 8px;
    padding: 6px 10px;
  }
</style>
