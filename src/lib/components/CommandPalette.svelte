<script lang="ts">
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { fade, scale } from "svelte/transition";
  import { repoStore } from "../stores/repoStore";
  import { graphStore } from "../stores/graphStore";
  import { isCaseInsensitiveFs, displayName, isPathAmong } from "../repos/paths";
  import type { ViewTab } from "../repos/persist";
  import { VIEW_REGISTRY, type ViewRegistration } from "../views/viewRegistry";
  import { themeStore } from "../stores/themeStore";
  import { interfaceStore } from "../stores/interfaceStore";
  import { askText, promptState } from "../stores/modalStore";
  import { promptQuickCommit } from "../commit/quickCommit";
  import {
    backdropFade,
    backdropFadeOut,
    cardScale,
    cardScaleOut,
  } from "../ui/transitions";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import {
    GitBranch,
    GitCommit,
    Moon,
    RefreshCw,
    Plus,
    Search,
    Download,
    Upload,
    Layers,
    Percent,
    ShieldAlert,
    FolderOpen,
    FolderGit2,
    X,
    Bug,
    Terminal,
    CircleUserRound,
    FileCode,
    Keyboard,
  } from "lucide-svelte";

  let isOpen = $state(false);
  let query = $state("");
  let highlighted = $state(0);
  let inputEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();

  const FRECENCY_KEY = "gitpulse_palette_frecency";

  function readFrecency(): Record<string, number> {
    if (typeof window === "undefined" || !window.localStorage) return {};
    try {
      const raw = window.localStorage.getItem(FRECENCY_KEY);
      return raw ? JSON.parse(raw) : {};
    } catch {
      return {};
    }
  }

  function recordFrecency(id: string) {
    if (typeof window === "undefined" || !window.localStorage) return;
    try {
      const frecency = readFrecency();
      frecency[id] = (frecency[id] ?? 0) + 1;
      window.localStorage.setItem(FRECENCY_KEY, JSON.stringify(frecency));
    } catch {
      /* ignore quota errors */
    }
  }

  // Keyboard navigation keeps the highlighted row visible.
  $effect(() => {
    void highlighted;
    listEl
      ?.querySelector('[data-highlighted="true"]')
      ?.scrollIntoView({ block: "nearest" });
  });

  const VIEW_COMMAND_ICONS: Partial<Record<ViewTab, typeof ShieldAlert>> = {
    files: FileCode,
    terminal: Terminal,
    manvi: ShieldAlert,
    github: GitBranch,
    coverage: Percent,
    health: ShieldAlert,
    reflog: Search,
  };

  const viewCommands = Object.values(VIEW_REGISTRY)
    .filter((view): view is ViewRegistration & { paletteCommand: string } =>
      Boolean(view.paletteCommand)
    )
    .map((view) => ({
      id: view.id,
      label: view.paletteCommand,
      icon: VIEW_COMMAND_ICONS[view.id] ?? GitBranch,
      shortcut: undefined as string | undefined,
      action: () => repoStore.setActiveTab(view.id),
    }));

  const commands = [
    {
      id: "refresh",
      label: "Refresh Repository Status",
      icon: RefreshCw,
      shortcut: "⌘R",
      action: () => repoStore.refresh(),
    },
    {
      id: "shortcuts",
      label: "Keyboard Shortcuts Cheat Sheet",
      icon: Keyboard,
      shortcut: "?",
      action: () => window.dispatchEvent(new CustomEvent("gitpulse:shortcuts")),
    },
    {
      id: "theme",
      label: "Toggle Dark / Light Theme",
      icon: Moon,
      shortcut: undefined,
      action: () => themeStore.toggle(),
    },
    {
      id: "toggle_author_avatars",
      label: "Toggle Author Avatars",
      icon: CircleUserRound,
      shortcut: undefined,
      action: () => interfaceStore.toggleGraphAvatars(),
    },
    {
      id: "theme_system",
      label: "Use System Appearance",
      icon: Moon,
      shortcut: undefined,
      action: () => themeStore.setPreference("system"),
    },
    {
      id: "new_branch",
      label: "Create New Branch…",
      icon: Plus,
      shortcut: undefined,
      action: async () => {
        const name = await askText({
          title: "Create New Branch",
          message: "New branch name",
          placeholder: "feat/name",
          confirmLabel: "Create",
        });
        if (name?.trim()) repoStore.createBranch(name.trim());
      },
    },
    {
      id: "rename_branch",
      label: "Rename Current Branch…",
      icon: GitBranch,
      shortcut: undefined,
      action: async () => {
        const current = $repoStore.currentBranch;
        if (!current) return;
        const name = await askText({
          title: "Rename branch",
          message: current,
          initialValue: current,
          confirmLabel: "Rename",
        });
        if (name?.trim() && name.trim() !== current) repoStore.renameBranch(current, name.trim());
      },
    },
    { id: "fetch", label: "Fetch All Remotes", icon: Download, shortcut: undefined, action: () => repoStore.fetch() },
    { id: "pull", label: "Pull (fast-forward)", icon: Download, shortcut: undefined, action: () => repoStore.pull() },
    { id: "push", label: "Push Current Branch", icon: Upload, shortcut: undefined, action: () => repoStore.push() },
    ...viewCommands,
    { id: "stash", label: "Stash Working Tree", icon: Layers, shortcut: undefined, action: () => repoStore.stashSave() },
    { id: "stash_pop", label: "Pop Stash", icon: Layers, shortcut: undefined, action: () => repoStore.stashPop() },
    {
      id: "quick_commit",
      label: "Quick Commit…",
      icon: GitCommit,
      shortcut: "⌘Enter",
      action: () => void promptQuickCommit(),
    },
    {
      id: "diagnostics",
      label: "Open Diagnostics",
      icon: Bug,
      shortcut: undefined,
      action: () => window.dispatchEvent(new CustomEvent("gitpulse:diagnostics")),
    },
  ];

  let repoCommands = $derived([
    { id: "open_repo", label: "Open Repository…", icon: FolderOpen, shortcut: "⌘T", action: () => repoStore.pickAndOpenRepo() },
    { id: "close_tab", label: "Close Repository Tab", icon: X, shortcut: "⌘⇧W", action: () => void repoStore.closeActiveTab() },
    { id: "next_tab", label: "Next Repository Tab", icon: FolderGit2, shortcut: "Ctrl+Tab", action: () => void repoStore.nextTab() },
    { id: "prev_tab", label: "Previous Repository Tab", icon: FolderGit2, shortcut: "Ctrl+⇧+Tab", action: () => void repoStore.prevTab() },
    { id: "reopen_tab", label: "Reopen Closed Repository", icon: FolderGit2, shortcut: undefined, action: () => void repoStore.reopenLastClosed() },
    ...$repoStore.openTabs.map((tab) => ({
      id: `switch:${tab.id}`,
      label: `Switch to ${tab.label}`,
      icon: FolderGit2,
      shortcut: undefined,
      action: () => void repoStore.activateTab(tab.id),
    })),
    ...$repoStore.recentRepos
      .filter((path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), { caseInsensitive: isCaseInsensitiveFs() }))
      .map((path) => ({
        id: `recent:${path}`,
        label: `Open recent ${displayName(path)}`,
        icon: FolderGit2,
        shortcut: undefined,
        action: () => void repoStore.openRepo(path),
      })),
  ]);

  interface PaletteItem {
    id: string;
    label: string;
    icon: any;
    shortcut?: string;
    category?: string;
    action: () => void;
  }

  let mode = $derived.by<"commands" | "commits" | "branches" | "help">(() => {
    const trimmed = query.trim();
    if (trimmed.startsWith("#")) return "commits";
    if (trimmed.startsWith("@")) return "branches";
    if (trimmed.startsWith("?")) return "help";
    return "commands";
  });

  let effectiveSearchText = $derived.by(() => {
    const trimmed = query.trim();
    if (trimmed.startsWith(">") || trimmed.startsWith("#") || trimmed.startsWith("@") || trimmed.startsWith("?")) {
      return trimmed.slice(1).trim();
    }
    return trimmed;
  });

  let allAvailableItems = $derived.by<PaletteItem[]>(() => {
    const currentMode = mode;
    const search = effectiveSearchText.toLowerCase();

    if (currentMode === "commits") {
      // Commit Search Mode
      return $graphStore.rows
        .filter((r) => r.id.toLowerCase().includes(search) || r.summary.toLowerCase().includes(search))
        .slice(0, 30)
        .map((r) => ({
          id: `commit:${r.id}`,
          label: `${r.id.slice(0, 7)} — ${r.summary}`,
          icon: GitCommit,
          category: "Commit",
          action: () => {
            repoStore.selectCommitDiff(r.id);
            repoStore.setActiveTab("history");
          },
        }));
    }

    if (currentMode === "branches") {
      // Branch Jump Mode
      return $repoStore.branches
        .filter((b) => b.name.toLowerCase().includes(search))
        .map((b) => ({
          id: `branch:${b.name}`,
          label: b.name,
          icon: GitBranch,
          category: b.is_remote ? "Remote Branch" : "Local Branch",
          action: () => {
            if (!b.is_current) repoStore.checkoutBranch(b.name);
          },
        }));
    }

    if (currentMode === "help") {
      return [
        {
          id: "help_shortcuts",
          label: "View All Keyboard Shortcuts",
          icon: Keyboard,
          shortcut: "?",
          action: () => window.dispatchEvent(new CustomEvent("gitpulse:shortcuts")),
        },
        {
          id: "help_commits",
          label: "Type # to search and jump to commits",
          icon: GitCommit,
          action: () => { query = "#"; },
        },
        {
          id: "help_branches",
          label: "Type @ to jump between branches",
          icon: GitBranch,
          action: () => { query = "@"; },
        },
      ];
    }

    // Default: General Commands & Repos with Frecency
    const frecency = readFrecency();
    const list: PaletteItem[] = [...repoCommands, ...commands];
    return list
      .filter((c) => c.label.toLowerCase().includes(search))
      .sort((a, b) => {
        if (!search) {
          const scoreA = frecency[a.id] ?? 0;
          const scoreB = frecency[b.id] ?? 0;
          if (scoreA !== scoreB) return scoreB - scoreA;
        }
        return 0;
      });
  });

  let filteredCommands = $derived(allAvailableItems);

  $effect(() => {
    if (highlighted > 0 && highlighted >= filteredCommands.length) {
      highlighted = Math.max(0, filteredCommands.length - 1);
    }
  });

  $effect(() => {
    if (isOpen) {
      inputEl?.focus();
      inputEl?.select();
    }
  });

  function run(index: number) {
    const cmd = filteredCommands[index];
    if (!cmd) return;
    recordFrecency(cmd.id);
    cmd.action();
    isOpen = false;
  }

  function modalOccupied(): boolean {
    return get(promptState) !== null;
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (isImeComposition(e)) return;
    if ((e.metaKey || e.ctrlKey) && e.key === "k") {
      if (modalOccupied()) return;
      e.preventDefault();
      isOpen = !isOpen;
      query = "";
      highlighted = 0;
    } else if (e.key === "Escape" && isOpen) {
      isOpen = false;
    } else if (isOpen && e.key === "ArrowDown") {
      e.preventDefault();
      highlighted = Math.min(highlighted + 1, Math.max(0, filteredCommands.length - 1));
    } else if (isOpen && e.key === "ArrowUp") {
      e.preventDefault();
      highlighted = Math.max(highlighted - 1, 0);
    } else if (isOpen && e.key === "Enter") {
      e.preventDefault();
      run(highlighted);
    }
  }

  onMount(() => {
    window.addEventListener("keydown", handleKeyDown);
    const openPalette = () => {
      if (modalOccupied()) return;
      isOpen = true;
      query = "";
      highlighted = 0;
    };
    window.addEventListener("gitpulse:palette", openPalette);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("gitpulse:palette", openPalette);
    };
  });

  function highlightMatches(text: string, search: string): { text: string; match: boolean }[] {
    if (!search) return [{ text, match: false }];
    const idx = text.toLowerCase().indexOf(search.toLowerCase());
    if (idx === -1) return [{ text, match: false }];
    const parts: { text: string; match: boolean }[] = [];
    if (idx > 0) parts.push({ text: text.slice(0, idx), match: false });
    parts.push({ text: text.slice(idx, idx + search.length), match: true });
    if (idx + search.length < text.length) {
      parts.push({ text: text.slice(idx + search.length), match: false });
    }
    return parts;
  }
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onclick={() => (isOpen = false)}
    onkeydown={(e) => e.key === "Escape" && (isOpen = false)}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm flex items-start justify-center pt-24 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL}"
  >
    <!-- Modal Card -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      use:trapFocus={{ initial: () => inputEl ?? null }}
      onclick={(e) => e.stopPropagation()}
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-lg gp-card shadow-float rounded-2xl overflow-hidden flex flex-col gp-gpu bg-surface border border-border/80"
    >
      <div class="p-3.5 border-b border-border/60 flex items-center gap-2.5 bg-surface">
        <Search size={16} class="text-accent shrink-0" />
        <input
          bind:this={inputEl}
          type="text"
          bind:value={query}
          placeholder="Type a command or #commit, @branch, ?help..."
          class="w-full bg-transparent text-textPrimary placeholder:text-textMuted text-sm focus:outline-none"
          role="combobox"
          aria-expanded="true"
          aria-autocomplete="list"
          aria-controls="command-palette-listbox"
          aria-activedescendant={filteredCommands.length > 0
            ? `palette-option-${highlighted}`
            : undefined}
        />
        {#if mode !== "commands"}
          <span class="gp-chip text-[10px] uppercase font-bold text-accent border-accent/40 bg-accent/10 shrink-0">
            {mode}
          </span>
        {/if}
      </div>

      <div
        bind:this={listEl}
        id="command-palette-listbox"
        class="max-h-72 overflow-y-auto p-1.5"
        role="listbox"
        aria-label="Commands"
      >
        {#each filteredCommands as cmd, i (cmd.id)}
          {@const parts = highlightMatches(cmd.label, effectiveSearchText)}
          <button
            id={`palette-option-${i}`}
            onclick={() => run(i)}
            role="option"
            aria-selected={i === highlighted}
            aria-label={cmd.label}
            data-highlighted={i === highlighted ? "true" : "false"}
            class="w-full px-3 py-2 text-left rounded-xl text-xs flex items-center justify-between gap-3 transition-colors {i === highlighted ? 'bg-surfaceHover ring-1 ring-accent/25' : 'hover:bg-surfaceHover'}"
          >
            <div class="flex items-center gap-2.5 min-w-0 flex-1">
              <span class="flex items-center justify-center w-6 h-6 rounded-lg bg-background/80 shrink-0 {i === highlighted ? 'text-accent' : 'text-textMuted'}">
                <cmd.icon size={14} />
              </span>
              <span class="truncate">
                {#each parts as part}
                  {#if part.match}
                    <b class="text-accent font-semibold">{part.text}</b>
                  {:else}
                    <span>{part.text}</span>
                  {/if}
                {/each}
              </span>
            </div>

            {#if cmd.shortcut}
              <kbd class="gp-keycap shrink-0">{cmd.shortcut}</kbd>
            {/if}
          </button>
        {/each}
        {#if filteredCommands.length === 0}
          <div class="px-3 py-4 text-xs text-textMuted text-center" role="status">
            No matching {mode === "commands" ? "commands" : mode}
          </div>
        {/if}
      </div>

      <!-- Footer Hints -->
      <div class="px-3 py-1.5 bg-background/60 border-t border-border/60 flex items-center justify-between text-[10px] text-textMuted select-none">
        <div class="flex items-center gap-3">
          <span><kbd class="gp-keycap font-mono text-[9px]">↑↓</kbd> Navigate</span>
          <span><kbd class="gp-keycap font-mono text-[9px]">↵</kbd> Select</span>
          <span><kbd class="gp-keycap font-mono text-[9px]">Esc</kbd> Close</span>
        </div>
        <div class="flex items-center gap-2">
          <span>Prefixes: <b class="font-mono">#</b> commits <b class="font-mono">@</b> branches</span>
        </div>
      </div>
    </div>
  </div>
{/if}
