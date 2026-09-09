<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { get } from "svelte/store";
  import { fade, scale } from "svelte/transition";
  import { FileCode, FolderGit2, GitBranch, GitCommit, Search, X, ArrowRight, LoaderCircle } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { promptState } from "../stores/modalStore";
  import { sameRepo, isCaseInsensitiveFs, displayName } from "../repos/paths";
  import { backdropFade, cardScale } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { isMacOS } from "../platform";
  import { highlightMatches } from "../branches/groupBranches";
  import LanguageLogo from "./LanguageLogo.svelte";
  import { openSetupWizard } from "../tools/onboardingStore";
  import { buildCommands, helpCommands, repoUnavailable, worktreeUnavailable, type PaletteHostActions } from "../palette/catalog";
  import { PALETTE_MODES, PAGE_SIZE, MAX_QUERY_LENGTH, parsePaletteQuery, rankItems, readFrecency, recordFrecency, actionFailure, type PaletteItem, type PaletteMode, type Frecency, type PaletteStorage } from "../palette/model";
  import { emptySearch, scheduleSearch, workspaceRoot } from "../palette/search";

  let { openSignal = 0, onClone, onRebase }: { openSignal?: number } & PaletteHostActions = $props();
  let servedSignal = 0;
  let openEpoch = 0;
  let isOpen = $state(false);
  let query = $state("");
  let highlighted = $state(0);
  let page = $state(0);
  let pendingAction = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let retrySignal = $state(0);
  let searchResult = $state(emptySearch());
  let history = $state<Frecency>(new Map());
  let inputEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();

  function storage(): PaletteStorage | null {
    try { return window.localStorage; } catch { return null; }
  }

  let parsed = $derived(parsePaletteQuery(query));
  let mode = $derived.by(() => parsed.mode);
  let effectiveSearchText = $derived(parsed.text);
  let currentMode = $derived(PALETTE_MODES.find(entry => entry.mode === mode) ?? PALETTE_MODES[0]);
  let repoPath = $derived($repoStore.currentPath);
  let commands = $derived(buildCommands($repoStore, changeMode, { onClone, onRebase }));
  let isRemoteSearch = $derived(mode === "files" || mode === "symbols" || mode === "workspace");
  let symbolSearchNote = $derived(mode === "symbols" ? searchResult.note : null);
  let workspaceSearchNote = $derived(mode === "workspace" ? searchResult.note : null);

  // Query text does not reload the file list; filtering operates on the current
  // snapshot. The opening epoch, repository generation and Retry refresh it.
  $effect(() => {
    const open = isOpen;
    const searchMode = mode;
    const path = repoPath;
    const generation = $repoStore.generation;
    const searchText = searchMode === "files" ? "" : effectiveSearchText;
    const semantic = searchMode === "workspace" ? parsed.semantic : false;
    void generation; void retrySignal;
    if (!open || !path || !(searchMode === "files" || searchMode === "symbols" || searchMode === "workspace") || (searchMode !== "files" && !searchText)) {
      searchResult = emptySearch();
      return;
    }
    return scheduleSearch({ mode: searchMode, repoPath: path, text: searchText, semantic }, result => { searchResult = result; });
  });

  function changeMode(next: PaletteMode) {
    query = PALETTE_MODES.find(entry => entry.mode === next)?.prefix ?? ">";
    actionError = null;
    inputEl?.focus();
  }

  function selectFile(path: string) {
    repoStore.selectFilePath(path);
    repoStore.setActiveTab("code", "explorer");
  }

  async function openWorkspaceHit(root: string, filePath: string) {
    const opened = await repoStore.openRepo(root);
    const current = get(repoStore);
    if (!opened || !current.currentPath || !sameRepo(root, current.currentPath, { caseInsensitive: isCaseInsensitiveFs() })) {
      throw Error(current.error || "The target repository could not be opened. No file was selected.");
    }
    selectFile(filePath);
  }

  let allAvailableItems = $derived.by<PaletteItem[]>(() => {
    if (mode === "help") return helpCommands(changeMode);
    if (mode === "repositories") return commands.filter(command => ["Repositories", "Open repositories", "Recent repositories"].includes(command.category));
    if (mode === "files") return searchResult.files.map(path => ({ id: `file:${repoPath}:${path}`, label: path.split("/").pop() ?? path, description: path, filePath: path, category: "Repository files", icon: FileCode, action: () => selectFile(path) }));
    if (mode === "symbols") return searchResult.symbols.map(hit => ({ id: `symbol:${repoPath}:${hit.file_path}:${hit.symbol_name}:${hit.span_start_line}`, label: hit.symbol_name, description: `${hit.kind} · ${hit.file_path}:${hit.span_start_line}`, filePath: hit.file_path, category: "Code Intelligence", icon: FileCode, action: () => selectFile(hit.file_path) }));
    if (mode === "workspace") return searchResult.workspace.map(hit => {
      const root = workspaceRoot(hit.repo, searchResult.repos);
      return { id: `ws:${hit.repo}:${hit.file_path}:${hit.symbol_name}:${hit.span_start_line}`, label: hit.symbol_name, description: `${hit.repo} · ${hit.kind} · ${hit.file_path}:${hit.span_start_line}`, filePath: hit.file_path, category: "Cross-repo symbols", icon: FileCode, disabledReason: root ? undefined : `The workspace registry cannot uniquely resolve ${hit.repo}. Open Map to repair its registration.`, action: () => { if (root) return openWorkspaceHit(root, hit.file_path); } };
    });
    if (mode === "branches") return $repoStore.branches.map(branch => ({ id: `branch:${repoPath}:${branch.name}`, label: branch.name, description: branch.is_current ? "Current branch" : `${branch.last_author} · ${branch.last_summary}`, category: branch.is_remote ? "Remote branches" : "Local branches", icon: GitBranch, disabledReason: branch.is_current ? "This branch is already checked out." : worktreeUnavailable($repoStore), action: () => repoStore.checkoutBranch(branch.name) }));
    if (mode === "commits") {
      if ($graphStore.visiblePath !== repoPath) return [];
      return $graphStore.commits.map(commit => ({ id: `commit:${repoPath}:${commit.id}`, label: commit.summary, description: `${commit.id.slice(0, 8)} · ${commit.author_name}`, keywords: `${commit.id} ${commit.author_email}`, category: "Loaded commits", icon: GitCommit, action: async () => { await repoStore.selectCommitDiff(commit.id); repoStore.setActiveTab("history", "diff"); } }));
    }
    return commands;
  });

  // Remote providers own relevance (including TF-IDF); do not refilter their
  // results by the literal query or reorder them by local usage counts.
  let matchedItems = $derived(mode === "symbols" || mode === "workspace"
    ? allAvailableItems.filter((item, index, items) => items.findIndex(other => other.id === item.id) === index)
    : rankItems(allAvailableItems, effectiveSearchText, history));
  let pageCount = $derived(Math.max(1, Math.ceil(matchedItems.length / PAGE_SIZE)));
  let filteredCommands = $derived(matchedItems.slice(page * PAGE_SIZE, (page + 1) * PAGE_SIZE));
  let activeCommand = $derived(filteredCommands[highlighted]);
  let requiresRepo = $derived(!["commands", "help", "repositories"].includes(mode));
  let unavailable = $derived(requiresRepo ? repoUnavailable($repoStore) : undefined);
  let searchNote = $derived(symbolSearchNote ?? workspaceSearchNote ?? (mode === "files" ? searchResult.note : null));
  let historyNote = $derived(mode === "commits" ?
    ($graphStore.visiblePath !== repoPath ? "History has not loaded for this repository. Open History to load commits." :
      $graphStore.error ? `History unavailable: ${$graphStore.error}` :
      `${$graphStore.commits.length} loaded commits${$graphStore.hasMore ? "; older commits are available in History" : ""}. ${$graphStore.notices.join(" ")}`) : null);
  let resultSummary = $derived(searchResult.loading && isRemoteSearch ? "Searching…" :
    `${matchedItems.length ? page * PAGE_SIZE + 1 : 0}–${Math.min((page + 1) * PAGE_SIZE, matchedItems.length)} of ${matchedItems.length} results`);

  $effect(() => {
    void query; void repoPath;
    page = 0; highlighted = 0; actionError = null;
  });
  $effect(() => {
    if (page >= pageCount) page = pageCount - 1;
    if (highlighted >= filteredCommands.length) highlighted = Math.max(0, filteredCommands.length - 1);
  });
  $effect(() => {
    void highlighted; void filteredCommands;
    listEl?.querySelector('[data-highlighted="true"]')?.scrollIntoView({ block: "nearest" });
  });

  function close() { isOpen = false; }

  async function run(index: number) {
    const command = filteredCommands[index];
    if (!command || command.disabledReason || pendingAction) return;
    const epoch = openEpoch;
    pendingAction = command.id;
    actionError = null;
    try {
      if (command.closeBefore) { close(); await tick(); }
      const result = await command.action();
      const failure = actionFailure(result);
      if (failure) {
        if (isOpen && openEpoch === epoch) actionError = failure;
        return;
      }
      if (!command.keepOpen) history = recordFrecency(history, command.id, storage());
      if (!command.keepOpen && openEpoch === epoch) close();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (isOpen && openEpoch === epoch) actionError = message;
      else repoStore.setError(message);
    } finally { pendingAction = null; }
  }

  function modalOccupied(): boolean {
    return get(promptState) !== null || Boolean(document.querySelector('[aria-modal="true"]:not([data-command-palette])'));
  }

  function requestOpen() {
    if (modalOccupied()) return;
    openEpoch += 1;
    history = readFrecency(storage());
    query = ""; page = 0; highlighted = 0; actionError = null;
    isOpen = true;
    inputEl?.focus();
  }

  function changePage(next: number) {
    page = Math.max(0, Math.min(next, pageCount - 1));
    highlighted = 0;
    inputEl?.focus();
  }

  function handleKeyDown(event: KeyboardEvent) {
    if (isImeComposition(event)) return;
    if ((event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey && event.key.toLowerCase() === "k") {
      if (modalOccupied()) return;
      event.preventDefault(); event.stopImmediatePropagation();
      if (isOpen) close(); else requestOpen();
      return;
    }
    if (!isOpen) return;
    if (event.key === "Escape") {
      event.preventDefault(); event.stopImmediatePropagation(); close(); return;
    }
    // Buttons (mode chips, Retry, paging) keep native Enter/Space behavior.
    if (event.target !== inputEl || event.metaKey || event.ctrlKey || event.altKey) return;
    if (!["ArrowDown", "ArrowUp", "Home", "End", "PageDown", "PageUp", "Enter"].includes(event.key)) return;
    event.preventDefault(); event.stopImmediatePropagation();
    if (event.key === "ArrowDown") highlighted = Math.min(highlighted + 1, Math.max(0, filteredCommands.length - 1));
    else if (event.key === "ArrowUp") highlighted = Math.max(0, highlighted - 1);
    else if (event.key === "Home") highlighted = 0;
    else if (event.key === "End") highlighted = Math.max(0, filteredCommands.length - 1);
    else if (event.key === "PageDown") changePage(page + 1);
    else if (event.key === "PageUp") changePage(page - 1);
    else void run(highlighted);
  }

  $effect(() => {
    if (openSignal > servedSignal) {
      servedSignal = openSignal;
      untrack(requestOpen);
    }
  });
  onMount(() => {
    window.addEventListener("keydown", handleKeyDown, true);
    window.addEventListener("gitpulse:palette", requestOpen);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, true);
      window.removeEventListener("gitpulse:palette", requestOpen);
    };
  });
</script>

{#if isOpen}
  <div role="dialog" aria-modal="true" aria-labelledby="command-palette-title" data-command-palette tabindex="-1"
    onclick={event => { if (event.target === event.currentTarget) close(); }}
    onkeydown={event => event.stopPropagation()}
    in:fade={backdropFade()} class="gp-scrim palette-scrim bg-black/40" style="z-index: {LAYERS.MODAL}">
    <!-- Immediate teardown restores focus before a follow-on prompt mounts. -->
    <div use:trapFocus={{ initial: () => inputEl ?? null }} in:scale={cardScale()} class="palette-card gp-card bg-surface text-textPrimary border border-border/80 shadow-float">
      <header class="palette-header">
        <div class="palette-heading"><span class="palette-brand"><Search size={14} /> <h2 id="command-palette-title">Command palette</h2></span>
          <span class="palette-repo" title={repoPath ?? "No repository open"}><FolderGit2 size={12} />{repoPath ? displayName(repoPath) : "Workspace"}</span>
          <button type="button" class="palette-close" onclick={close} aria-label="Close command palette"><X size={15} /></button>
        </div>
        <div class="palette-search"><Search size={20} class="text-accent shrink-0" />
          <input bind:this={inputEl} bind:value={query} type="text" maxlength={MAX_QUERY_LENGTH} autocomplete="off" spellcheck="false"
            placeholder={currentMode.hint} aria-label="Search commands and repositories" role="combobox" aria-expanded="true" aria-autocomplete="list"
            aria-controls="command-palette-listbox" aria-describedby="palette-status palette-detail"
            aria-activedescendant={activeCommand ? `palette-option-${highlighted}` : undefined} />
          {#if query}<button type="button" class="palette-clear" onclick={() => { query = ""; inputEl?.focus(); }} aria-label="Clear search"><X size={14} /></button>{/if}
        </div>
        <nav class="palette-modes" aria-label="Search mode">
          {#each PALETTE_MODES as entry}<button type="button" aria-pressed={mode === entry.mode} title={entry.hint} onclick={() => changeMode(entry.mode)}><span>{entry.label}</span><kbd>{entry.prefix}</kbd></button>{/each}
        </nav>
      </header>
      <div class="palette-meta"><span>{mode === "commands" && !effectiveSearchText && history.size ? "Suggested & recent" : currentMode.label}</span><span id="palette-status" role="status" aria-live="polite">{resultSummary}</span></div>
      {#if unavailable}<div class="palette-notice" role="status">{unavailable}<button type="button" class="gp-btn" onclick={() => changeMode("repositories")}>Choose a repository</button></div>{/if}
      {#if searchNote || historyNote}
        <div class="palette-notice" class:palette-warning={searchResult.failed} role="status">
          <p>{searchNote ?? historyNote}</p>
          {#if isRemoteSearch}
            <button type="button" class="gp-btn" onclick={() => retrySignal += 1} disabled={searchResult.loading}>Retry search</button>
            {#if mode === "symbols"}<button type="button" class="gp-btn" onclick={async () => { close(); await tick(); openSetupWizard("devmap", "explain"); }}>Set up devmap</button>{/if}
          {/if}
          {#if mode === "workspace" || mode === "commits"}<button type="button" class="gp-btn" disabled={!repoPath} onclick={() => { repoStore.setActiveTab(mode === "workspace" ? "code" : "history", mode === "workspace" ? "map" : "graph"); close(); }}>Open {mode === "workspace" ? "Map" : "History"}</button>{/if}
        </div>
      {/if}
      <div bind:this={listEl} id="command-palette-listbox" class="palette-results" role="listbox" aria-label={currentMode.label} aria-busy={searchResult.loading || Boolean(pendingAction)}>
        {#if searchResult.loading && isRemoteSearch}
          <div class="palette-empty" role="status"><LoaderCircle size={22} class="palette-spinner" /><strong>Searching {currentMode.label.toLowerCase()}…</strong><span>Results will appear here.</span></div>
        {:else}
          {#each filteredCommands as cmd, i (cmd.id)}
            <button id={`palette-option-${i}`} type="button" role="option" aria-selected={i === highlighted} aria-disabled={Boolean(cmd.disabledReason) || Boolean(pendingAction)} aria-label={cmd.label} aria-describedby={`palette-description-${i}`}
              tabindex="-1" data-highlighted={i === highlighted ? "true" : "false"} class="palette-option" class:palette-disabled={Boolean(cmd.disabledReason)}
              onclick={() => void run(i)} onpointermove={() => { highlighted = i; }}>
              <span class="palette-icon">{#if pendingAction === cmd.id}<LoaderCircle size={17} class="palette-spinner" />{:else if cmd.filePath}<LanguageLogo filePath={cmd.filePath} size={17} />{:else}<cmd.icon size={17} />{/if}</span>
              <span class="palette-copy"><span class="palette-label">{#each highlightMatches(cmd.label, effectiveSearchText) as part}{#if part.matched}<b>{part.text}</b>{:else}{part.text}{/if}{/each}</span><span class="palette-description" id={`palette-description-${i}`}>{cmd.disabledReason ?? cmd.description ?? cmd.category}</span></span>
              <span class="palette-trailing">{#if cmd.shortcut}<kbd class="gp-keycap">{isMacOS() ? cmd.shortcut : cmd.shortcut.replaceAll("⌘", "Ctrl+").replaceAll("⇧", "Shift+")}</kbd>{:else}<span class="palette-category">{cmd.category}</span>{/if}{#if i === highlighted && !cmd.disabledReason}<ArrowRight size={13} />{/if}</span>
            </button>
          {/each}
          {#if filteredCommands.length === 0}
            <div class="palette-empty"><Search size={24} /><strong>{unavailable ? "Choose a repository to start" : !effectiveSearchText && (mode === "symbols" || mode === "workspace") ? "Start with a symbol name" : searchResult.failed ? "Search needs attention" : "No matching results"}</strong><span>{searchResult.failed ? "Use the recovery actions above to try again." : "Try another search or choose a mode above."}</span></div>
          {/if}
        {/if}
      </div>
      {#if actionError}<div role="alert" class="palette-notice palette-warning">{actionError}</div>{/if}
      <div id="palette-detail" class="palette-detail" title={activeCommand?.description ?? ""}>{pendingAction ? "Action in progress…" : activeCommand?.disabledReason ?? activeCommand?.description ?? currentMode.hint}</div>
      <footer class="palette-footer"><span><kbd>↑↓</kbd> Navigate <kbd>↵</kbd> {activeCommand?.keepOpen ? "Explore" : "Run"} <kbd>Esc</kbd> Close</span>
        {#if pageCount > 1}<span class="palette-pages"><button type="button" aria-label="Previous results" disabled={page === 0} onclick={() => changePage(page - 1)}>←</button><span>{page + 1} / {pageCount}</span><button type="button" aria-label="Next results" disabled={page + 1 >= pageCount} onclick={() => changePage(page + 1)}>→</button></span>{:else}<kbd>{isMacOS() ? "⌘K" : "Ctrl+K"}</kbd>{/if}
      </footer>
    </div>
  </div>
{/if}

<style>
  .palette-scrim { display:flex; align-items:flex-start; justify-content:center; padding: min(12vh, 100px) 16px 20px; }
  .palette-card { width: min(680px, 100%); max-height: calc(100dvh - min(12vh, 100px) - 20px); display:flex; flex-direction:column; overflow:hidden; border-radius:18px; }
  .palette-header { padding:16px 18px 0; flex-shrink:0; }
  .palette-heading,.palette-brand,.palette-repo,.palette-search,.palette-meta,.palette-footer,.palette-pages { display:flex; align-items:center; }
  .palette-heading { gap:12px; color:var(--color-textMuted); font-size:11px; }
  .palette-brand { gap:7px; flex:1; }
  h2 { font-size:11px; font-weight:600; margin:0; }
  .palette-repo { gap:5px; max-width:40%; overflow:hidden; white-space:nowrap; text-overflow:ellipsis; }
  .palette-close,.palette-clear { padding:5px; border-radius:6px; color:var(--color-textMuted); }
  .palette-search { gap:12px; padding:19px 0 17px; }
  input { min-width:0; width:100%; background:transparent; border:0; border-radius:5px; font-size:17px; color:var(--color-textPrimary); }
  input::placeholder { color:var(--color-textMuted); font-size:14px; }
  .palette-modes { display:flex; gap:3px; overflow-x:auto; padding-bottom:12px; }
  .palette-modes button { display:flex; align-items:center; gap:5px; padding:5px 7px; font-size:10px; white-space:nowrap; border-radius:6px; color:var(--color-textMuted); }
  .palette-modes button[aria-pressed="true"] { background:var(--color-surfaceHover); color:var(--color-accent); }
  .palette-modes kbd { opacity:.65; font-size:9px; }
  .palette-meta { justify-content:space-between; padding:10px 18px 7px; border-top:1px solid var(--color-border); font-size:10px; color:var(--color-textMuted); }
  .palette-results { min-height:130px; max-height:390px; overflow-y:auto; overscroll-behavior:contain; padding:0 8px 8px; }
  .palette-option { display:flex; align-items:center; gap:11px; text-align:left; width:100%; padding:10px; border-radius:9px; color:var(--color-textPrimary); }
  .palette-option[data-highlighted="true"] { background:var(--color-surfaceHover); box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--color-accent) 24%, transparent); }
  .palette-icon { display:flex; align-items:center; justify-content:center; flex-shrink:0; width:29px; height:29px; border-radius:8px; background:var(--color-background); color:var(--color-textMuted); }
  .palette-option[data-highlighted="true"] .palette-icon { color:var(--color-accent); }
  .palette-copy { display:flex; flex:1; min-width:0; flex-direction:column; gap:3px; }
  .palette-label { font-size:12px; overflow:hidden; white-space:nowrap; text-overflow:ellipsis; }
  .palette-label b { color:var(--color-accent); font-weight:600; }
  .palette-description { font-size:10px; color:var(--color-textMuted); overflow:hidden; white-space:nowrap; text-overflow:ellipsis; }
  .palette-disabled .palette-label,.palette-disabled .palette-icon { color:var(--color-textMuted); }
  .palette-trailing { display:flex; align-items:center; gap:10px; color:var(--color-textMuted); font-size:9px; }
  .palette-empty { min-height:145px; display:flex; align-items:center; justify-content:center; flex-direction:column; gap:9px; color:var(--color-textMuted); font-size:11px; text-align:center; padding:22px; }
  .palette-empty strong { color:var(--color-textPrimary); font-size:13px; font-weight:500; }
  .palette-notice { padding:9px 18px; color:var(--color-textMuted); font-size:11px; overflow-wrap:anywhere; max-height:110px; overflow-y:auto; flex-shrink:0; }
  .palette-notice p { margin:0 0 6px; }
  .palette-notice button { margin:2px 6px 0 0; font-size:10px; }
  .palette-warning { color:var(--color-textPrimary); border-left:3px solid var(--color-accent); background:var(--color-background); }
  .palette-detail { padding:9px 18px; min-height:34px; font-size:10px; color:var(--color-textMuted); border-top:1px solid var(--color-border); overflow-wrap:anywhere; max-height:75px; overflow-y:auto; flex-shrink:0; }
  .palette-footer { justify-content:space-between; flex-shrink:0; padding:10px 18px; background:var(--color-background); font-size:10px; color:var(--color-textMuted); gap:8px; }
  .palette-footer kbd { margin:0 3px 0 6px; font-size:10px; }
  .palette-pages { gap:8px; }
  .palette-pages button { padding:0 6px; }
  button:focus-visible { outline:2px solid var(--color-accent); outline-offset:-2px; }
  button:disabled { opacity:.4; }
  .palette-card :global(.palette-spinner) { animation:palette-spin 1s linear infinite; }
  @keyframes palette-spin { to { transform:rotate(360deg); } }
  @media (prefers-reduced-motion:reduce) { .palette-card :global(.palette-spinner) { animation:none; } }
  @media (max-width:540px) { .palette-scrim { padding:16px 8px; } .palette-card { max-height:calc(100dvh - 32px); } .palette-category { display:none; } .palette-header { padding:12px 12px 0; } .palette-results { max-height:none; } }
</style>
