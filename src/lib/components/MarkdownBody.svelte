<script lang="ts">
  /**
   * Renders markdown bodies (commit messages, PR/issue text, verdicts, …)
   * through MarkDev's Rust flat-model renderer.
   */
  import { renderMarkDevMarkdown } from "../files/markdevRender";

  let {
    source = "",
    class: className = "text-[11px] text-textMuted select-text markdown-body",
  }: {
    source?: string | null;
    class?: string;
  } = $props();

  let html = $state("");
  let error = $state<string | null>(null);

  $effect(() => {
    const text = source ?? "";
    let cancelled = false;
    error = null;
    if (!text) {
      html = "";
      return;
    }
    void renderMarkDevMarkdown(text)
      .then((rendered) => {
        if (!cancelled) html = rendered;
      })
      .catch((err: unknown) => {
        if (!cancelled) {
          error = err instanceof Error ? err.message : String(err);
          // Fail closed: show escaped plaintext rather than a blank panel.
          html = "";
        }
      });
    return () => {
      cancelled = true;
    };
  });
</script>

{#if error}
  <div class="text-[11px] text-rose-400 whitespace-pre-wrap">{source}</div>
{:else if html}
  <div class={className}>{@html html}</div>
{:else if source}
  <div class="text-[11px] text-textMuted whitespace-pre-wrap">{source}</div>
{/if}
