<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import TaskBoard from "./TaskBoard.svelte";
  let { active = true }: { active?: boolean } = $props();
</script>
<div class="repo-tasks">
  <TaskBoard repositoryPath={$repoStore.currentPath} {active} />
</div>
<style>
  .repo-tasks{position:relative;display:flex;flex:1;min-height:0;min-width:0}
</style>
