<script lang="ts">
  import { ExternalLink } from "@lucide/svelte";

  /**
   * The row-level "open this on GitHub" button.
   *
   * It was written three times — Dependabot alerts, code scanning alerts,
   * advisories — each as an icon-only `<button>` carrying a `title` and
   * nothing else. `title` is a tooltip, not an accessible name in any
   * reliable sense: with no text content and no `aria-label` the three
   * announced as "button", so a table of twelve alerts offered twelve
   * identical unnamed controls.
   *
   * `label` is required and names the specific thing being opened, so the
   * controls are distinguishable from one another rather than merely named.
   */
  let {
    url,
    label,
    onopen,
  }: {
    url: string;
    label: string;
    onopen: (url: string) => void;
  } = $props();
</script>

<button
  type="button"
  class="p-1 rounded-full hover:bg-surfaceHover text-textMuted hover:text-accent transition-colors focus-visible:outline-2 focus-visible:outline-accent"
  aria-label={label}
  title={label}
  onclick={() => onopen(url)}
>
  <ExternalLink size={13} aria-hidden="true" />
</button>
