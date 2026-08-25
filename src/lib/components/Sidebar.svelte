<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { displayName } from "../repos/paths";
  import BranchList from "./BranchList.svelte";
  import CommitComposer from "./CommitComposer.svelte";
  import WorktreesPanel from "./WorktreesPanel.svelte";
  import {
    FolderGit2,
    Plus,
    Minus,
    FolderOpen,
  } from "lucide-svelte";

  // An agent can touch thousands of files in one pass; lists mount a window
  // and grow on demand so the sidebar never becomes the bottleneck.
  const FILE_LIST_STEP = 300;
  let stagedLimit = $state(FILE_LIST_STEP);
  let unstagedLimit = $state(FILE_LIST_STEP);

  let stagedFiles = $derived($repoStore.statuses.filter((s) => s.is_staged));
  let unstagedFiles = $derived($repoStore.statuses.filter((s) => !s.is_staged));
  let visibleStaged = $derived(stagedFiles.slice(0, stagedLimit));
  let visibleUnstaged = $derived(unstagedFiles.slice(0, unstagedLimit));

</script>

<aside class="relative w-80 bg-surface border-r border-border flex flex-col font-sans select-none text-xs shrink-0 h-full gp-pane">
  {#if $repoStore.pendingMutation}
    <!-- Stage/unstage and menu-driven mutations report through the store;
         this strip is their progress indicator. -->
    <div class="absolute top-0 inset-x-0 h-0.5 z-30 overflow-hidden" role="status">
      <span class="sr-only">{$repoStore.pendingMutation}…</span>
      <div class="h-full w-1/3 bg-accent animate-[gp-slide_1.2s_ease-in-out_infinite]"></div>
    </div>
  {/if}
  <!-- Repo Header & Open Button -->
  <div class="p-3 flex items-center justify-between bg-surfaceHover/30">
    <div class="flex items-center gap-2 truncate">
      <FolderGit2 size={15} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary truncate" title={$repoStore.currentPath || "No Repo"}>
        {$repoStore.currentPath ? displayName($repoStore.currentPath) : "No Repository"}
      </span>
    </div>
    <button
      onclick={() => repoStore.pickAndOpenRepo()}
      title="Open Repository"
      aria-label="Open Repository"
      class="gp-icon-btn !p-1 hover:text-accent"
    >
      <FolderOpen size={14} />
    </button>
  </div>

  <!-- Main Scrollable Section -->
  <div class="flex-1 overflow-y-auto p-2 space-y-4">
    <BranchList />

    <WorktreesPanel />

    <!-- Staged Changes -->
    <div>
      <div class="flex items-center justify-between text-[10px] font-bold text-textMuted uppercase tracking-wider px-2 mb-1">
        <span>Staged Changes ({stagedFiles.length})</span>
        {#if stagedFiles.length > 0}
          <button
            onclick={() => repoStore.unstageAll()}
            title="Unstage all files"
            class="text-[9px] lowercase font-normal text-textMuted hover:text-red-400 transition-colors"
          >
            unstage all
          </button>
        {/if}
      </div>
      {#if stagedFiles.length === 0}
        <div class="text-[11px] text-textMuted/60 px-2 py-1 italic">No staged changes</div>
      {:else}
        <div class="space-y-0.5">
          {#each visibleStaged as f (f.path)}
            <div
              role="button"
              tabindex="0"
              class="px-2 py-1 rounded-full flex items-center justify-between hover:bg-surfaceHover text-textPrimary group transition-colors cursor-pointer"
              onclick={() => repoStore.selectFileDiff(f.path, true)}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  // Space would otherwise scroll the ancestor scroller.
                  e.preventDefault();
                  repoStore.selectFileDiff(f.path, true);
                }
              }}
            >
              <span class="truncate text-green-400 font-mono text-[11px]">{f.path}</span>
              <button
                onclick={(e) => {
                  e.stopPropagation();
                  repoStore.unstageFile(f.path);
                }}
                title="Unstage file"
                class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 hover:bg-background hover:text-red-400 shrink-0"
              >
                <Minus size={12} />
              </button>
            </div>
          {/each}
          {#if stagedFiles.length > visibleStaged.length}
            <button
              class="w-full px-2 py-1 rounded-xl border border-dashed border-border/80 text-[10px] text-textMuted hover:text-textPrimary"
              onclick={() => (stagedLimit = stagedFiles.length)}
            >
              Show {(stagedFiles.length - visibleStaged.length).toLocaleString()} more
            </button>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Unstaged / Working Tree Changes -->
    <div>
      <div class="flex items-center justify-between text-[10px] font-bold text-textMuted uppercase tracking-wider px-2 mb-1">
        <span>Changes ({unstagedFiles.length})</span>
        {#if unstagedFiles.length > 0}
          <button
            onclick={() => repoStore.stageAll()}
            title="Stage all files"
            class="text-[9px] lowercase font-normal text-textMuted hover:text-green-400 transition-colors"
          >
            stage all
          </button>
        {/if}
      </div>
      {#if unstagedFiles.length === 0}
        <div class="text-[11px] text-textMuted/60 px-2 py-1 italic">Working tree clean</div>
      {:else}
        <div class="space-y-0.5">
          {#each visibleUnstaged as f (f.path + "-" + f.status_code)}
            <div
              role="button"
              tabindex="0"
              class="px-2 py-1 rounded-full flex items-center justify-between hover:bg-surfaceHover text-textPrimary group transition-colors cursor-pointer"
              onclick={() => repoStore.selectFileDiff(f.path, false)}
              onkeydown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  // Space would otherwise scroll the ancestor scroller.
                  e.preventDefault();
                  repoStore.selectFileDiff(f.path, false);
                }
              }}
            >
              <span class="truncate font-mono text-[11px] {f.is_conflicted ? 'text-amber-400 font-bold' : 'text-textPrimary'}">{f.path}</span>
              <button
                onclick={(e) => {
                  e.stopPropagation();
                  repoStore.stageFile(f.path);
                }}
                title="Stage file"
                class="p-0.5 rounded-full opacity-0 group-hover:opacity-100 hover:bg-background hover:text-green-400 shrink-0"
              >
                <Plus size={12} />
              </button>
            </div>
          {/each}
          {#if unstagedFiles.length > visibleUnstaged.length}
            <button
              class="w-full px-2 py-1 rounded-xl border border-dashed border-border/80 text-[10px] text-textMuted hover:text-textPrimary"
              onclick={() => (unstagedLimit = unstagedFiles.length)}
            >
              Show {(unstagedFiles.length - visibleUnstaged.length).toLocaleString()} more
            </button>
          {/if}
        </div>
      {/if}
    </div>
  </div>

  <CommitComposer />
</aside>
