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
  let { initial, width, height = 640 }: { initial: Shown; width: number; height?: number } = $props();

  // The first value only: later changes arrive through `update`.
  let shown = $state<Shown>(untrack(() => ({ ...initial })));
  let hostWidth = $state(untrack(() => width));
  let hostHeight = $state(untrack(() => height));

  export function update(next: Partial<Shown> & { width?: number; height?: number }) {
    const { width: nextWidth, height: nextHeight, ...rest } = next;
    if (typeof nextWidth === "number") hostWidth = nextWidth;
    if (typeof nextHeight === "number") hostHeight = nextHeight;
    shown = { ...shown, ...rest };
  }
</script>

<div data-host="viewer" style="height: {hostHeight}px; width: {hostWidth}px; display: flex; flex-direction: column">
  <MarkDevViewer
    filePath={shown.filePath}
    blob={{ path: shown.filePath, is_binary: false, is_image: false, mime: "text/markdown", text: shown.text }}
  />
</div>
<div data-host="commit" style="width: {hostWidth}px; padding: 8px">
  <MarkdownBody source={shown.commitBody} />
</div>
