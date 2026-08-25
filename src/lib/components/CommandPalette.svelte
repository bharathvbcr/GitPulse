<script lang="ts">
  import { onMount } from "svelte";
  import { fade, scale } from "svelte/transition";
  import { repoStore } from "../stores/repoStore";
  import { isCaseInsensitiveFs, isPathAmong } from "../repos/paths";
  import type { ViewTab } from "../repos/persist";
  import { VIEW_REGISTRY, type ViewRegistration } from "../views/viewRegistry";
  import { themeStore } from "../stores/themeStore";
  import { askConfirm, askText } from "../stores/modalStore";
  import { fadeParams, scaleParams } from "../motion/easing";
  import { isImeComposition } from "../keyboard/imeGuard";
  import { GitBranch, Moon, RefreshCw, Plus, Search, Download, Upload, Layers, Percent, ShieldAlert, FolderOpen, FolderGit2, X } from "lucide-svelte";

  let isOpen = $state(false);
  let query = $state("");
  let highlighted = $state(0);
  let inputEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();

  // Keyboard navigation must keep the highlighted row visible; without this,
  // ArrowDown past the fold selects an off-screen command.
  $effect(() => {
    void highlighted;
    listEl
      ?.querySelector('[data-highlighted="true"]')
      ?.scrollIntoView({ block: "nearest" });
  });

  // View-opening commands derive from the view registry: registering a view
  // with a paletteCommand is all it takes to appear here.
  const VIEW_COMMAND_ICONS: Partial<Record<ViewTab, typeof ShieldAlert>> = {
    manvi: ShieldAlert,
    github: GitBranch,
    coverage: Percent,
    health: ShieldAlert,
    reflog: Search,
  };
  const viewCommands = Object.values(VIEW_REGISTRY)
    .filter((view): view is ViewRegistration & { paletteCommand: string } =>
      Boolean(view.paletteCommand))
    .map((view) => ({
      id: view.id,
      label: view.paletteCommand,
      icon: VIEW_COMMAND_ICONS[view.id] ?? GitBranch,
      action: () => repoStore.setActiveTab(view.id),
    }));

  const commands = [
    { id: "refresh", label: "Refresh Repository Status", icon: RefreshCw, action: () => repoStore.refresh() },
    { id: "theme", label: "Toggle Dark / Light Theme", icon: Moon, action: () => themeStore.toggle() },
    { id: "theme_system", label: "Use System Appearance", icon: Moon, action: () => themeStore.setPreference("system") },
    {
      id: "new_branch",
      label: "Create New Branch…",
      icon: Plus,
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
    { id: "fetch", label: "Fetch All Remotes", icon: Download, action: () => repoStore.fetch() },
    { id: "pull", label: "Pull (fast-forward)", icon: Download, action: () => repoStore.pull() },
    { id: "push", label: "Push Current Branch", icon: Upload, action: () => repoStore.push() },
    ...viewCommands,
    { id: "stash", label: "Stash Working Tree", icon: Layers, action: () => repoStore.stashSave() },
    {
      id: "stash_pop",
      label: "Pop Stash",
      icon: Layers,
      // Popping drops the stash entry even when applying conflicts; same
      // confirm-before-destructive pattern as BranchList's delete.
      action: async () => {
        const ok = await askConfirm({
          title: "Pop Stash",
          message:
            "Apply the most recent stash and remove it from the stash list? Conflicted files keep their local state, but the stash entry is dropped.",
          confirmLabel: "Pop",
        });
        if (ok) repoStore.stashPop();
      },
    },
  ];

  let repoCommands = $derived([
    { id: "open_repo", label: "Open Repository…", icon: FolderOpen, action: () => repoStore.pickAndOpenRepo() },
    { id: "close_tab", label: "Close Repository Tab", icon: X, action: () => void repoStore.closeActiveTab() },
    { id: "next_tab", label: "Next Repository Tab", icon: FolderGit2, action: () => void repoStore.nextTab() },
    { id: "prev_tab", label: "Previous Repository Tab", icon: FolderGit2, action: () => void repoStore.prevTab() },
    { id: "reopen_tab", label: "Reopen Closed Repository", icon: FolderGit2, action: () => void repoStore.reopenLastClosed() },
    ...$repoStore.openTabs.map((tab) => ({
      id: `switch:${tab.id}`,
      label: `Switch to ${tab.label}`,
      icon: FolderGit2,
      action: () => void repoStore.activateTab(tab.id),
    })),
    ...$repoStore.recentRepos
      .filter((path) => !isPathAmong(path, $repoStore.openTabs.map((tab) => tab.path), { caseInsensitive: isCaseInsensitiveFs() }))
      .map((path) => ({
        id: `recent:${path}`,
        label: `Open recent ${path.split(/[\\/]/).pop() ?? path}`,
        icon: FolderGit2,
        action: () => void repoStore.openRepo(path),
      })),
  ]);

  let filteredCommands = $derived(
    [...repoCommands, ...commands].filter((c) => c.label.toLowerCase().includes(query.toLowerCase()))
  );

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
    cmd.action();
    isOpen = false;
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (isImeComposition(e)) return;
    if ((e.metaKey || e.ctrlKey) && e.key === "k") {
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
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    tabindex="-1"
    onclick={() => (isOpen = false)}
    onkeydown={(e) => e.key === "Escape" && (isOpen = false)}
    transition:fade={fadeParams()}
    class="fixed inset-0 bg-black/40 backdrop-blur-sm z-50 flex items-start justify-center pt-24 select-none gp-gpu"
  >
    <!-- Modal Card -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      onclick={(e) => e.stopPropagation()}
      in:scale={scaleParams()}
      out:scale={scaleParams()}
      class="w-full max-w-lg gp-card shadow-float rounded-2xl overflow-hidden flex flex-col gp-gpu"
    >
      <div class="p-3.5 border-b border-border/60 flex items-center gap-2.5">
        <Search size={16} class="text-accent" />
        <input
          bind:this={inputEl}
          type="text"
          bind:value={query}
          placeholder="Type a command or search..."
          class="w-full bg-transparent text-textPrimary placeholder:text-textMuted text-sm focus:outline-none"
        />
      </div>

      <div bind:this={listEl} class="max-h-72 overflow-y-auto p-1.5" role="listbox" aria-label="Commands">
        {#each filteredCommands as cmd, i}
          <button
            onclick={() => run(i)}
            role="option"
            aria-selected={i === highlighted}
            data-highlighted={i === highlighted ? "true" : "false"}
            class="w-full px-3 py-2 text-left rounded-xl text-xs flex items-center gap-3 transition-colors {i === highlighted ? 'bg-surfaceHover ring-1 ring-accent/25' : 'hover:bg-surfaceHover'}"
          >
            <span class="flex items-center justify-center w-6 h-6 rounded-lg bg-background/80 shrink-0 {i === highlighted ? 'text-accent' : 'text-textMuted'}">
              <cmd.icon size={14} />
            </span>
            <span class="flex-1">{cmd.label}</span>
          </button>
        {/each}
      </div>
    </div>
  </div>
{/if}

{#if $repoStore.pendingMutation}
  <!-- Menu-driven mutations have no busy indicator of their own; this thin
       strip (same treatment as CommitTable's load bar) shows the work. -->
  <div class="fixed top-0 inset-x-0 h-0.5 z-[60] overflow-hidden pointer-events-none" role="status">
    <span class="sr-only">{$repoStore.pendingMutation}…</span>
    <div class="h-full w-1/3 bg-accent animate-[gp-slide_1.2s_ease-in-out_infinite]"></div>
  </div>
{/if}
