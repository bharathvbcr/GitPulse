<script lang="ts">
  /**
   * macOS watch scope. Narrow (the default) leaves the build and dependency
   * trees git ignores out of the file-event stream; Whole worktree covers
   * everything. The change re-registers the live watches (`repoStore`).
   */
  import SettingSegment from "./SettingSegment.svelte";
  import { interfaceStore, type WatchScope } from "../stores/interfaceStore";

  const OPTIONS: readonly { value: WatchScope; label: string }[] = [
    { value: "narrow", label: "Narrow (skip ignored build folders)" },
    { value: "whole", label: "Whole worktree" },
  ];
</script>

<div class="space-y-1 text-meta" data-testid="watch-scope-setting">
  <div class="text-textPrimary text-meta font-medium mb-2">Watch scope</div>
  <SettingSegment
    ariaLabel="Watch scope"
    options={OPTIONS}
    value={$interfaceStore.watchScope}
    onselect={(scope) => interfaceStore.setWatchScope(scope)}
  />
  <p class="text-textMuted text-micro mt-1.5">
    On macOS, Narrow stops watching folders git ignores that hold build output or other checkouts,
    which cuts system file-event work during builds. Whole worktree watches everything, as before.
    It only affects macOS.
  </p>
</div>
