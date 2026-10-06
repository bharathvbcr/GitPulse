<script lang="ts">
  /**
   * Which model each agent CLI starts with when GitPulse starts it in a
   * terminal — a task terminal attempt and a new agent tab alike.
   *
   * Every field empty is the CLI's own choice (its settings files, its
   * environment, its default) and is stored as absence. The controls a row
   * shows come from the backend's flag table (`model_fields`), so a field a
   * CLI does not take is never offered: Codex and Grok list no reasoning
   * levels in their help, so a level for them could not be proved before a
   * launch and is not offered.
   *
   * Names are checked for shape here and again by the backend, never for
   * existence: Claude Code accepts any full model name and Antigravity's list
   * is per account. What the CLI does with an unknown name is the CLI's, and
   * the note says what that is.
   *
   * The names a field suggests come from `cmd_agent_models`: Antigravity's
   * own `agy models` (asked only when the reader presses the button — it is a
   * network call under their sign-in), and for Claude Code its aliases plus
   * the models its settings name, read when the pane opens (a local file).
   *
   * Its own catalog row, for the same reason as the setting sources: the
   * modal hides each entry's wrapper by search. A save carries the other
   * agent defaults through unchanged.
   */
  import { onMount } from "svelte";
  import { AlertTriangle } from "@lucide/svelte";
  import { agentDefaultsStore, loadAgentDefaults, saveAgentDefaults } from "../stores/agentDefaultsStore";
  import {
    MAX_FALLBACK_MODELS,
    MODEL_SUGGESTIONS,
    isEffortLevel,
    isModelId,
    modelChoiceOf,
    parseFallbackList,
    withModelChoice,
    type ModelChoice,
    type ModelField,
  } from "../terminal/agentDefaults";
  import { LAUNCHERS, type LauncherKind } from "../terminal/tabs";
  import { agentModelCatalogs, listedModel, loadAgentModels, type AgentModelCatalog, type CatalogState } from "../terminal/agentModelCatalog";
  import { formatError } from "../ui/formatError";

  let { active = true }: { active?: boolean } = $props();

  let busy = $state(false);
  let errors = $state<Partial<Record<LauncherKind, string>>>({});
  let loaded = false;
  /** Launchers whose settings-only catalog was already read this mount. */
  const autoLoaded = new Set<LauncherKind>();

  const view = $derived($agentDefaultsStore);
  const catalogs = $derived($agentModelCatalogs);
  /** Launchers with at least one model control, in the tab strip's order. */
  const rows = $derived(
    LAUNCHERS.filter((entry) => (view.modelFields[entry.kind]?.length ?? 0) > 0).map((entry) => ({
      kind: entry.kind,
      label: entry.label,
      fields: view.modelFields[entry.kind] ?? [],
    })),
  );

  const FIELD_LABELS: Record<ModelField, string> = {
    model: "Model",
    effort: "Effort",
    fallback: "Fallback models",
    advisor: "Advisor",
  };

  onMount(() => {
    if (active && !loaded) {
      loaded = true;
      void loadAgentDefaults();
    }
  });

  $effect(() => {
    if (active && !loaded) {
      loaded = true;
      void loadAgentDefaults();
    }
  });

  // A `known` catalog is a local read with no process and no network, so it
  // is fetched when the pane is shown. A `listed` one waits for the button.
  $effect(() => {
    if (!active) return;
    for (const [launcher, how] of Object.entries(view.modelListing)) {
      const kind = launcher as LauncherKind;
      if (how === "known" && !autoLoaded.has(kind)) {
        autoLoaded.add(kind);
        void loadAgentModels(kind);
      }
    }
  });

  /** The list to suggest from: the current answer, or the last good one while loading or after a failure. */
  function shownCatalog(state: CatalogState | undefined): AgentModelCatalog | null {
    if (!state) return null;
    return state.status === "ready" ? state.catalog : state.previous;
  }

  function suggestionsFor(launcher: LauncherKind, field: ModelField): { id: string; label: string | null }[] {
    if (field === "model" || field === "fallback") {
      const catalog = shownCatalog(catalogs[launcher]);
      if (catalog) return catalog.models;
    }
    return (MODEL_SUGGESTIONS[launcher]?.[field] ?? []).map((id) => ({ id, label: null }));
  }

  function timeOf(ms: number): string {
    return ms > 0 ? new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : "";
  }

  /** One line saying what the list is, where it came from, and what went wrong. */
  function catalogStatus(launcher: LauncherKind): { text: string; problem: boolean } | null {
    const state = catalogs[launcher];
    if (!state) return null;
    if (state.status === "loading") return { text: "Asking for the model list…", problem: false };
    if (state.status === "failed") return { text: `Could not list models: ${state.message}`, problem: true };
    const { catalog } = state;
    if (catalog.error) return { text: catalog.error, problem: true };
    const count = `${catalog.models.length}${catalog.truncated ? "+" : ""} model${catalog.models.length === 1 ? "" : "s"}`;
    const skipped = catalog.skipped ? `, ${catalog.skipped} unreadable skipped` : "";
    if (catalog.listing === "listed") {
      return { text: `${count} from ${catalog.command ?? "the CLI"}${skipped}, ${catalog.cached ? "cached" : "fetched"} ${timeOf(catalog.fetched_at)}.`, problem: false };
    }
    return { text: `${count}: the CLI's aliases and the models your Claude settings name${skipped}. Claude Code publishes no model list.`, problem: false };
  }

  /** Why `text` cannot be stored for `field`, or `""` when it can. */
  function refusal(field: ModelField, text: string): string {
    if (text === "") return "";
    if (field === "fallback") {
      const list = parseFallbackList(text);
      if (list.length > MAX_FALLBACK_MODELS) return `Name at most ${MAX_FALLBACK_MODELS} fallback models.`;
      const bad = list.find((id) => !isModelId(id));
      if (bad !== undefined) return `"${bad}" is not a model name.`;
      if (new Set(list).size !== list.length) return "A fallback model is listed twice.";
      return "";
    }
    if (field === "effort") return isEffortLevel(text) ? "" : `"${text}" is not an effort level.`;
    return isModelId(text)
      ? ""
      : `"${text}" is not a model name: start with a letter or digit, then letters, digits and . _ : / @ [ ] -`;
  }

  function valueOf(choice: ModelChoice, field: ModelField): string {
    if (field === "fallback") return (choice.fallback ?? []).join(", ");
    return choice[field] ?? "";
  }

  async function commit(launcher: LauncherKind, field: ModelField, input: HTMLInputElement | HTMLSelectElement) {
    const current = modelChoiceOf(view.defaults, launcher);
    const text = input.value.trim();
    if (text === valueOf(current, field)) return;
    const problem = refusal(field, text);
    if (problem) {
      errors = { ...errors, [launcher]: problem };
      return;
    }
    const next: ModelChoice = { ...current };
    if (field === "fallback") next.fallback = text === "" ? undefined : parseFallbackList(text);
    else if (field === "effort") next.effort = text === "" || !isEffortLevel(text) ? undefined : text;
    else next[field] = text === "" ? undefined : text;
    errors = { ...errors, [launcher]: "" };
    busy = true;
    try {
      await saveAgentDefaults(withModelChoice(view.defaults, launcher, next));
    } catch (err) {
      errors = { ...errors, [launcher]: formatError(err) };
      input.value = valueOf(current, field);
    } finally {
      busy = false;
    }
  }

  function onKey(event: KeyboardEvent, launcher: LauncherKind, field: ModelField) {
    if (event.key === "Enter") {
      event.preventDefault();
      void commit(launcher, field, event.currentTarget as HTMLInputElement);
    }
  }
</script>

<fieldset>
  <legend class="text-textPrimary text-[11px]">Agent models</legend>
  {#if rows.length === 0}
    <p class="text-textMuted text-[10px] leading-snug mt-1" data-testid="agent-models-unavailable">
      This build of GitPulse's backend does not offer model settings.
    </p>
  {:else}
    <div class="mt-1.5 flex flex-col gap-2.5" data-testid="agent-models">
      {#each rows as row (row.kind)}
        {@const choice = modelChoiceOf(view.defaults, row.kind)}
        <div class="flex flex-col gap-1" data-testid={`agent-models-${row.kind}`}>
          <span class="text-[11px] text-textPrimary font-medium">{row.label}</span>
          <div class="grid grid-cols-[minmax(0,7rem)_minmax(0,1fr)] items-center gap-x-2 gap-y-1">
            {#each row.fields as field (field)}
              {@const id = `agent-model-${row.kind}-${field}`}
              <label for={id} class="text-[10px] text-textMuted">{FIELD_LABELS[field]}</label>
              {#if field === "effort"}
                <select
                  {id}
                  class="gp-input text-[11px] py-0.5! min-w-0"
                  data-testid={id}
                  disabled={busy}
                  value={choice.effort ?? ""}
                  onchange={(event) => void commit(row.kind, field, event.currentTarget)}
                >
                  <option value="">CLI default</option>
                  {#each view.effortLevels as level (level)}
                    <option value={level}>{level}</option>
                  {/each}
                </select>
              {:else}
                {@const suggestions = suggestionsFor(row.kind, field)}
                <input
                  {id}
                  type="text"
                  spellcheck="false"
                  autocomplete="off"
                  class="gp-input text-[11px] py-0.5! min-w-0 font-mono"
                  data-testid={id}
                  disabled={busy}
                  placeholder={field === "advisor" ? "Off unless your settings turn it on" : field === "fallback" ? "None" : "CLI default"}
                  list={suggestions.length ? `${id}-suggestions` : undefined}
                  value={valueOf(choice, field)}
                  onchange={(event) => void commit(row.kind, field, event.currentTarget)}
                  onkeydown={(event) => onKey(event, row.kind, field)}
                />
                {#if suggestions.length}
                  <datalist id={`${id}-suggestions`}>
                    {#each suggestions as suggestion (suggestion.id)}
                      <option value={suggestion.id}>{suggestion.label ?? ""}</option>
                    {/each}
                  </datalist>
                {/if}
              {/if}
            {/each}
          </div>
          {#if view.modelListing[row.kind]}
            {@const status = catalogStatus(row.kind)}
            {@const listed = view.modelListing[row.kind] === "listed"}
            {@const known = listedModel(catalogs[row.kind], choice.model)}
            <div class="flex items-start gap-2 text-[10px]">
              {#if status}
                <span class={status.problem ? "text-amber-600 dark:text-amber-400" : "text-textMuted"} data-testid={`agent-models-${row.kind}-catalog`}>{status.text}</span>
              {/if}
              {#if listed}
                <button
                  type="button"
                  class="ml-auto shrink-0 rounded border border-border px-1.5 py-px text-[10px] text-textPrimary hover:bg-bgSecondary disabled:opacity-50"
                  data-testid={`agent-models-${row.kind}-list`}
                  disabled={catalogs[row.kind]?.status === "loading"}
                  onclick={() => void loadAgentModels(row.kind, shownCatalog(catalogs[row.kind]) !== null)}
                >
                  {shownCatalog(catalogs[row.kind]) ? "Refresh models" : "List models"}
                </button>
              {/if}
            </div>
            {#if listed && known === false}
              <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px]" data-testid={`agent-models-${row.kind}-unlisted`}>
                <AlertTriangle size={12} class="shrink-0 mt-px" />
                <span>"{choice.model}" is not in the list your account returned. Antigravity would start with its default model instead.</span>
              </div>
            {/if}
          {/if}
          {#if errors[row.kind]}
            <div class="flex items-start gap-1.5 text-amber-600 dark:text-amber-400 text-[10px]">
              <AlertTriangle size={12} class="shrink-0 mt-px" />
              <span data-testid={`agent-models-${row.kind}-error`}>{errors[row.kind]}</span>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
  <p class="text-textMuted text-[10px] leading-snug mt-1.5" data-testid="agent-models-note">
    The model each agent starts with when GitPulse starts it in a terminal, for task attempts and new agent tabs alike. Empty means the CLI's own choice.
    A task attempt first checks that the installed CLI offers each control (and the effort level you picked), and refuses to start if it does not.
    GitPulse checks that a name is well-formed, not that the model exists — the CLI reports a model it cannot use in the session.
    Antigravity does not stop for an unknown name: it falls back to its default model and prints a warning in the terminal. Its names are the ones <span class="font-mono">agy models</span> lists.
    Claude's advisor needs access to the advisor model, and Claude Code reports a pairing it cannot use (the advisor must be at least as capable as the main model).
    Managed sessions are not affected: Manvi starts those with the CLI's own model.
  </p>
</fieldset>
