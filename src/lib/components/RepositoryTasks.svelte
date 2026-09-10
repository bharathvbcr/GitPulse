<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import TaskBoard from "./TaskBoard.svelte";
  let { active = true }: { active?: boolean } = $props();
</script>
<TaskBoard repositoryPath={$repoStore.currentPath} {active} />
