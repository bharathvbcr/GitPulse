<script lang="ts">
  /**
   * The real terminal panel on a fixed stage, with the three things around it
   * a browser session has to follow: whether the dock shows the panel, how
   * wide the panel is, and whether it is mounted at all. The page drives them
   * through `window.__gpBrowserHost`.
   */
  import TerminalPanel from "../src/lib/components/TerminalPanel.svelte";

  let visible = $state(true);
  let width = $state(900);
  let mounted = $state(true);

  Reflect.set(window, "__gpBrowserHost", {
    setVisible: (next: boolean) => (visible = next),
    setWidth: (next: number) => (width = next),
    setMounted: (next: boolean) => (mounted = next),
  });
</script>

<div id="stage" style="position:absolute;left:20px;top:30px;width:{width}px;height:520px;display:flex;flex-direction:column;overflow:hidden">
  {#if mounted}
    <TerminalPanel repoPath="/fixture/GitPulse" {visible} />
  {/if}
</div>
