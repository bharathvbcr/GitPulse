<script lang="ts">
  import { ArrowLeft, Folder, FolderPlus, FolderTree } from "@lucide/svelte";

  /**
   * The tab menu's group page, reached from its "Add to group ›" or
   * "Change group ›" row: every group the tab could join, the group its
   * parent folder names, and a new group. It replaces the root page in
   * place, as TaskContextMenu's submenus do, so it shares the root's
   * placement, dismissal and keyboard owner rather than standing up a second
   * popover beside it.
   */
  interface Props {
    /** "Add to group" or "Change group", shown on the back row. */
    title: string;
    /** The tab's current group; it is left out of the choices. */
    current: string | null;
    /** Every group open in the strip, in strip order. */
    groups: readonly string[];
    /** The group the tab's parent folder names, or null when it has none. */
    parentFolder: string | null;
    onback: () => void;
    onpick: (group: string) => void;
    onnew: () => void;
  }
  let { title, current, groups, parentFolder, onback, onpick, onnew }: Props = $props();

  // The parent folder has its own row, so it is not listed twice.
  const offerParent = $derived(parentFolder !== null && parentFolder !== current);
  const choices = $derived(
    groups.filter((group) => group !== current && !(offerParent && group === parentFolder)),
  );
</script>

<button role="menuitem" class="gp-menu-item" data-group-back onclick={onback}>
  <ArrowLeft size={12} class="shrink-0 text-textMuted" />
  <span class="truncate">{title}</span>
</button>
<span class="gp-menu-sep" aria-hidden="true"></span>
{#if choices.length > 0}
  <div class="max-h-64 overflow-y-auto" role="none">
    {#each choices as group (group)}
      <button role="menuitem" class="gp-menu-item" data-group-choice={group} onclick={() => onpick(group)}>
        <Folder size={12} class="shrink-0 text-textMuted" />
        <span class="truncate">{group}</span>
      </button>
    {/each}
  </div>
  <span class="gp-menu-sep" aria-hidden="true"></span>
{/if}
{#if offerParent && parentFolder}
  <button role="menuitem" class="gp-menu-item" data-group-parent={parentFolder} onclick={() => onpick(parentFolder)}>
    <FolderTree size={12} class="shrink-0 text-textMuted" />
    <span class="truncate">Parent folder ({parentFolder})</span>
  </button>
{/if}
<button role="menuitem" class="gp-menu-item" data-group-new onclick={onnew}>
  <FolderPlus size={12} class="shrink-0 text-textMuted" />
  <span>New group…</span>
</button>
