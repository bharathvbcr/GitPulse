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
  import { ChevronDown, ChevronUp, Hash, Search, X } from "@lucide/svelte";
  import { parseColonLineJump, parseGoToLineInput } from "../files/revealRequests";
  import { fileSearchControl } from "../files/fileSearchTarget";

  let {
    open = $bindable(false),
    mode = $bindable<"find" | "line">("find"),
    query = $bindable(""),
    caseSensitive = $bindable(false),
    regex = $bindable(false),
    input = $bindable(),
    label,
    placeholder,
    shortcut,
    goToShortcut = "",
    lineCount = 0,
    status,
    invalid = false,
    hasMatches,
    onstep,
    ongotoline,
    registerControl = false,
    onclose,
  }: {
    open?: boolean;
    /** Active mode when unified with Go to line (`find` | `line`). */
    mode?: "find" | "line";
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
    /** The platform's chord label for Go to line, when `ongotoline` is supplied. */
    goToShortcut?: string;
    /** Total lines in the target file, enabling Go to line when > 0. */
    lineCount?: number;
    /** "3 of 12", "No matches", "Invalid pattern": shown while there is a query. */
    status: string;
    /** The pattern does not compile; the status reads in the danger ink. */
    invalid?: boolean;
    hasMatches: boolean;
    onstep: (delta: 1 | -1) => void;
    /** Jumps to a 1-based line (and optional column); enables inline Go to line in the same pill. */
    ongotoline?: (line: number, column?: number | null) => void;
    /** Registers this control for scoped `triggerFileSearch` (`⌘F`/`⌘G`) dispatch. */
    registerControl?: boolean;
    /** Runs after the field closes, for the viewer to drop its highlights. */
    onclose?: () => void;
  } = $props();

  let lineQuery = $state("");

  const supportsGoToLine = $derived(typeof ongotoline === "function");
  const canGoToLine = $derived(supportsGoToLine && lineCount > 0);
  const activeMode = $derived<"find" | "line">(
    supportsGoToLine && mode === "line" ? "line" : "find",
  );
  const parsedLine = $derived(parseGoToLineInput(lineQuery, lineCount));
  const colonJump = $derived(
    supportsGoToLine ? parseColonLineJump(query, lineCount, { regex, hasMatches }) : null,
  );
  const controlHandle = $derived(
    registerControl
      ? {
          openFind: (seed?: string) => {
            void openFind(seed);
          },
          openLine: () => {
            void openLine();
          },
          get canGoToLine() {
            return canGoToLine;
          },
        }
      : null,
  );

  export async function openFind(seed?: string): Promise<void> {
    if (typeof seed === "string" && seed.length > 0) {
      query = seed;
    }
    mode = "find";
    open = true;
    await tick();
    input?.focus();
    input?.select();
  }

  export async function openLine(): Promise<void> {
    if (!canGoToLine) return;
    mode = "line";
    lineQuery = "";
    open = true;
    await tick();
    input?.focus();
    input?.select();
  }

  export function close(): void {
    collapse();
  }

  function collapse(): void {
    open = false;
    query = "";
    lineQuery = "";
    mode = "find";
    onclose?.();
  }

  function submitLine(): void {
    if (parsedLine.target === null) return;
    const target = parsedLine.target;
    const column = parsedLine.column;
    open = false;
    lineQuery = "";
    mode = "find";
    ongotoline?.(target, column);
  }

  function handleEnter(shiftKey: boolean): void {
    if (activeMode === "line") {
      submitLine();
      return;
    }
    if (colonJump !== null) {
      const { line: target, column } = colonJump;
      open = false;
      query = "";
      lineQuery = "";
      mode = "find";
      onclose?.();
      ongotoline?.(target, column);
      return;
    }
    onstep(shiftKey ? -1 : 1);
  }

  function handleModeChord(event: KeyboardEvent): boolean {
    if (event.key === "F3" && !event.metaKey && !event.ctrlKey && !event.altKey && activeMode === "find" && hasMatches) {
      event.preventDefault();
      event.stopPropagation();
      onstep(event.shiftKey ? -1 : 1);
      return true;
    }
    if (!(event.metaKey || event.ctrlKey) || event.altKey || event.shiftKey) return false;
    const key = event.key.toLowerCase();
    if (key === "g" && canGoToLine) {
      event.preventDefault();
      event.stopPropagation();
      void openLine();
      return true;
    }
    if (key === "f") {
      event.preventDefault();
      event.stopPropagation();
      void openFind();
      return true;
    }
    return false;
  }

  function onkeydown(event: KeyboardEvent): void {
    if (event.isComposing) return;
    if (handleModeChord(event)) return;
    if (event.key === "Enter") {
      event.preventDefault();
      handleEnter(event.shiftKey);
    } else if (event.key === "Escape") {
      event.preventDefault();
      collapse();
    }
  }
</script>

{#if open}
  <!-- The open pill is 28px (md) so its 24px controls keep the minimum
       target. It flexes (11rem floor/basis for find-only toolbars like Diff's
       900px row so wider system fonts still stay on one line, 15rem/16rem when
       the Go-to-line toggle sits beside it, growing up to 20rem) so it takes
       the free width of the toolbar row it opened in; a wider basis wrapped
       onto a line of its own in the diff's 900px toolbar, which is the
       second row this replaced. Focus does not repaint it: the caret marks
       the field (text boxes carry no global focus ring; app.css). -->
  <div
    use:fileSearchControl={controlHandle}
    role="search"
    aria-label={activeMode === "line" ? "Go to line" : label}
    class="find-open flex h-7 {supportsGoToLine ? 'min-w-60 basis-64' : 'min-w-44 basis-44'} max-w-80 shrink grow items-center gap-0.5 rounded-full border border-edgeStrong bg-background pl-2.5 pr-0.5"
    data-find-field
    data-find-mode={activeMode}
  >
    {#if activeMode === "line"}
      <Hash size={13} class="shrink-0 text-accent" aria-hidden="true" />
      <input
        bind:this={input}
        bind:value={lineQuery}
        {onkeydown}
        type="text"
        inputmode="numeric"
        class="min-w-0 flex-1 bg-transparent px-1.5 font-mono text-xs text-textPrimary outline-hidden placeholder:text-faint"
        placeholder="Line (1–{lineCount})…"
        aria-label="Go to line"
        data-goto-line-input
      />
      {#if !parsedLine.empty}
        <span
          class="shrink-0 px-1 tabular-nums text-micro {parsedLine.invalid ? 'text-danger' : 'text-textMuted'}"
          aria-live="polite"
          data-find-status
        >
          {parsedLine.invalid
            ? `1–${lineCount}`
            : parsedLine.column !== null
              ? `Line ${parsedLine.target}, Col ${parsedLine.column}`
              : `Line ${parsedLine.target} of ${lineCount}`}
        </span>
      {/if}
      <button
        type="button"
        class="gp-seg-btn shrink-0 px-2! text-micro font-medium"
        disabled={parsedLine.target === null}
        onclick={submitLine}
        title="Jump to line (Enter)"
        data-goto-submit
      >Go</button>
      <button
        type="button"
        class="gp-seg-btn shrink-0 px-1.5! text-micro"
        onclick={() => void openFind()}
        title="{label} ({shortcut})"
        aria-label="Switch to find"
        data-find-mode-toggle
      ><Search size={12} aria-hidden="true" /></button>
      <button
        type="button"
        class="gp-icon-btn shrink-0"
        onclick={collapse}
        title="Close go to line (Escape)"
        aria-label="Close go to line"
      ><X size={13} /></button>
    {:else}
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
        <span
          class="shrink-0 px-1 tabular-nums text-micro {invalid && colonJump === null ? 'text-danger' : 'text-textMuted'}"
          aria-live="polite"
          data-find-status
        >{colonJump !== null
          ? colonJump.column !== null
            ? `Line ${colonJump.line}, Col ${colonJump.column}`
            : `Line ${colonJump.line}`
          : status}</span>
      {/if}
      <button type="button" class="gp-seg-btn shrink-0 px-1.5! text-micro" data-active={caseSensitive} aria-pressed={caseSensitive} onclick={() => (caseSensitive = !caseSensitive)} title="Match case">Aa</button>
      <button type="button" class="gp-seg-btn shrink-0 px-1.5! font-mono text-micro" data-active={regex} aria-pressed={regex} onclick={() => (regex = !regex)} title="Regular expression">.*</button>
      <button type="button" class="gp-icon-btn shrink-0" disabled={!hasMatches} onclick={() => onstep(-1)} title="Previous match (Shift+Enter)" aria-label="Previous match"><ChevronUp size={13} /></button>
      <button type="button" class="gp-icon-btn shrink-0" disabled={!hasMatches} onclick={() => onstep(1)} title="Next match (Enter)" aria-label="Next match"><ChevronDown size={13} /></button>
      {#if supportsGoToLine}
        <button
          type="button"
          class="gp-seg-btn shrink-0 px-1.5! text-micro"
          disabled={!canGoToLine}
          onclick={() => void openLine()}
          title="Go to line{goToShortcut ? ` (${goToShortcut})` : ''}"
          aria-label="Switch to go to line"
          data-goto-mode-toggle
        ><Hash size={12} aria-hidden="true" /></button>
      {/if}
      <button type="button" class="gp-icon-btn shrink-0" onclick={collapse} title="Close find (Escape)" aria-label="Close find"><X size={13} /></button>
    {/if}
  </div>
{:else}
  <button
    use:fileSearchControl={controlHandle}
    type="button"
    class="gp-btn shrink-0 text-textMuted"
    data-size="sm"
    aria-expanded="false"
    onclick={() => void openFind()}
    title="{label} ({shortcut})"
    data-find-trigger
  >
    <Search size={13} aria-hidden="true" />
    <span>Find</span>
    <span class="gp-keycap text-micro!">{shortcut}</span>
  </button>
  {#if supportsGoToLine}
    <button
      type="button"
      class="gp-btn shrink-0 text-textMuted"
      data-size="sm"
      aria-expanded="false"
      disabled={!canGoToLine}
      onclick={() => void openLine()}
      title="Go to line{goToShortcut ? ` (${goToShortcut})` : ''}"
      data-goto-line-trigger
    >
      <Hash size={12} aria-hidden="true" />
      <span>Go to line</span>
      {#if goToShortcut}
        <span class="gp-keycap text-micro!">{goToShortcut}</span>
      {/if}
    </button>
  {/if}
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
