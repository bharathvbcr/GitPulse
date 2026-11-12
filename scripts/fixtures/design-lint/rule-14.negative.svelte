<script lang="ts">
  import { listProjects } from "$lib/projects/attempt-store";
  import frameRate from "../harness/frame-rate";
  import { invoke } from "@tauri-apps/api/core";
  // The harness retries each attempt in its own lane.
  const status = invoke("devmap_status", { project: "repo" });
</script>

<!-- A comment about the attempt, the lane and MANVI. -->
<button data-testid="attempt-retry">Try again</button>
<Row data-testid="project-lane" data-harness="on" />
<p>{attemptCount} {lane.name} {project.repo.attempt} {harness}</p>
<p>{item.kind === "attempt" ? "Done" : "Open"} {"lane" !== kind ? "x" : "y"}</p>
<p>{invoke("devmap_status")} {mapFailureMessage("no_cli")} {openSetupWizard("devmap", "explain")}</p>
{#if kind === "attempt"}<p>Done</p>{:else if kind === "lane"}<p>Open</p>{/if}
<Row onTools={() => run("devmap")} onclick={() => go("project")} data-testid={"attempt-row"} />
<p class={busy ? "attempt-row lane-row" : ""}>Runs</p>
<p class="project-list lane-header">Runs in a worktree</p>
<a href="/projects/lane" id="repo">Open</a>
<Field name="project" for="harness" />
<p>Manvi policy, the code map, checks, Settings and a run</p>
<style>
  .attempt-lane { content: "project"; }
</style>
<p>The log couldn't be read</p>
<p>{item.reason === "could not run" ? "Done" : "Open"} {invoke("could not")}</p>
{#if state === "could not"}<p>Done</p>{/if}
<!-- A comment: the scan could not run. -->
