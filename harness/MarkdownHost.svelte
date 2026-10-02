<script lang="ts">
  /**
   * Mounts the production Markdown surfaces for `markdown.html`: the
   * explorer's MarkDevViewer for a repository file, and MarkdownBody as a
   * commit message uses it. `update` changes what they show in place, so a
   * check can see what a re-render leaves behind.
   */
  import { untrack } from "svelte";
  import MarkDevViewer from "../src/lib/components/files/MarkDevViewer.svelte";
  import MarkdownBody from "../src/lib/components/MarkdownBody.svelte";

  type Shown = { filePath: string; text: string; commitBody: string };
  let { initial, width }: { initial: Shown; width: number } = $props();

  // The first value only: later changes arrive through `update`.
  let shown = $state<Shown>(untrack(() => ({ ...initial })));

  export function update(next: Partial<Shown>) {
    shown = { ...shown, ...next };
  }
</script>

<div data-host="viewer" style="height: 640px; width: {width}px; display: flex; flex-direction: column">
  <MarkDevViewer
    filePath={shown.filePath}
    blob={{ path: shown.filePath, is_binary: false, is_image: false, mime: "text/markdown", text: shown.text }}
  />
</div>
<div data-host="commit" style="width: {width}px; padding: 8px">
  <MarkdownBody source={shown.commitBody} />
</div>
