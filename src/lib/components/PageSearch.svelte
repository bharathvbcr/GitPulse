<script lang="ts">
  /**
   * The search field in the view bar. One field for every page: it asks the
   * page search registry (`search/pageSearch.svelte.ts`) what the view and
   * section on screen can search, and becomes that.
   *
   * - On History's graph it filters the commits (the query stays applied
   *   when the field loses focus; the branch chip sits inside it).
   * - In a diff or a file it finds: a count, case and pattern toggles,
   *   previous and next; Enter steps, Shift+Enter steps back.
   * - On a page with nothing of its own to search it is the command palette
   *   for this repository, prefixes and all (">" commands, "/" files, "@"
   *   branches, "#" commits, ":" symbols, "%" repositories, "?" help): results
   *   drop down under the field from the palette's own engine, loaded the
   *   first time they are needed.
   *
   * Where the page has a search, what is typed is that search, literally:
   * `#include`, `// TODO`, `@param` or `:root` in a file, `#123` in the commit
   * filter. A prefix that switched to the palette there took those away, and
   * the viewers have no Find of their own to fall back on. ⌘K opens the
   * palette from every page.
   *
   * Each page keeps its own query, so a diff's find text never becomes the
   * commit filter on a section switch. Focus never repaints the pill; the
   * caret marks it. ⌘F focuses it from anywhere on the repository surface.
   */
  import { onMount } from "svelte";
  import { ChevronDown, ChevronUp, Search, X } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";
  import { diagnostics } from "../diagnostics/diagnostics";
  import { hostPlatform } from "../stores/platformStore";
  import { shortcutTextLabel } from "../ui/platformCopy";
  import { activeSectionFor } from "../views/viewRegistry";
  import { FOCUS_PAGE_SEARCH_EVENT } from "../views/commitFilter";
  import { MAX_QUERY_LENGTH } from "../palette/modes";
  import { pageScope, pageSearchFor, standingFilterFor } from "../search/pageSearch.svelte";

  const REPOSITORY_PLACEHOLDER = "Search this repository: commands, / files, @ branches, # commits";

  let input: HTMLInputElement | undefined = $state();
  let pill: HTMLDivElement | undefined = $state();
  let focused = $state(false);
  /** The palette's text, on a page with no search of its own. */
  let paletteText = $state("");
  let activeOption = $state<string | undefined>(undefined);

  const scope = $derived(pageScope($repoStore.activeTab, activeSectionFor($repoStore.activeTab, $repoStore.viewSections)));
  const provider = $derived(pageSearchFor(scope));
  const inPalette = $derived(!provider);
  const value = $derived(inPalette ? paletteText : (provider?.query() ?? ""));
  const cardOpen = $derived(focused && inPalette);
  const find = $derived(provider?.kind === "find" && !inPalette ? provider : null);
  const filter = $derived(provider?.kind === "filter" && !inPalette ? provider : null);
  /** History's commit filter also narrows its Diff section: shown there, not hidden. */
  const standing = $derived(
    provider?.kind !== "filter" ? standingFilterFor($repoStore.activeTab) : null,
  );
  const chip = $derived(filter?.chip?.() ?? standing);
  const label = $derived(inPalette ? "Search this repository" : (provider?.label ?? "Search this repository"));

  // The palette's chunk loads the first time the card has to open: the view
  // bar is in the entry chunk, the palette is not.
  // `requested` is a plain flag, not state: the effect must not read the
  // component it writes, or it would depend on its own result.
  let Palette = $state<(typeof import("./CommandPalette.svelte"))["default"] | null>(null);
  let requested = false;
  $effect(() => {
    if (!cardOpen || requested) return;
    requested = true;
    import("./CommandPalette.svelte").then(
      (module) => {
        Palette = module.default;
      },
      (error: unknown) => {
        // Said, as a lazy mount says it; the next time the card opens retries.
        requested = false;
        diagnostics.error("page-search:palette", error);
        toastStore.error(
          "Repository search couldn't be loaded. It ships as its own chunk, so this means the install is incomplete — reopening the window retries it.",
        );
      },
    );
  });

  function oninput(event: Event & { currentTarget: HTMLInputElement }) {
    const next = event.currentTarget.value;
    if (provider) provider.setQuery(next);
    else paletteText = next;
  }

  function clear() {
    if (inPalette) paletteText = "";
    else provider?.setQuery("");
    input?.focus();
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.isComposing) return;
    // With the results card open, its own handler (capture phase) has already
    // taken the arrows, Enter and Escape.
    if (event.key === "Escape") {
      event.preventDefault();
      if (find) find.setQuery("");
      input?.blur();
    } else if (event.key === "Enter" && find) {
      event.preventDefault();
      find.step(event.shiftKey ? -1 : 1);
    }
  }

  function closeCard() {
    paletteText = "";
    input?.blur();
  }

  onMount(() => {
    const take = () => {
      input?.focus();
      input?.select();
    };
    window.addEventListener(FOCUS_PAGE_SEARCH_EVENT, take);
    return () => window.removeEventListener(FOCUS_PAGE_SEARCH_EVENT, take);
  });
</script>

<div
  bind:this={pill}
  role="search"
  aria-label={label}
  class="flex h-7 min-w-24 @min-[1400px]:min-w-40 max-w-4xl flex-[3] items-center gap-0.5 rounded-full border border-edgeStrong bg-background pl-2.5 pr-0.5 text-xs"
  data-page-search={inPalette ? "palette" : provider?.kind}
>
  <Search size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
  <input
    bind:this={input}
    {value}
    {oninput}
    {onkeydown}
    onfocus={() => (focused = true)}
    onblur={() => {
      focused = false;
      paletteText = "";
    }}
    type="text"
    maxlength={MAX_QUERY_LENGTH}
    autocomplete="off"
    spellcheck="false"
    role="combobox"
    aria-label={label}
    aria-expanded={cardOpen}
    aria-controls={cardOpen ? "page-palette-listbox" : undefined}
    aria-autocomplete="list"
    aria-activedescendant={cardOpen ? activeOption : undefined}
    placeholder={inPalette ? REPOSITORY_PLACEHOLDER : provider?.placeholder}
    class="min-w-0 flex-1 bg-transparent px-1.5 text-textPrimary outline-hidden placeholder:text-faint"
    id="gitpulse-page-search"
  />
  {#if chip}
    <button type="button" class="gp-chip shrink-0" data-tone="accent" title={chip.title} onclick={chip.clear}>
      <span class="max-w-40 truncate">{chip.label}</span>
      <X size={11} aria-hidden="true" />
    </button>
  {/if}
  {#if find && value.trim()}
    {@const status = find.status()}
    <span class="shrink-0 px-1 tabular-nums text-micro {status.invalid ? 'text-danger' : 'text-textMuted'}" aria-live="polite" data-find-status>{status.text}</span>
  {/if}
  {#if find}
    <button type="button" class="gp-seg-btn shrink-0 px-1.5! text-micro" data-active={find.caseSensitive()} aria-pressed={find.caseSensitive()} onclick={() => find.setCaseSensitive(!find.caseSensitive())} title="Match case">Aa</button>
    <button type="button" class="gp-seg-btn shrink-0 px-1.5! font-mono text-micro" data-active={find.regex()} aria-pressed={find.regex()} onclick={() => find.setRegex(!find.regex())} title="Regular expression">.*</button>
    <button type="button" class="gp-icon-btn shrink-0" disabled={!find.status().hasMatches} onclick={() => find.step(-1)} title="Previous match (Shift+Enter)" aria-label="Previous match"><ChevronUp size={13} /></button>
    <button type="button" class="gp-icon-btn shrink-0" disabled={!find.status().hasMatches} onclick={() => find.step(1)} title="Next match (Enter)" aria-label="Next match"><ChevronDown size={13} /></button>
  {/if}
  {#if value}
    <button type="button" class="gp-icon-btn shrink-0" onclick={clear} title="Clear" aria-label="Clear search"><X size={13} /></button>
  {:else if !focused}
    <span class="gp-keycap mr-1.5 shrink-0 text-micro!" aria-hidden="true">{shortcutTextLabel("⌘F", $hostPlatform.os)}</span>
  {/if}
</div>

{#if Palette && input && pill}
  <Palette
    dropdown={{
      input,
      anchor: pill,
      query: paletteText,
      open: cardOpen,
      setQuery: (next) => {
        paletteText = next;
        input?.focus();
      },
      onclose: closeCard,
      onactive: (id) => (activeOption = id),
    }}
    onClone={() => window.dispatchEvent(new CustomEvent("gitpulse:clone"))}
    onRebase={() => window.dispatchEvent(new CustomEvent("gitpulse:rebase"))}
  />
{/if}
