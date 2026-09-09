<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import TaskBoard from "./TaskBoard.svelte";
</script>
<TaskBoard repositoryPath={$repoStore.currentPath} />
