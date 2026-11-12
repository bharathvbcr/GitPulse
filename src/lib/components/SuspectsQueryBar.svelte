<script lang="ts">
  /**
   * The Suspects list's question, in History's context bar where the commit
   * filter sits for the other two lists.
   *
   * The filter does not apply here: the ranking is one answer whose scores
   * are comparable only within it, so narrowing it by text would hand the
   * reader a list whose order means something it does not. ⌘F, which focuses
   * the view bar's search field (the commit filter) on the other lists,
   * focuses the symptom field instead (findChordTarget in views/commitFilter).
   */
  import { tick } from "svelte";
  import { Loader2, Search } from "@lucide/svelte";
  import { currentRepoPath } from "../stores/repoStore";
  import {
    suspectsQuery,
    canRunSuspects,
    DEFAULT_SUSPECTS_SINCE,
    MAX_SUSPECT_CONE_DEPTH,
    MAX_SYMPTOM_LENGTH,
  } from "../codeintel/suspectsQuery";
  import { FOCUS_SUSPECTS_SYMPTOM_EVENT } from "../views/commitFilter";

  let symptomInput = $state<HTMLInputElement | null>(null);

  const repoPath = $derived($currentRepoPath);
  const canRun = $derived(canRunSuspects($suspectsQuery, repoPath));

  function focusSymptom() {
    symptomInput?.focus();
    symptomInput?.select();
  }

  $effect(() => {
    window.addEventListener(FOCUS_SUSPECTS_SYMPTOM_EVENT, focusSymptom);
    return () => window.removeEventListener(FOCUS_SUSPECTS_SYMPTOM_EVENT, focusSymptom);
  });

  // "Find suspects since this commit" with no symptom yet asks for this field,
  // usually in the same click that switched Graph onto this list and mounted it.
  $effect(() => {
    if (!$suspectsQuery.focusPending || !symptomInput) return;
    suspectsQuery.consumeFocus();
    void tick().then(focusSymptom);
  });
</script>

<form
  class="flex min-w-0 flex-1 items-center gap-2"
  aria-label="Regression suspects query"
  onsubmit={(event) => {
    event.preventDefault();
    void suspectsQuery.run(repoPath);
  }}
>
  <input
    bind:this={symptomInput}
    type="text"
    value={$suspectsQuery.symptom}
    oninput={(e) => suspectsQuery.setInputs({ symptom: e.currentTarget.value })}
    maxlength={MAX_SYMPTOM_LENGTH}
    autocomplete="off"
    spellcheck="false"
    aria-label="Symptom symbol"
    title="The symbol that is misbehaving: module::function, Class.method, or a file path"
    placeholder="Symptom: module::function, Class.method, or a file path"
    class="gp-field h-7 min-w-0 flex-1 font-mono text-meta"
  />
  <input
    type="text"
    value={$suspectsQuery.since}
    oninput={(e) => suspectsQuery.setInputs({ since: e.currentTarget.value })}
    autocomplete="off"
    spellcheck="false"
    aria-label="Last known good"
    title="The last ref the symptom was well at. The window runs from here to the indexed commit, not to HEAD."
    placeholder={DEFAULT_SUSPECTS_SINCE}
    class="gp-field h-7 w-32 shrink-0 font-mono text-meta"
  />
  <input
    type="number"
    min="1"
    max={MAX_SUSPECT_CONE_DEPTH}
    value={$suspectsQuery.depth}
    oninput={(e) => suspectsQuery.setInputs({ depth: e.currentTarget.valueAsNumber })}
    aria-label="Cone depth"
    title="How many call edges out from the symptom to walk (1–{MAX_SUSPECT_CONE_DEPTH})"
    class="gp-field h-7 w-14 shrink-0 font-mono text-meta tabular-nums"
  />
  <button type="submit" class="gp-btn h-7 shrink-0" disabled={!canRun}>
    {#if $suspectsQuery.loading}
      <Loader2 size={13} class="animate-spin" />
    {:else}
      <Search size={13} />
    {/if}
    <span>Find suspects</span>
  </button>
</form>
