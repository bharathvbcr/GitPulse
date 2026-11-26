<script lang="ts">
  /**
   * Starts a thread from a single elevated composer hero sitting on the calm
   * canvas. Inline triggers (`/`, `@`, `&`) complete commands, repository files,
   * and agents/models directly at the caret, while anchored popovers in the
   * card toolbar hold agent/model, target, and permission mode choices.
   */
  import { onMount, tick, untrack } from "svelte";
  import {
    AtSign,
    Check,
    ChevronDown,
    CircleCheck,
    Eye,
    Laptop,
    LoaderCircle,
    Pencil,
    RefreshCw,
    RotateCcw,
    Server,
    ShieldAlert,
    ShieldCheck,
    ShieldQuestion,
    Slash,
    Sparkles,
    TriangleAlert,
  } from "@lucide/svelte";
  import { MAVI_MODES, maviClient, type MaviModeId, type Probe } from "../../mavi/client";
  import { maviStore } from "../../mavi/store.svelte";
  import { maviPrefs } from "../../mavi/prefs.svelte";
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
  import { currentRepoPath, repoStore } from "../../stores/repoStore";
  import { agentDefaults, agentDefaultsStore, saveAgentDefaults } from "../../stores/agentDefaultsStore";
  import { effectiveMode, modelChoiceOf } from "../../terminal/agentDefaults";
  import { agentModelCatalogs, loadAgentModels } from "../../terminal/agentModelCatalog";
  import { terminalLaunchRequests } from "../../terminal/launchRequests";
  import { listRepoFiles } from "../../files/repoListing";
  import { planWorktree, slugFromMessage } from "../../mavi/worktreeStart";
  import { reportFailure } from "../../stores/mutationOutcome";
  import { worktreeSetupNotices } from "../../branches/types";
  import { toastStore } from "../../stores/toastStore";
  import { runningAgents, terminalSessions } from "../../terminal/sessionRegistry";
  import { displayName, isCaseInsensitiveFs, sameRepo } from "../../repos/paths";
  import { portal } from "../../dom/portal";
  import { LAYERS } from "../../ui/layers";
  import { popupEnter } from "../../ui/headerMotion";
  import { popover, popoverExit, restoreFocusTo, type PopoverOptions } from "../../ui/popover";
  import { shortcutTextLabel } from "../../ui/platformCopy";
  import { hostPlatform } from "../../stores/platformStore";
  import Checkbox from "../Checkbox.svelte";
  import LauncherIcon from "../LauncherIcon.svelte";
  import MaviTriggerMenu from "./MaviTriggerMenu.svelte";

  let {
    onStarted,
    onCancel,
    onOpenCustomize,
    prefill = "",
    preferWorktree = false,
    initialAgent,
    initialModel,
  }: {
    onStarted: (key: string) => void;
    onCancel?: () => void;
    onOpenCustomize?: () => void;
    /** The start of the first message, from a request elsewhere in the app. */
    prefill?: string;
    preferWorktree?: boolean;
    initialAgent?: "claude" | "agy";
    initialModel?: string;
  } = $props();

  const uid = $props.id();

  const QUICK_PROMPTS = [
    "Explain how this repository is structured",
    "Find and fix failing tests",
    "Review uncommitted changes",
  ] as const;

  const MODE_ICONS: Record<MaviModeId, typeof Eye> = {
    inspect: Eye,
    ask: ShieldQuestion,
    edit: Pencil,
    auto_review: Sparkles,
    preapproved: ShieldCheck,
    bypass: ShieldAlert,
  };

  function seededMode(): MaviModeId {
    try {
      return (effectiveMode(agentDefaults().defaults, "claude") ?? "ask") as MaviModeId;
    } catch {
      return "ask";
    }
  }
  function seededModelFor(kind: "claude" | "agy"): string {
    try {
      return modelChoiceOf(agentDefaults().defaults, kind).model ?? "";
    } catch {
      return "";
    }
  }

  let defaultClaudeModel = $derived(modelChoiceOf($agentDefaultsStore.defaults, "claude").model ?? "");
  let defaultAgyModel = $derived(modelChoiceOf($agentDefaultsStore.defaults, "agy").model ?? "");

  let agentChosen = false;
  let agent = $state<"claude" | "agy">(untrack(() => initialAgent ?? maviPrefs.defaultAgent));
  $effect(() => {
    const pref = maviPrefs.defaultAgent;
    if (!initialAgent && !agentChosen) {
      agent = pref;
      if (pref === "agy" && untrack(() => where) === "ssh") where = "local";
    }
  });

  let where = $state<"local" | "ssh">("local");
  let host = $state("");
  let dir = $state("~/");
  let hosts = $state<string[]>([]);
  let probe = $state<Probe | null>(null);
  let probing = $state(false);
  let mode = $state<MaviModeId>(seededMode());
  let acknowledged = $state(false);
  let model = $state(
    untrack(() => initialModel ?? seededModelFor(initialAgent ?? maviPrefs.defaultAgent)),
  );
  let applyAgyModelAsDefault = $state(true);
  let message = $state(untrack(() => prefill));
  let caret = $state(untrack(() => prefill.length));
  let promptEl: HTMLTextAreaElement | undefined = $state();

  // Toolbar popovers (one open at a time)
  type ToolbarPopover = "agent" | "target" | "mode" | null;
  let openPopover = $state<ToolbarPopover>(null);
  let agentTriggerEl: HTMLButtonElement | undefined = $state();
  let targetTriggerEl: HTMLButtonElement | undefined = $state();
  let modeTriggerEl: HTMLButtonElement | undefined = $state();

  function popoverConfig(kind: Exclude<ToolbarPopover, null>, anchorEl: HTMLElement | undefined): PopoverOptions {
    return {
      anchor: { kind: "element", element: anchorEl, gap: 6, align: "start" },
      inset: 8,
      dismiss: {
        inside: `[data-mavi-start-pop="${uid}-${kind}"]`,
        escape: "capture",
        resize: true,
      },
      onDismiss: (reason) => {
        if (openPopover === kind) openPopover = null;
        if (reason === "escape") restoreFocusTo(anchorEl);
      },
    };
  }

  function togglePopover(kind: Exclude<ToolbarPopover, null>) {
    openPopover = openPopover === kind ? null : kind;
    if (openPopover === "agent") ensureAgentCatalogs();
  }

  // Trigger menu state (`/`, `@`, `&`)
  let triggerSuppressed = $state(false);
  let selectedIndex = $state(0);
  let lastTriggerSignature = "";

  let repoFiles = $state<readonly string[]>([]);
  let repoFilesLoading = $state(false);
  let loadedRepoFor = "";

  function ensureRepoFiles() {
    const repo = $currentRepoPath;
    if (!repo || (loadedRepoFor === repo && repoFiles.length > 0)) return;
    if (repoFilesLoading && loadedRepoFor === repo) return;
    loadedRepoFor = repo;
    repoFilesLoading = true;
    void listRepoFiles(repo)
      .then((list) => {
        if (loadedRepoFor === repo) repoFiles = list;
      })
      .catch(() => {
        if (loadedRepoFor === repo) repoFiles = [];
      })
      .finally(() => {
        if (loadedRepoFor === repo) repoFilesLoading = false;
      });
  }

  let requestedCatalogs = false;
  function ensureAgentCatalogs() {
    if (requestedCatalogs) return;
    requestedCatalogs = true;
    void loadAgentModels("claude").catch(() => {});
    void loadAgentModels("agy").catch(() => {});
  }

  let claudeCatalogState = $derived($agentModelCatalogs.claude);
  let agyCatalogState = $derived($agentModelCatalogs.agy);
  let claudeCatalogModels = $derived(
    claudeCatalogState?.status === "ready"
      ? claudeCatalogState.catalog.models
      : claudeCatalogState?.previous?.models ?? [],
  );
  let agyCatalogModels = $derived(
    agyCatalogState?.status === "ready"
      ? agyCatalogState.catalog.models
      : agyCatalogState?.previous?.models ?? [],
  );
  let modelsLoading = $derived(
    claudeCatalogState?.status === "loading" || agyCatalogState?.status === "loading",
  );
  let activeCatalogState = $derived(agent === "claude" ? claudeCatalogState : agyCatalogState);
  let activeCatalogLoading = $derived(activeCatalogState?.status === "loading");
  let activeCatalogError = $derived(
    activeCatalogState?.status === "failed" ? activeCatalogState.message : null,
  );

  let rawTrigger = $derived(detectTrigger(message, caret));
  $effect(() => {
    const sig = rawTrigger ? `${rawTrigger.kind}:${rawTrigger.start}:${rawTrigger.query}` : "";
    if (sig !== lastTriggerSignature) {
      lastTriggerSignature = sig;
      triggerSuppressed = false;
      selectedIndex = 0;
    }
  });

  let activeTrigger = $derived(triggerSuppressed ? null : rawTrigger);
  $effect(() => {
    if (!activeTrigger) return;
    if (activeTrigger.kind === "@") ensureRepoFiles();
    else if (activeTrigger.kind === "&") ensureAgentCatalogs();
  });

  let slashCommands = $derived(
    activeTrigger?.kind === "/" ? filterSlashCommands(activeTrigger.query, { inThread: false, working: false }) : [],
  );
  let matchedFiles = $derived(
    activeTrigger?.kind === "@" ? filterRepoFiles(repoFiles, activeTrigger.query) : [],
  );
  let allAgentModels = $derived(
    buildAgentModelOptions({ claude: claudeCatalogModels, agy: agyCatalogModels }),
  );
  let matchedAgentModels = $derived(
    activeTrigger?.kind === "&" ? filterAgentModels(allAgentModels, activeTrigger.query) : [],
  );

  let menuCount = $derived(
    activeTrigger?.kind === "/"
      ? slashCommands.length
      : activeTrigger?.kind === "@"
        ? matchedFiles.length
        : activeTrigger?.kind === "&"
          ? matchedAgentModels.length
          : 0,
  );
  let activeMenuIndex = $derived(menuCount > 0 ? Math.min(selectedIndex, menuCount - 1) : 0);

  let pickerModelsForActiveAgent = $derived(
    allAgentModels.filter((opt) => opt.agent === agent && !opt.isAgentDefault),
  );

  /** A live thread already works in this repository: the host allows one per worktree. */
  let busyHere = $derived(
    !!$currentRepoPath &&
      maviStore.records.some((r) => {
        if (r.target.kind !== "local") return false;
        const phase = maviStore.summaryOf(r)?.phase;
        return !!phase && phase !== "ended" && r.target.root.replace(/[\\/]+$/, "") === $currentRepoPath!.replace(/[\\/]+$/, "");
      }),
  );
  /** Agent terminals already working in this checkout (their edits are not this thread's). */
  let terminalAgentsHere = $derived(
    $currentRepoPath
      ? runningAgents($terminalSessions).filter((record) =>
          sameRepo(record.checkout ?? record.repoPath, $currentRepoPath!, { caseInsensitive: isCaseInsensitiveFs() }),
        ).length
      : 0,
  );
  let newWorktree = $state(untrack(() => preferWorktree));
  $effect(() => {
    if (busyHere) newWorktree = true;
  });

  let starting = $state(false);
  let error = $state<string | null>(null);

  function fit() {
    if (!promptEl) return;
    promptEl.style.height = "auto";
    promptEl.style.height = `${Math.max(112, Math.min(promptEl.scrollHeight, 240))}px`;
  }

  function syncCaret() {
    caret = promptEl?.selectionStart ?? message.length;
  }

  onMount(() => {
    void maviClient.sshHosts().then((list) => (hosts = list)).catch(() => (hosts = []));
    if (message) fit();
    if (typeof document !== "undefined" && !document.querySelector("[data-dialog-scrim]")) {
      promptEl?.focus();
    }
  });

  let modeInfo = $derived(MAVI_MODES.find((m) => m.id === mode)!);
  let ModeIcon = $derived(MODE_ICONS[mode]);
  let defaultForAgent = $derived(agent === "claude" ? defaultClaudeModel : defaultAgyModel);
  let modelOverridden = $derived(model.trim() !== defaultForAgent);
  let agyModelDiffersFromDefault = $derived(agent === "agy" && model.trim() !== defaultAgyModel);
  let sendKeyLabel = $derived(
    maviPrefs.sendKey === "enter" ? "↵" : shortcutTextLabel("⌘↵", $hostPlatform.os),
  );
  let probeOk = $derived(probe?.kind === "ok" || probe?.kind === "not_a_repo");
  let ready = $derived(
    message.trim().length > 0 &&
      (agent === "agy"
        ? !!$currentRepoPath
        : (mode !== "bypass" || acknowledged) &&
          (where === "local" ? !!$currentRepoPath : host.trim().length > 0 && dir.trim().length > 0 && probeOk)),
  );

  $effect(() => {
    // A changed host or directory needs a new check.
    void host;
    void dir;
    probe = null;
  });

  async function applyReplacement(replacement: string) {
    if (!activeTrigger) return;
    const next = applyTriggerReplacement(message, activeTrigger, replacement);
    message = next.text;
    caret = next.caret;
    triggerSuppressed = false;
    selectedIndex = 0;
    await tick();
    fit();
    promptEl?.focus();
    promptEl?.setSelectionRange(next.caret, next.caret);
  }

  async function selectCommand(cmd: SlashCommandItem) {
    if (cmd.id === "clear") {
      message = "";
      caret = 0;
      triggerSuppressed = false;
      await tick();
      fit();
      promptEl?.focus();
      return;
    }
    if (cmd.id === "customize") {
      await applyReplacement("");
      onOpenCustomize?.();
      return;
    }
    if (cmd.id === "worktree") {
      await applyReplacement("");
      if (busyHere) {
        newWorktree = true;
        toastStore.info("Another Mavi thread is working in this repository, so a new worktree is required.");
      } else {
        newWorktree = !newWorktree;
      }
      return;
    }
    if (cmd.kind === "prompt") {
      await applyReplacement(cmd.insertText ?? "");
    }
  }

  async function selectFile(path: string) {
    await applyReplacement(`@${path} `);
  }

  async function selectAgentModel(opt: AgentModelOption) {
    agentChosen = true;
    agent = opt.agent;
    model = opt.model ?? (opt.agent === "claude" ? defaultClaudeModel : defaultAgyModel);
    if (opt.agent === "agy") where = "local";
    await applyReplacement("");
  }

  async function insertTrigger(char: TriggerKind) {
    openPopover = null;
    const pos = promptEl?.selectionStart ?? message.length;
    const endPos = promptEl?.selectionEnd ?? pos;
    const prevChar = pos > 0 ? message.charAt(pos - 1) : "";
    const needsSpace = prevChar !== "" && !/\s/.test(prevChar);
    const inserted = needsSpace ? ` ${char}` : char;
    message = `${message.slice(0, pos)}${inserted}${message.slice(endPos)}`;
    const nextCaret = pos + inserted.length;
    caret = nextCaret;
    triggerSuppressed = false;
    selectedIndex = 0;
    await tick();
    fit();
    promptEl?.focus();
    promptEl?.setSelectionRange(nextCaret, nextCaret);
  }

  async function pickQuickPrompt(prompt: string) {
    message = prompt;
    caret = prompt.length;
    await tick();
    fit();
    promptEl?.focus();
  }

  function switchAgent(nextAgent: "claude" | "agy") {
    if (agent === nextAgent) return;
    agentChosen = true;
    agent = nextAgent;
    model = nextAgent === "claude" ? defaultClaudeModel : defaultAgyModel;
    if (nextAgent === "agy") where = "local";
  }

  async function check() {
    probing = true;
    error = null;
    try {
      probe = await maviClient.sshProbe(host.trim(), dir.trim());
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      probing = false;
    }
  }

  async function persistAgyModelDefault(nextModel: string) {
    const currentDefaults = agentDefaults().defaults;
    const nextModels = { ...(currentDefaults.models ?? {}) };
    const trimmed = nextModel.trim();
    if (trimmed) {
      nextModels.agy = { ...(nextModels.agy ?? {}), model: trimmed };
    } else {
      const existingAgy = { ...(nextModels.agy ?? {}) };
      delete existingAgy.model;
      if (Object.keys(existingAgy).length > 0) nextModels.agy = existingAgy;
      else delete nextModels.agy;
    }
    await saveAgentDefaults({
      ...currentDefaults,
      models: nextModels,
    });
  }

  async function start() {
    if (!ready || starting) return;
    starting = true;
    error = null;
    try {
      const finalMessage = message.trim();
      if (agent === "agy") {
        let repoPath = $currentRepoPath;
        if (!repoPath) return;
        if (agyModelDiffersFromDefault && applyAgyModelAsDefault) {
          await persistAgyModelDefault(model);
        }
        if (newWorktree) {
          repoPath = await createWorktree(repoPath);
          if (!repoPath) return;
          await repoStore.trustRepo(repoPath);
          await repoStore.openRepo(repoPath, { activate: true });
        }
        const opened = terminalLaunchRequests.request(repoPath, "agy", finalMessage);
        repoStore.setTerminalOpen(true);
        await opened;
        message = "";
        caret = 0;
        onCancel?.();
        return;
      }

      let repoPath = $currentRepoPath;
      if (where === "local" && newWorktree && repoPath) {
        repoPath = await createWorktree(repoPath);
        if (!repoPath) return;
      }
      const key = await maviStore.create({
        target:
          where === "local"
            ? { kind: "local", repoPath: repoPath! }
            : { kind: "ssh", host: host.trim(), dir: dir.trim() },
        mode,
        acknowledged: mode === "bypass" ? acknowledged : false,
        model: model.trim() || undefined,
        firstMessage: finalMessage,
      });
      onStarted(key);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      starting = false;
    }
  }

  /** Creates the sibling worktree; its path, or null after saying why not. */
  async function createWorktree(repo: string): Promise<string | null> {
    const plan = planWorktree(repo, slugFromMessage(message));
    if (!plan) {
      error = "Couldn't work out where to put a sibling worktree for this repository.";
      return null;
    }
    const outcome = await repoStore.addWorktree(
      {
        targetPath: plan.path,
        newBranch: plan.branch,
        startPoint: null,
        detach: false,
        cowCaches: true,
        runHooks: false,
        zeroCopyDevmap: false,
        seedDevmap: true,
        sparsePaths: null,
      },
      repoStore.captureTarget() ?? undefined,
      true,
    );
    if (!outcome.ok) {
      if (outcome.refused) reportFailure(outcome, "Couldn't create the worktree");
      else error = `Couldn't create the worktree: ${outcome.error ?? "it did not complete."}`;
      return null;
    }
    for (const notice of worktreeSetupNotices(outcome.output ?? {}, "create")) toastStore.warning(notice);
    return outcome.output?.path ?? plan.path;
  }

  function onKeydown(event: KeyboardEvent) {
    if (activeTrigger) {
      if (event.key === "ArrowDown" && menuCount > 0) {
        event.preventDefault();
        selectedIndex = (activeMenuIndex + 1) % menuCount;
        return;
      }
      if (event.key === "ArrowUp" && menuCount > 0) {
        event.preventDefault();
        selectedIndex = (activeMenuIndex - 1 + menuCount) % menuCount;
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        triggerSuppressed = true;
        return;
      }
      if ((event.key === "Tab" || (event.key === "Enter" && !event.shiftKey)) && menuCount > 0) {
        event.preventDefault();
        if (activeTrigger.kind === "/") {
          const cmd = slashCommands[activeMenuIndex];
          if (cmd) void selectCommand(cmd);
        } else if (activeTrigger.kind === "@") {
          const file = matchedFiles[activeMenuIndex];
          if (file) void selectFile(file);
        } else {
          const opt = matchedAgentModels[activeMenuIndex];
          if (opt) void selectAgentModel(opt);
        }
        return;
      }
    }

    // keyCode 229 is the Enter that confirms an IME composition in WebKit.
    if (event.isComposing || event.keyCode === 229) return;
    if (maviPrefs.sendKey === "enter") {
      if (event.key === "Enter" && !event.shiftKey) {
        event.preventDefault();
        void start();
      }
    } else if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      void start();
    }
  }
</script>

<form
  class="mavi-start-shell w-full flex flex-col items-center gap-3.5"
  onsubmit={(e) => {
    e.preventDefault();
    void start();
  }}
  aria-label="New Mavi thread"
>
  <div class="mavi-hero gp-card relative w-full rounded-2xl">
    <MaviTriggerMenu
      trigger={activeTrigger}
      placement="below"
      commands={slashCommands}
      files={matchedFiles}
      filesLoading={repoFilesLoading}
      agentModels={matchedAgentModels}
      {modelsLoading}
      selectedIndex={activeMenuIndex}
      onSelectCommand={(cmd) => void selectCommand(cmd)}
      onSelectFile={(path) => void selectFile(path)}
      onSelectAgentModel={(opt) => void selectAgentModel(opt)}
      onHoverIndex={(idx) => (selectedIndex = idx)}
      onClose={() => (triggerSuppressed = true)}
    />

    <textarea
      bind:this={promptEl}
      bind:value={message}
      rows="4"
      class="mavi-hero-input text-sm text-textPrimary leading-relaxed"
      placeholder="Ask for a change…  / commands · @ files · & agents"
      aria-label="First message"
      oninput={() => {
        fit();
        syncCaret();
      }}
      onclick={syncCaret}
      onkeyup={syncCaret}
      onselect={syncCaret}
      onblur={() => (triggerSuppressed = true)}
      onkeydown={onKeydown}
    ></textarea>

    <div class="mavi-hero-bar flex flex-wrap items-center justify-between gap-2 pt-2">
      <div class="flex flex-wrap items-center gap-1.5 min-w-0">
        <!-- Agent & Model pill -->
        <div class="relative" data-mavi-start-pop={`${uid}-agent`}>
          <button
            type="button"
            bind:this={agentTriggerEl}
            class="gp-chip cursor-pointer px-2.5 py-1 text-xs gap-1.5"
            data-tone="neutral"
            aria-expanded={openPopover === "agent"}
            aria-haspopup="dialog"
            data-testid="mavi-start-agent-pill"
            onclick={() => togglePopover("agent")}
          >
            <LauncherIcon kind={agent} size={13} />
            <span class="font-medium text-textPrimary">{agent === "agy" ? "Antigravity" : "Claude Code"}</span>
            <span class="text-textMuted font-mono truncate">{model.trim() || "Default"}</span>
            <ChevronDown size={11} class="text-textMuted" aria-hidden="true" />
          </button>

          {#if openPopover === "agent"}
            <div
              use:portal={"body"}
              use:popover={popoverConfig("agent", agentTriggerEl)}
              in:popupEnter
              out:popoverExit
              data-mavi-start-pop={`${uid}-agent`}
              class="mavi-start-popover fixed gp-menu w-72"
              style:z-index={LAYERS.MENU}
              role="dialog"
              aria-label="Agent and model"
            >
              <div class="p-2.5 flex flex-col gap-2.5">
                <div class="gp-segmented w-full" data-size="sm" role="group" aria-label="Agent">
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 inline-flex items-center justify-center gap-1.5"
                    data-active={agent === "claude"}
                    aria-pressed={agent === "claude"}
                    onclick={() => switchAgent("claude")}
                  >
                    <LauncherIcon kind="claude" size={12} />
                    <span>Claude Code</span>
                  </button>
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 inline-flex items-center justify-center gap-1.5"
                    data-active={agent === "agy"}
                    aria-pressed={agent === "agy"}
                    onclick={() => switchAgent("agy")}
                  >
                    <LauncherIcon kind="agy" size={12} />
                    <span>Antigravity</span>
                  </button>
                </div>

                <div class="flex items-center justify-between gap-2 px-1">
                  <span class="gp-label">Models</span>
                  <button
                    type="button"
                    class="gp-btn"
                    data-variant="ghost"
                    data-size="sm"
                    disabled={activeCatalogLoading}
                    aria-label="Refresh models"
                    title="Refresh model list"
                    onclick={() => void loadAgentModels(agent, true)}
                  >
                    <RefreshCw size={11} class={activeCatalogLoading ? "animate-spin" : ""} aria-hidden="true" />
                    <span>Refresh</span>
                  </button>
                </div>

                <div class="flex flex-col gap-0.5 max-h-48 overflow-y-auto">
                  <button
                    type="button"
                    class="gp-menu-item justify-between"
                    class:bg-surfaceHover={!model.trim()}
                    onclick={() => {
                      agentChosen = true;
                      model = "";
                      openPopover = null;
                    }}
                  >
                    <span class="text-xs text-textPrimary">Default</span>
                    {#if !model.trim()}<Check size={12} class="text-accent shrink-0" aria-hidden="true" />{/if}
                  </button>

                  {#if activeCatalogLoading && pickerModelsForActiveAgent.length === 0}
                    <div class="flex items-center gap-1.5 px-2.5 py-1.5 text-micro text-textMuted">
                      <LoaderCircle size={11} class="animate-spin shrink-0" aria-hidden="true" />
                      <span>Loading models…</span>
                    </div>
                  {:else if activeCatalogError && pickerModelsForActiveAgent.length === 0}
                    <p class="px-2.5 py-1.5 text-micro text-warn leading-snug">{activeCatalogError}</p>
                  {:else if pickerModelsForActiveAgent.length === 0}
                    <p class="px-2.5 py-1.5 text-micro text-textMuted">No models listed.</p>
                  {/if}

                  {#each pickerModelsForActiveAgent as opt (opt.id)}
                    <button
                      type="button"
                      class="gp-menu-item justify-between"
                      class:bg-surfaceHover={model.trim() === opt.model}
                      onclick={() => {
                        agentChosen = true;
                        model = opt.model ?? "";
                        openPopover = null;
                      }}
                    >
                      <span class="min-w-0 flex-1 truncate text-xs font-mono text-textPrimary">{opt.label}</span>
                      {#if model.trim() === opt.model}<Check size={12} class="text-accent shrink-0" aria-hidden="true" />{/if}
                    </button>
                  {/each}
                </div>

                <div class="flex items-center gap-1.5">
                  <input
                    class="gp-field font-mono flex-1 min-w-0"
                    data-size="sm"
                    bind:value={model}
                    placeholder="Custom model id…"
                    aria-label="Custom model id"
                    spellcheck="false"
                  />
                  {#if modelOverridden}
                    <button
                      type="button"
                      class="gp-icon-btn shrink-0"
                      data-size="sm"
                      title="Reset to default model"
                      aria-label="Reset to default model"
                      onclick={() => (model = defaultForAgent)}
                    >
                      <RotateCcw size={11} aria-hidden="true" />
                    </button>
                  {/if}
                </div>
              </div>
            </div>
          {/if}
        </div>

        <!-- Target pill -->
        <div class="relative" data-mavi-start-pop={`${uid}-target`}>
          <button
            type="button"
            bind:this={targetTriggerEl}
            class="gp-chip cursor-pointer px-2.5 py-1 text-xs gap-1.5"
            data-tone="neutral"
            aria-expanded={openPopover === "target"}
            aria-haspopup="dialog"
            data-testid="mavi-start-target-pill"
            onclick={() => togglePopover("target")}
          >
            {#if where === "ssh" && agent === "claude"}
              <Server size={12} class="text-textMuted" aria-hidden="true" />
              <span class="text-textPrimary truncate max-w-36">{host.trim() || "SSH host"}</span>
            {:else}
              <Laptop size={12} class="text-textMuted" aria-hidden="true" />
              <span class="text-textPrimary truncate max-w-36">
                {$currentRepoPath ? displayName($currentRepoPath) : "No repository"}
              </span>
              {#if newWorktree && $currentRepoPath}
                <span class="text-accent">· worktree</span>
              {/if}
            {/if}
            <ChevronDown size={11} class="text-textMuted" aria-hidden="true" />
          </button>

          {#if openPopover === "target"}
            <div
              use:portal={"body"}
              use:popover={popoverConfig("target", targetTriggerEl)}
              in:popupEnter
              out:popoverExit
              data-mavi-start-pop={`${uid}-target`}
              class="mavi-start-popover fixed gp-menu w-80"
              style:z-index={LAYERS.MENU}
              role="dialog"
              aria-label="Where the agent runs"
            >
              <div class="p-2.5 flex flex-col gap-2.5">
                <div class="gp-segmented w-full" data-size="sm" role="group" aria-label="Target environment">
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 inline-flex items-center justify-center gap-1.5"
                    data-active={where === "local"}
                    aria-pressed={where === "local"}
                    onclick={() => (where = "local")}
                  >
                    <Laptop size={12} aria-hidden="true" />
                    <span>This repository</span>
                  </button>
                  <button
                    type="button"
                    class="gp-seg-btn flex-1 inline-flex items-center justify-center gap-1.5"
                    data-active={where === "ssh"}
                    aria-pressed={where === "ssh"}
                    disabled={agent === "agy"}
                    title={agent === "agy" ? "Antigravity sessions run locally in the terminal dock" : "Run on an SSH host"}
                    onclick={() => (where = "ssh")}
                  >
                    <Server size={12} aria-hidden="true" />
                    <span>SSH host</span>
                  </button>
                </div>

                {#if where === "local"}
                  {#if $currentRepoPath}
                    <div class="rounded-lg bg-background/50 px-2.5 py-2 flex flex-col gap-1">
                      <span class="text-xs font-medium text-textPrimary truncate">{displayName($currentRepoPath)}</span>
                      <span class="text-micro text-textMuted font-mono truncate" title={$currentRepoPath}>{$currentRepoPath}</span>
                    </div>
                    <Checkbox
                      label="Run in a new worktree"
                      description={busyHere
                        ? "Another Mavi thread is working in this repository; a thread needs a worktree of its own."
                        : "A sibling worktree on a new mavi/ branch, so this thread's changes stay apart."}
                      checked={newWorktree}
                      onchange={(next) => (newWorktree = next)}
                    />
                  {:else}
                    <p class="text-meta text-textMuted">Open a repository first.</p>
                  {/if}
                {:else}
                  <p class="text-meta text-textMuted">Configure host, directory, and connection check below the composer.</p>
                {/if}
              </div>
            </div>
          {/if}
        </div>

        <!-- Mode pill (Claude Code native threads) -->
        {#if agent === "claude"}
          <div class="relative" data-mavi-start-pop={`${uid}-mode`}>
            <button
              type="button"
              bind:this={modeTriggerEl}
              class="gp-chip cursor-pointer px-2.5 py-1 text-xs gap-1.5"
              data-tone={mode === "bypass" ? "danger" : "neutral"}
              aria-expanded={openPopover === "mode"}
              aria-haspopup="dialog"
              title={modeInfo.hint}
              data-testid="mavi-start-mode-pill"
              onclick={() => togglePopover("mode")}
            >
              <ModeIcon size={12} class={mode === "bypass" ? "text-danger" : "text-textMuted"} aria-hidden="true" />
              <span class={mode === "bypass" ? "text-danger font-medium" : "text-textPrimary"}>{modeInfo.label}</span>
              <ChevronDown size={11} class="text-textMuted" aria-hidden="true" />
            </button>

            {#if openPopover === "mode"}
              <div
                use:portal={"body"}
                use:popover={popoverConfig("mode", modeTriggerEl)}
                in:popupEnter
                out:popoverExit
                data-mavi-start-pop={`${uid}-mode`}
                class="mavi-start-popover fixed gp-menu w-80"
                style:z-index={LAYERS.MENU}
                role="dialog"
                aria-label="Permission mode"
              >
                <div class="p-1.5 flex flex-col gap-0.5">
                  {#each MAVI_MODES as m (m.id)}
                    {@const Icon = MODE_ICONS[m.id]}
                    <button
                      type="button"
                      class="gp-menu-item items-start py-2"
                      class:bg-surfaceHover={mode === m.id}
                      onclick={() => {
                        mode = m.id;
                        openPopover = null;
                      }}
                    >
                      <Icon
                        size={13}
                        class="mt-0.5 shrink-0 {m.id === 'bypass' ? 'text-danger' : mode === m.id ? 'text-accent' : 'text-textMuted'}"
                        aria-hidden="true"
                      />
                      <span class="min-w-0 flex-1 flex flex-col gap-0.5">
                        <span class="text-xs font-medium {m.id === 'bypass' ? 'text-danger' : 'text-textPrimary'}">{m.label}</span>
                        <span class="text-micro text-textMuted leading-snug">{m.hint}</span>
                      </span>
                      {#if mode === m.id}
                        <Check size={12} class="mt-0.5 text-accent shrink-0" aria-hidden="true" />
                      {/if}
                    </button>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
        {/if}
      </div>

      <div class="flex items-center gap-1.5 shrink-0">
        <button
          type="button"
          class="gp-icon-btn"
          data-size="sm"
          aria-label="Insert / command"
          title="Commands (/)"
          onmousedown={(e) => e.preventDefault()}
          onclick={() => void insertTrigger("/")}
        >
          <Slash size={12} aria-hidden="true" />
        </button>
        <button
          type="button"
          class="gp-icon-btn"
          data-size="sm"
          aria-label="Mention @ file"
          title="Files (@)"
          onmousedown={(e) => e.preventDefault()}
          onclick={() => void insertTrigger("@")}
        >
          <AtSign size={12} aria-hidden="true" />
        </button>
        <button
          type="button"
          class="gp-icon-btn"
          data-size="sm"
          aria-label="Pick & agent or model"
          title="Agents & models (&)"
          onmousedown={(e) => e.preventDefault()}
          onclick={() => void insertTrigger("&")}
        >
          <span class="text-xs font-mono leading-none">&amp;</span>
        </button>

        {#if onCancel}
          <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={onCancel}>
            Cancel
          </button>
        {/if}
        <button type="submit" class="gp-btn" data-variant="primary" data-size="md" disabled={!ready || starting}>
          {#if starting}<LoaderCircle size={12} class="animate-spin" aria-hidden="true" />{/if}
          <span>Start</span>
        </button>
        <span class="gp-keycap">{sendKeyLabel}</span>
      </div>
    </div>
  </div>

  {#if error}
    <p class="gp-notice gp-notice-error text-meta w-full mx-0" role="alert">{error}</p>
  {/if}

  {#if where === "ssh" && agent === "claude"}
    <div class="mavi-ssh w-full flex flex-col gap-2.5 px-1">
      <div class="mavi-ssh-fields grid gap-2.5">
        <label class="grid gap-1 min-w-0">
          <span class="gp-label">Host</span>
          <input
            class="gp-field w-full"
            list="mavi-ssh-hosts"
            bind:value={host}
            placeholder="devbox or me@devbox.lan"
            autocomplete="off"
            spellcheck="false"
          />
          <datalist id="mavi-ssh-hosts">
            {#each hosts as h (h)}<option value={h}></option>{/each}
          </datalist>
        </label>
        <label class="grid gap-1 min-w-0">
          <span class="gp-label">Directory on the host</span>
          <input class="gp-field font-mono w-full" bind:value={dir} placeholder="~/code/app" spellcheck="false" />
        </label>
      </div>
      <div class="flex flex-wrap items-center gap-2.5">
        <button type="button" class="gp-btn" disabled={!host.trim() || probing} onclick={check}>
          {#if probing}<LoaderCircle size={12} class="animate-spin" aria-hidden="true" />{/if} Check connection
        </button>
        {#if probe}
          <span class="text-meta inline-flex items-center gap-1" class:text-ok={probeOk} class:text-danger={!probeOk} role="status">
            {#if probeOk}<CircleCheck size={12} aria-hidden="true" />{:else}<TriangleAlert size={12} aria-hidden="true" />{/if}
            {probe.message}
            <span class="text-textMuted">· {probe.millis} ms</span>
          </span>
        {/if}
      </div>
      <p class="text-micro text-textMuted leading-relaxed">
        Uses your ssh config, keys and known hosts. The agent runs on the host for as long as this connection lasts; if it drops, resume the thread to continue the conversation.
      </p>
    </div>
  {/if}

  {#if agent === "agy"}
    <div class="w-full flex flex-col items-center gap-2">
      <p class="text-meta text-textMuted flex items-center gap-1.5">
        <LauncherIcon kind="agy" size={12} />
        <span>Starts an Antigravity session in the terminal dock</span>
      </p>
      {#if agyModelDiffersFromDefault}
        <div class="w-full rounded-xl bg-background/50 px-3 py-2 flex flex-col gap-1.5">
          <Checkbox
            label="Save as Antigravity default before opening"
            description={model.trim()
              ? `Updates Settings → Agents so Antigravity starts with ${model.trim()}.`
              : "Clears Antigravity's saved model override in Settings → Agents."}
            checked={applyAgyModelAsDefault}
            onchange={(next) => (applyAgyModelAsDefault = next)}
          />
          {#if !applyAgyModelAsDefault}
            <p class="gp-notice text-meta mx-0" data-tone="warn" role="status">
              Antigravity reads its model from Settings → Agents ({defaultAgyModel || "Default"}); this choice won't apply unless saved as default.
            </p>
          {/if}
        </div>
      {/if}
    </div>
  {/if}

  {#if mode === "bypass" && agent === "claude"}
    <div class="gp-notice w-full mx-0" data-tone="danger">
      <Checkbox
        label="I understand the agent will run any command and edit any file without asking."
        checked={acknowledged}
        onchange={(next) => (acknowledged = next)}
      />
    </div>
  {/if}

  {#if terminalAgentsHere > 0 && !newWorktree && (where === "local" || agent === "agy")}
    <div class="gp-notice text-meta w-full mx-0" data-tone="warn" role="status">
      {terminalAgentsHere === 1 ? "An agent terminal is" : `${terminalAgentsHere} agent terminals are`} running in this checkout.
      Its edits would show in this thread as other changes; a new worktree keeps them apart.
    </div>
  {/if}

  {#if message.length === 0 && !prefill}
    <div class="flex flex-wrap justify-center gap-2" aria-label="Quick-start prompts">
      {#each QUICK_PROMPTS as prompt (prompt)}
        <button
          type="button"
          class="gp-chip cursor-pointer px-3 py-1 text-meta hover:text-textPrimary"
          data-tone="neutral"
          onclick={() => void pickQuickPrompt(prompt)}
        >
          {prompt}
        </button>
      {/each}
    </div>
  {/if}
</form>

<style>
  .mavi-start-shell {
    max-width: var(--mavi-lane-max, 46rem);
  }
  .mavi-hero {
    padding: 14px 16px 12px;
  }
  .mavi-hero-input {
    display: block;
    width: 100%;
    min-height: 112px;
    resize: none;
    background: transparent;
    border: 0;
    outline: none;
    padding: 2px 2px 8px;
    -webkit-text-fill-color: rgb(var(--c-text));
  }
  .mavi-hero-input::placeholder {
    color: rgb(var(--c-text-faint));
    -webkit-text-fill-color: rgb(var(--c-text-faint));
  }
  .mavi-start-popover {
    max-height: min(72vh, 26rem);
    overflow-y: auto;
  }
  .mavi-ssh {
    container-type: inline-size;
  }
  .mavi-ssh-fields {
    grid-template-columns: minmax(0, 1fr);
  }
  @container (min-width: 420px) {
    .mavi-ssh-fields {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }
</style>
