<script lang="ts">
  /**
   * The Tasks board's search box: the query text, with completions for the
   * search language (`taskQuery.ts`) under it.
   *
   * Typing `la` offers `label:`; typing `label:` offers the labels on the
   * loaded tasks; status, priority, due and the board's `is:` words are
   * offered whatever is loaded. Arrow keys move, Enter or Tab take a
   * completion, Escape closes the list, then clears the text, then leaves the
   * box. Nothing here fetches: the board decides what a search loads.
   */
  import { Search } from "@lucide/svelte";
  import { LAYERS } from "../ui/layers";
  import { MAX_QUERY_TEXT, replaceToken, suggest, tokenAtCaret, type QuerySuggestion, type SuggestionSources } from "../workbench/taskQuery";

  let { id, value = $bindable(""), sources, busy = false, describedby, title }: {
    id: string;
    value: string;
    sources: SuggestionSources;
    /** Candidates are loading for this search. */
    busy?: boolean;
    /** The id of the board's own description of the syntax. */
    describedby: string;
    title: string;
  } = $props();

  let input: HTMLInputElement | undefined = $state();
  let caret = $state(0);
  let focused = $state(false);
  let dismissed = $state(false);
  let active = $state(-1);

  const at = $derived(focused ? tokenAtCaret(value, caret) : null);
  const options = $derived<QuerySuggestion[]>(at && !dismissed ? suggest(at.prefix, sources) : []);
  const open = $derived(options.length > 0);
  const listId = $derived(`${id}-suggestions`);
  // A new list starts with nothing chosen, so Enter never takes one by accident.
  $effect(() => { options; active = -1; });

  function track() {
    caret = input?.selectionStart ?? value.length;
    dismissed = false;
  }
  function take(option: QuerySuggestion) {
    if (!at) return;
    const next = replaceToken(value, at.index, option.insert);
    value = next.text;
    queueMicrotask(() => {
      input?.focus();
      input?.setSelectionRange(next.caret, next.caret);
      caret = next.caret;
    });
  }
  function onkeydown(e: KeyboardEvent) {
    if (open && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      e.preventDefault();
      // -1 is the text itself: past either end the list lets go of the choice.
      if (e.key === "ArrowDown") active = active + 1 >= options.length ? -1 : active + 1;
      else active = active <= -1 ? options.length - 1 : active - 1;
      return;
    }
    if (open && active >= 0 && (e.key === "Enter" || e.key === "Tab")) {
      e.preventDefault();
      take(options[active]);
      return;
    }
    if (open && e.key === "Tab" && !e.shiftKey && options.length === 1) {
      e.preventDefault();
      take(options[0]);
      return;
    }
    if (e.key === "Escape") {
      e.stopPropagation();
      if (open) { dismissed = true; return; }
      if (value) { e.preventDefault(); value = ""; return; }
      input?.blur();
    }
  }
</script>

<div class="field" data-task-search-field>
  <Search size={12} aria-hidden="true" />
  <input
    bind:this={input}
    {id}
    class="gp-field"
    type="search"
    role="combobox"
    aria-label="Search tasks"
    aria-autocomplete="list"
    aria-expanded={open}
    aria-controls={listId}
    aria-activedescendant={open && active >= 0 ? `${listId}-${active}` : undefined}
    aria-describedby={describedby}
    aria-busy={busy}
    placeholder="Search — is:bug repo:name"
    {title}
    maxlength={MAX_QUERY_TEXT}
    autocomplete="off"
    spellcheck="false"
    bind:value
    oninput={track}
    onclick={track}
    onkeyup={(e) => { if (e.key.startsWith("Arrow") && !open) track(); }}
    onfocus={() => { focused = true; track(); }}
    onblur={() => { focused = false; }}
    {onkeydown}
  />
  {#if open}
    <ul id={listId} class="list gp-menu gp-pop" role="listbox" aria-label="Search completions" style="z-index: {LAYERS.MENU}">
      {#each options as option, index (option.insert)}
        <li
          id={`${listId}-${index}`}
          role="option"
          aria-selected={index === active}
          class:active={index === active}
          onpointerdown={(e) => { e.preventDefault(); take(option); }}
        >
          <span class="label">{option.label}</span>
          {#if option.detail}<span class="detail">{option.detail}</span>{/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .field{position:relative;display:flex;align-items:center;gap:6px;color:rgb(var(--c-text-muted))}
  .field input{width:200px}
  .list{position:absolute;top:calc(100% + 4px);left:18px;min-width:220px;max-width:320px;margin:0;padding:4px;list-style:none}
  .list li{display:flex;justify-content:space-between;gap:12px;padding:4px 6px;border-radius:5px;font-size:11px;cursor:pointer;color:rgb(var(--c-text))}
  .list li:hover,.list li.active{background:rgb(var(--c-surface-hover) / 0.8)}
  .list li.active{outline:1px solid rgb(var(--c-accent) / 0.5)}
  .label{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
  .detail{flex:none;color:rgb(var(--c-text-muted))}
</style>
