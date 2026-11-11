<script lang="ts">
  /**
   * Find, in place. Closed it is the toolbar's "Find" pill; open, that same
   * pill widens into the field with its match count, case and pattern
   * toggles, previous/next and close, in the toolbar row it already sat in.
   *
   * The Diff and Code viewers each had a Find button that opened a second
   * toolbar row under the first, styled differently in each (bordered square
   * toggles in one, pressed pills in the other, an "✕" glyph beside a lucide
   * X). One owner now draws it; each viewer keeps its own matching, because a
   * diff's rows and a file's lines are searched differently.
   *
   * Keys belong to the box: Enter steps forward, Shift+Enter back, Escape
   * closes. The opener focuses and selects the query, so a second Cmd+F
   * replaces it rather than appending.
   */
  import { tick } from "svelte";
  import { ChevronDown, ChevronUp, Search, X } from "@lucide/svelte";

  let {
    open = $bindable(false),
    query = $bindable(""),
    caseSensitive = $bindable(false),
    regex = $bindable(false),
    input = $bindable(),
    label,
    placeholder,
    shortcut,
    status,
    invalid = false,
    hasMatches,
    onstep,
    onclose,
  }: {
    open?: boolean;
    query?: string;
    caseSensitive?: boolean;
    regex?: boolean;
    /** The text box, for a viewer's own Cmd+F handler to focus. */
    input?: HTMLInputElement;
    /** What is searched, for assistive tech and the tooltip: "Find in this diff". */
    label: string;
    placeholder: string;
    /** The platform's chord label, shown on the closed pill and in its tooltip. */
    shortcut: string;
    /** "3 of 12", "No matches", "Invalid pattern": shown while there is a query. */
    status: string;
    /** The pattern does not compile; the status reads in the danger ink. */
    invalid?: boolean;
    hasMatches: boolean;
    onstep: (delta: 1 | -1) => void;
    /** Runs after the field closes, for the viewer to drop its highlights. */
    onclose?: () => void;
  } = $props();

  async function expand(): Promise<void> {
    open = true;
    await tick();
    input?.focus();
    input?.select();
  }

  function collapse(): void {
    open = false;
    query = "";
    onclose?.();
  }

  function onkeydown(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (event.key === "Enter") {
      event.preventDefault();
      onstep(event.shiftKey ? -1 : 1);
    } else if (event.key === "Escape") {
      event.preventDefault();
      collapse();
    }
  }
</script>

{#if open}
  <!-- The open pill is 28px (md) so its 24px controls keep the minimum
       target. It flexes (16rem basis, 15rem floor, up to 20rem) so it takes
       the free width of the toolbar row it opened in; a fixed width wrapped
       onto a line of its own in the diff's 900px toolbar, which is the
       second row this replaced. Focus does not repaint it: the caret marks
       the field (text boxes carry no global focus ring; app.css). -->
  <div role="search" aria-label={label} class="find-open flex h-7 min-w-60 max-w-80 shrink grow basis-64 items-center gap-0.5 rounded-full border border-edgeStrong bg-background pl-2.5 pr-0.5" data-find-field>
    <Search size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
    <input
      bind:this={input}
      bind:value={query}
      {onkeydown}
      type="text"
      class="min-w-0 flex-1 bg-transparent px-1.5 text-xs text-textPrimary outline-hidden placeholder:text-faint"
      {placeholder}
      aria-label={label}
    />
    {#if query.trim()}
      <span class="shrink-0 px-1 tabular-nums text-micro {invalid ? 'text-danger' : 'text-textMuted'}" aria-live="polite" data-find-status>{status}</span>
    {/if}
    <button type="button" class="gp-seg-btn shrink-0 px-1.5! text-micro" data-active={caseSensitive} aria-pressed={caseSensitive} onclick={() => (caseSensitive = !caseSensitive)} title="Match case">Aa</button>
    <button type="button" class="gp-seg-btn shrink-0 px-1.5! font-mono text-micro" data-active={regex} aria-pressed={regex} onclick={() => (regex = !regex)} title="Regular expression">.*</button>
    <button type="button" class="gp-icon-btn shrink-0" disabled={!hasMatches} onclick={() => onstep(-1)} title="Previous match (Shift+Enter)" aria-label="Previous match"><ChevronUp size={13} /></button>
    <button type="button" class="gp-icon-btn shrink-0" disabled={!hasMatches} onclick={() => onstep(1)} title="Next match (Enter)" aria-label="Next match"><ChevronDown size={13} /></button>
    <button type="button" class="gp-icon-btn shrink-0" onclick={collapse} title="Close find (Escape)" aria-label="Close find"><X size={13} /></button>
  </div>
{:else}
  <button type="button" class="gp-btn shrink-0 text-textMuted" data-size="sm" aria-expanded="false" onclick={() => void expand()} title="{label} ({shortcut})" data-find-trigger>
    <Search size={13} aria-hidden="true" />
    <span>Find</span>
    <span class="gp-keycap text-micro!">{shortcut}</span>
  </button>
{/if}

<style>
  /* It grows out of the pill's left edge: transform and opacity only
     (DESIGN_LANGUAGE §8.3), so the toolbar never re-lays out per frame.
     Reduced motion (the app setting or the system's) opens it at once, as
     app.css does for the shared primitives. */
  .find-open {
    transform-origin: left center;
    transition-property: transform, opacity;
    transition-duration: var(--gp-dur-base);
    transition-timing-function: var(--gp-ease-spatial);
  }
  @starting-style {
    .find-open {
      opacity: 0;
      transform: scaleX(0.3);
    }
  }
  :global(html[data-motion="reduced"]) .find-open {
    transition: none;
  }
  @media (prefers-reduced-motion: reduce) {
    .find-open {
      transition: none;
    }
  }
</style>
