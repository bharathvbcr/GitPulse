<script lang="ts">
  /**
   * Stands in for TerminalPanel in the hidden-pane harness. The dock hosts
   * whatever its loader resolves to and hands it the panel props; the slide is
   * what is under test, so a plain box with a visible label is enough and no
   * PTY or xterm is started.
   */
  interface Props {
    repoPath?: string | null;
    visible?: boolean;
    dockMode?: "docked" | "overlay";
    onToggleDockMode?: () => void;
    onClose?: () => void;
  }
  let { repoPath = null, visible = true, dockMode = "docked", onToggleDockMode, onClose }: Props = $props();
</script>

<div class="flex-1 flex flex-col gap-2 p-3 text-xs text-textPrimary" data-panel-stub data-visible={visible}>
  <div class="flex items-center gap-2">
    <span class="gp-label">Terminal · {repoPath}</span>
    {#if onToggleDockMode}
      <button type="button" class="gp-btn" data-size="sm" data-stub-mode-toggle onclick={onToggleDockMode}>
        {dockMode === "overlay" ? "Dock" : "Float"}
      </button>
    {/if}
    {#if onClose}
      <button type="button" class="gp-btn" data-size="sm" data-stub-close onclick={onClose}>Hide</button>
    {/if}
  </div>
  <div class="font-mono text-meta text-textMuted">$ git status</div>
</div>
