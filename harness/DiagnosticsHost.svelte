<script lang="ts">
  import BlameViewer from "../src/lib/components/BlameViewer.svelte";
  import RepoMapPanel from "../src/lib/components/RepoMapPanel.svelte";
  import MarkDevViewer from "../src/lib/components/files/MarkDevViewer.svelte";
  import DiagnosticsModal from "../src/lib/components/DiagnosticsModal.svelte";
  import { keyedList } from "../src/lib/ui/eachKeys";
  import { createPaneCrashReporter } from "../src/lib/diagnostics/paneCrash";

  let { crashes, reports }: { crashes: string[]; reports: string[] } = $props();
  let mode = $state("blame");
  let rows = $state<string[]>([]);
  const reporter = createPaneCrashReporter({ error: (_source, detail) => reports.push(String(detail)) },
    () => ({ view: "code", section: mode, repo: "/r/diagnostics", file: "A.md" }));
  export function show(next: string) { mode = next; }
  export function setRows(next: string[]) { rows = next; }
</script>

{#key mode}
  <svelte:boundary onerror={(error) => { crashes.push(String(error)); reporter.report("code-harness", error); }}>
    {#if mode === "blame"}
      <BlameViewer />
    {:else if mode === "map"}
      <RepoMapPanel />
    {:else if mode === "diagnostics"}
      <DiagnosticsModal isOpen={true} />
    {:else if mode === "markdown"}
      <MarkDevViewer filePath="A.md" blob={{ path: "A.md", text: "# Hello", is_binary: false, is_image: false, mime: "text/markdown" }} />
    {:else if mode === "canary"}
      {#each ["repeat", "repeat"] as item (item)}<span>{item}</span>{/each}
    {:else if mode === "keys"}
      {#each keyedList(rows, (row) => row) as { item, key } (key)}<span data-key-row>{item}</span>{/each}
    {/if}
    {#snippet failed()}<p data-failed>Harness caught a pane crash</p>{/snippet}
  </svelte:boundary>
{/key}
