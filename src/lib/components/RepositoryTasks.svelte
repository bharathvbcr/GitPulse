<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import TaskBoard from "./TaskBoard.svelte";
  import ScanProposals from "./ScanProposals.svelte";
  let { active = true }: { active?: boolean } = $props();
</script>
<!-- The proposals dock sits over the board's corner; it owns no board state, so
     the board (and every file that draws it) is unchanged. -->
<div class="repo-tasks">
  <TaskBoard repositoryPath={$repoStore.currentPath} {active} />
  <ScanProposals repositoryPath={$repoStore.currentPath ?? ""} />
</div>
<style>
  .repo-tasks{position:relative;display:flex;flex:1;min-height:0;min-width:0}
</style>
