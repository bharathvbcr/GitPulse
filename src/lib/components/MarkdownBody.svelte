<script lang="ts">
  /**
   * Renders markdown bodies (commit messages, PR/issue text, verdicts, …)
   * through MarkDev's renderer. These are not files, so nothing is read from
   * disk and relative links have nothing to resolve against.
   */
  import { renderMarkDevMarkdown } from "../files/markdevRender";
  import { formatError } from "../ui/formatError";
  import MarkdownContent from "./MarkdownContent.svelte";

  let {
    source = "",
    class: className = "text-[11px] text-textMuted select-text",
  }: {
    source?: string | null;
    class?: string;
  } = $props();

  let bodyHtml = $state("");
  let error = $state<string | null>(null);

  $effect(() => {
    const text = source ?? "";
    let cancelled = false;
    error = null;
    bodyHtml = "";
    if (!text) return;
    void renderMarkDevMarkdown(text)
      .then((rendered) => {
        if (!cancelled) bodyHtml = rendered.html;
      })
      .catch((err: unknown) => {
        // Fail closed: the escaped source rather than a blank panel, and the
        // reason beside it rather than nowhere.
        if (!cancelled) error = formatError(err);
      });
    return () => {
      cancelled = true;
    };
  });
</script>

{#if error}
  <div class="text-[11px] text-textMuted whitespace-pre-wrap">{source}</div>
  <div class="text-[10px] text-rose-400" role="status">Could not render this as Markdown: {error}</div>
{:else if bodyHtml}
  <MarkdownContent html={bodyHtml} class="gp-markdown-compact {className}" />
{:else if source}
  <div class="text-[11px] text-textMuted whitespace-pre-wrap">{source}</div>
{/if}
