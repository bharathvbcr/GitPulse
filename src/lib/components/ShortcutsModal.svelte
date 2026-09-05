<script lang="ts">
  import { onMount } from "svelte";
  import { sectionShortcutRows, viewShortcutRows } from "../views/viewShortcuts";
  import { fade, scale } from "svelte/transition";
  import { backdropFade, backdropFadeOut, cardScale, cardScaleOut } from "../ui/transitions";
  import { trapFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { Keyboard, X, Search, Layers, GitBranch, LayoutGrid, FileDiff } from "lucide-svelte";
  import { isImeComposition } from "../keyboard/imeGuard";

  let {
    isOpen = $bindable(false),
    onClose,
  }: {
    isOpen?: boolean;
    onClose?: () => void;
  } = $props();

  let filterQuery = $state("");

  interface ShortcutItem {
    keys: string[];
    description: string;
  }

  interface ShortcutCategory {
    title: string;
    icon: typeof Keyboard;
    shortcuts: ShortcutItem[];
  }

  const SHORTCUT_CATEGORIES: ShortcutCategory[] = [
    {
      title: "Workspace & Tabs",
      icon: LayoutGrid,
      shortcuts: [
        { keys: ["⌘", "O"], description: "Open Repository…" },
        { keys: ["⌘", "T"], description: "Open Repository… (new tab)" },
        { keys: ["⌘", "⇧", "O"], description: "Clone Repository…" },
        { keys: ["⌘", "⇧", "W"], description: "Close active repository tab" },
        { keys: ["Ctrl", "Tab"], description: "Cycle to next repository tab" },
        { keys: ["Ctrl", "⇧", "Tab"], description: "Cycle to previous repository tab" },
        { keys: ["Ctrl", "Alt", "1–9"], description: "Jump to specific repository tab" },
      ],
    },
    {
      title: "Navigation & Command Palette",
      icon: Search,
      shortcuts: [
        { keys: ["⌘", "K"], description: "Open Command Palette" },
        // Derived, never restated. The hand-written line here promised nine
        // views ("Files, Graph, Diff, Resolve, Blame, Stack, GitHub, Coverage,
        // Health") for months after the app consolidated to four — from the
        // one screen whose entire job is to be right about this.
        ...viewShortcutRows(),
        ...sectionShortcutRows(),
        { keys: ["⌘", "⇧", "F"], description: "Open the Fleet dashboard" },
        { keys: ["Ctrl", "`"], description: "Toggle the terminal dock" },
        { keys: ["⌘", "F"], description: "Search commits — switches to History from Work and other views. In Code it searches the open file, and in History → Diff it searches the diff." },
        { keys: ["?"], description: "Show keyboard shortcuts cheat sheet" },
        { keys: ["⌘", "+"], description: "Zoom in — scales the whole interface" },
        { keys: ["⌘", "-"], description: "Zoom out — scales the whole interface" },
        { keys: ["⌘", "0"], description: "Reset the interface scale to 100%" },
      ],
    },
    {
      title: "Git Operations & Views",
      icon: GitBranch,
      shortcuts: [
        { keys: ["⌘", "R"], description: "Refresh repository status" },
        { keys: ["⌘", "Enter"], description: "Quick commit in composer" },
        { keys: ["Esc"], description: "Close modal / dismiss overlay" },
        { keys: ["↑", "↓"], description: "Navigate commits or branches in list" },
        { keys: ["Enter"], description: "Select highlighted item or run action" },
      ],
    },
    {
      title: "Diff",
      icon: FileDiff,
      shortcuts: [
        { keys: ["⌘", "F"], description: "Find in this diff" },
        { keys: ["F3"], description: "Next match (⇧F3 for previous)" },
        { keys: ["Esc"], description: "Close the find bar" },
        { keys: ["Alt", "↑ / ↓"], description: "Previous / next file in this change" },
        { keys: ["Alt", "PgUp / PgDn"], description: "Previous / next block of changes" },
      ],
    },
    {
      title: "Palette Modes",
      icon: Layers,
      shortcuts: [
        { keys: [">"], description: "Commands mode (default)" },
        { keys: ["#"], description: "Jump to commit by SHA or message" },
        { keys: ["@"], description: "Jump to branch and checkout" },
        { keys: ["?"], description: "Help and shortcuts preview" },
      ],
    },
  ];

  let filteredCategories = $derived.by(() => {
    const q = filterQuery.trim().toLowerCase();
    if (!q) return SHORTCUT_CATEGORIES;
    return SHORTCUT_CATEGORIES.map((cat) => ({
      ...cat,
      shortcuts: cat.shortcuts.filter(
        (s) =>
          s.description.toLowerCase().includes(q) ||
          s.keys.some((k) => k.toLowerCase().includes(q))
      ),
    })).filter((cat) => cat.shortcuts.length > 0);
  });

  function close() {
    isOpen = false;
    filterQuery = "";
    onClose?.();
  }

  function handleKeyDown(e: KeyboardEvent) {
    if (isImeComposition(e)) return;
    const target = e.target as HTMLElement | null;
    const isInput =
      target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable);

    if ((e.key === "?" || ((e.metaKey || e.ctrlKey) && e.key === "/")) && !isInput && !isOpen) {
      e.preventDefault();
      isOpen = true;
    } else if (e.key === "Escape" && isOpen) {
      e.preventDefault();
      close();
    }
  }

  onMount(() => {
    window.addEventListener("keydown", handleKeyDown);
    const openHandler = () => {
      isOpen = true;
    };
    window.addEventListener("gitpulse:shortcuts", openHandler);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      window.removeEventListener("gitpulse:shortcuts", openHandler);
    };
  });
</script>

{#if isOpen}
  <div
    role="dialog"
    aria-modal="true"
    aria-label="Keyboard Shortcuts"
    tabindex="-1"
    onclick={(e) => e.target === e.currentTarget && close()}
    onkeydown={(e) => e.key === "Escape" && close()}
    in:fade={backdropFade()}
    out:fade={backdropFadeOut()}
    class="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center p-4 select-none gp-gpu"
    style="z-index: {LAYERS.MODAL};"
  >
    <!-- Modal Card -->
    <div
      use:trapFocus
      in:scale={cardScale()}
      out:scale={cardScaleOut()}
      class="w-full max-w-2xl max-h-[85vh] gp-card shadow-float rounded-3xl overflow-hidden flex flex-col gp-gpu bg-surface border border-border/80"
    >
      <!-- Header -->
      <div class="p-4 border-b border-border/70 flex items-center justify-between bg-surfaceHover/30">
        <div class="flex items-center gap-2.5">
          <div class="p-2 rounded-xl bg-accent/10 text-accent ring-1 ring-accent/25">
            <Keyboard size={18} />
          </div>
          <div>
            <h2 class="text-sm font-semibold text-textPrimary">Keyboard Shortcuts</h2>
            <p class="text-[11px] text-textMuted">GitPulse navigation and workflow accelerators</p>
          </div>
        </div>

        <div class="flex items-center gap-2">
          <div class="relative">
            <Search size={13} class="absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" />
            <input
              type="text"
              bind:value={filterQuery}
              placeholder="Search shortcuts…"
              class="gp-field !pl-7 !py-1 !text-xs !w-44"
            />
          </div>
          <button
            type="button"
            onclick={close}
            aria-label="Close shortcuts modal"
            class="gp-icon-btn"
          >
            <X size={15} />
          </button>
        </div>
      </div>

      <!-- Content -->
      <div class="p-5 overflow-y-auto max-h-[60vh] space-y-6">
        {#each filteredCategories as category (category.title)}
          {@const CatIcon = category.icon}
          <div>
            <div class="flex items-center gap-2 text-xs font-semibold text-textPrimary mb-2.5 pb-1 border-b border-border/50">
              <CatIcon size={14} class="text-accent" />
              <span>{category.title}</span>
            </div>

            <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
              {#each category.shortcuts as item}
                <div class="flex items-center justify-between gap-3 p-2 rounded-xl bg-background/50 border border-border/60 hover:border-border transition-colors">
                  <span class="text-xs text-textMuted leading-tight truncate">{item.description}</span>
                  <div class="flex items-center gap-1 shrink-0">
                    {#each item.keys as key}
                      <kbd class="gp-keycap text-[11px] font-sans font-medium px-1.5 py-0.5">{key}</kbd>
                    {/each}
                  </div>
                </div>
              {/each}
            </div>
          </div>
        {/each}

        {#if filteredCategories.length === 0}
          <div class="text-center py-8 text-xs text-textMuted">
            No shortcuts match "{filterQuery}"
          </div>
        {/if}
      </div>

      <!-- Footer -->
      <div class="p-3 border-t border-border/70 bg-surfaceHover/20 flex items-center justify-between text-[11px] text-textMuted px-5">
        <span>Tip: Press <kbd class="gp-keycap">?</kbd> anywhere to reopen</span>
        <button type="button" class="gp-btn !py-1 !px-3" onclick={close}>Done</button>
      </div>
    </div>
  </div>
{/if}
