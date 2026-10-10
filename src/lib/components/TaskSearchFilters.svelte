<script lang="ts">
  /**
   * The Tasks board's filters and what its search covered.
   *
   * Every dropdown writes into the query text (`setQualifier`, `setIsState`)
   * and reads back from it, so the text is the one source of truth: picking
   * a label types `label:ui`, and typing `label:ui` shows it picked. A query
   * the dropdowns cannot show as one value (a list, an exclusion) shows as
   * "Several" rather than as a choice that is not in force. Each filter,
   * phrase and exclusion is also a chip with its own remove button.
   *
   * Counts beside each value are the tasks that would match with that value
   * picked and the rest of the query unchanged (`facetCounts`).
   */
  import { X } from "@lucide/svelte";
  import { STATUS_LABELS, STATUSES } from "../workbench/vocabulary";
  import { fold } from "../workbench/taskLexicon";
  import {
    IS_GROUPS, PRIORITY_NAMES, isState, qualifierState, queryChips, removePart, setIsState, setQualifier,
    type IsValue, type ParsedQuery, type QualifierKey,
  } from "../workbench/taskQuery";
  import type { FacetCount, SearchOrder } from "../workbench/taskRank";

  let {
    value = $bindable(""), parsed, showSelects, counts, showRepo, report, relaxed = false, sortable, sort = $bindable("relevance"),
    canLoadMore = false, loading = false, onLoadMore,
  }: {
    value: string;
    parsed: ParsedQuery;
    /** The Filters panel is open. */
    showSelects: boolean;
    counts: Partial<Record<"repo" | "kind" | "owner" | "label" | "severity", FacetCount[]>>;
    /** The board spans more than one repository. */
    showRepo: boolean;
    /** What the search covered (`describeSearch`), or null while browsing. */
    report: string | null;
    relaxed?: boolean;
    /** The query has words, so Relevance can reorder the board. */
    sortable: boolean;
    sort: SearchOrder;
    canLoadMore?: boolean;
    loading?: boolean;
    onLoadMore: () => void;
  } = $props();

  const MANY = "__several__";
  const chips = $derived(queryChips(value));

  /** The select value for a key: "all", a listed value, the typed value, or Several. */
  function pick(key: QualifierKey, listed: readonly FacetCount[]): string {
    const state = qualifierState(parsed, key);
    if (state.kind !== "one") return state.kind === "all" ? "all" : MANY;
    return listed.find((option) => fold(option.value) === fold(state.value))?.value ?? state.value;
  }
  /** Listed values, plus the one in force when nothing loaded carries it. */
  function listed(key: "repo" | "kind" | "owner" | "label" | "severity"): FacetCount[] {
    const list = counts[key] ?? [];
    const state = qualifierState(parsed, key);
    if (state.kind === "one" && !list.some((option) => fold(option.value) === fold(state.value))) return [...list, { value: state.value, count: 0 }];
    return list;
  }
  function write(key: QualifierKey, choice: string) {
    if (choice === MANY) return;
    // No value is a cleared select, not a filter for the empty string.
    value = setQualifier(value, key, choice === "all" || !choice.trim() ? null : choice);
  }
  function writeIs(group: readonly IsValue[], choice: string) {
    if (choice === MANY) return;
    value = setIsState(value, group, choice === "all" ? null : choice as IsValue);
  }
  const priority = $derived.by(() => {
    const state = qualifierState(parsed, "priority");
    return state.kind === "one" ? state.value : state.kind === "all" ? "all" : MANY;
  });
  const owner = $derived.by(() => {
    const named = pick("owner", listed("owner"));
    const state = isState(parsed, IS_GROUPS.owner);
    if (named !== "all") return state.kind === "all" ? named : MANY;
    if (state.kind === "all") return "all";
    return state.kind === "one" ? (state.value === "unassigned" ? "" : "__assigned") : MANY;
  });
  function writeOwner(choice: string) {
    if (choice === MANY) return;
    const cleared = setIsState(setQualifier(value, "owner", null), IS_GROUPS.owner, null);
    value = choice === "all" ? cleared
      : choice === "" ? setIsState(cleared, IS_GROUPS.owner, "unassigned")
        : choice === "__assigned" ? setIsState(cleared, IS_GROUPS.owner, "assigned")
          : setQualifier(cleared, "owner", choice);
  }
  const due = $derived.by(() => {
    const state = isState(parsed, IS_GROUPS.due);
    return state.kind === "one" ? state.value : state.kind === "all" ? "all" : MANY;
  });
  const label = (option: FacetCount) => `${option.value} (${option.count})`;
</script>

{#if showSelects || chips.length || report}
  <div class="facets" aria-label="Organize tasks">
    {#if showSelects}
      <select class="gp-select" aria-label="Filter by priority" value={priority} onchange={(e) => { const v = e.currentTarget.value; if (v !== MANY) value = setQualifier(value, "priority", v === "all" ? null : PRIORITY_NAMES[Number(v)]); }}>
        <option value="all">All priorities</option>
        {#if priority === MANY}<option value={MANY} disabled>Several priorities</option>{/if}
        <option value="0">Urgent</option>
        <option value="1">High</option>
        <option value="2">Normal</option>
        <option value="3">Low</option>
      </select>
      <select class="gp-select" aria-label="Filter by status" value={pick("status", STATUSES.map((s) => ({ value: s, count: 0 })))} onchange={(e) => write("status", e.currentTarget.value)}>
        <option value="all">All columns</option>
        {#if pick("status", []) === MANY}<option value={MANY} disabled>Several columns</option>{/if}
        {#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}
      </select>
      {#if showRepo}
        <select class="gp-select" aria-label="Filter by repository" value={pick("repo", listed("repo"))} onchange={(e) => write("repo", e.currentTarget.value)}>
          <option value="all">All repositories</option>
          {#if pick("repo", listed("repo")) === MANY}<option value={MANY} disabled>Several repositories</option>{/if}
          {#each listed("repo") as option (option.value)}<option value={option.value}>{label(option)}</option>{/each}
        </select>
      {/if}
      <select class="gp-select" aria-label="Filter by type" value={pick("kind", listed("kind"))} onchange={(e) => write("kind", e.currentTarget.value)}>
        <option value="all">All types</option>
        {#if pick("kind", listed("kind")) === MANY}<option value={MANY} disabled>Several types</option>{/if}
        {#each listed("kind") as option (option.value)}<option value={option.value}>{label(option)}</option>{/each}
      </select>
      <select class="gp-select" aria-label="Filter by owner" value={owner} onchange={(e) => writeOwner(e.currentTarget.value)}>
        <option value="all">All owners</option>
        {#if owner === MANY}<option value={MANY} disabled>Several owners</option>{/if}
        <option value="">Unassigned</option>
        <option value="__assigned">Anyone assigned</option>
        {#each listed("owner") as option (option.value)}<option value={option.value}>{label(option)}</option>{/each}
      </select>
      <select class="gp-select" aria-label="Filter by label" value={pick("label", listed("label"))} onchange={(e) => write("label", e.currentTarget.value)}>
        <option value="all">All labels</option>
        {#if pick("label", listed("label")) === MANY}<option value={MANY} disabled>Several labels</option>{/if}
        {#each listed("label") as option (option.value)}<option value={option.value}>{label(option)}</option>{/each}
      </select>
      {#if listed("severity").length}
        <select class="gp-select" aria-label="Filter by severity" value={pick("severity", listed("severity"))} onchange={(e) => write("severity", e.currentTarget.value)}>
          <option value="all">Any severity</option>
          {#if pick("severity", listed("severity")) === MANY}<option value={MANY} disabled>Several severities</option>{/if}
          {#each listed("severity") as option (option.value)}<option value={option.value}>{label(option)}</option>{/each}
        </select>
      {/if}
      <select class="gp-select" aria-label="Filter by due date" value={due} onchange={(e) => writeIs(IS_GROUPS.due, e.currentTarget.value)}>
        <option value="all">Any due date</option>
        {#if due === MANY}<option value={MANY} disabled>Several due dates</option>{/if}
        <option value="overdue">Overdue</option>
        <option value="soon">Due soon</option>
        <option value="due">Has a due date</option>
        <option value="no-due">No due date</option>
      </select>
    {/if}
    {#each chips as chip (chip.index + chip.label)}
      <span class="chip" class:negated={chip.negated} data-testid="task-search-chip">
        {#if chip.negated}<span class="sr-only">Excluding </span><span aria-hidden="true">not </span>{/if}{chip.label}
        <button type="button" aria-label={`Remove ${chip.negated ? "exclusion " : ""}${chip.label}`} onclick={() => { value = removePart(value, chip.index); }}><X size={10} /></button>
      </span>
    {/each}
    {#if value.trim()}<button type="button" class="gp-btn" onclick={() => { value = ""; }}>Clear filters</button>{/if}
  </div>
{/if}
{#if report}
  <div class="report" data-testid="task-search-report">
    <p role="status" aria-live="polite" class:relaxed>{report}</p>
    {#if sortable}
      <div class="gp-segmented" role="group" aria-label="Order of matching tasks">
        <button type="button" class="gp-seg-btn" data-active={sort === "relevance"} aria-pressed={sort === "relevance"} title="Best match first; dragging to reorder is off" onclick={() => { sort = "relevance"; }}>Relevance</button>
        <button type="button" class="gp-seg-btn" data-active={sort === "board"} aria-pressed={sort === "board"} title="The board's own order; cards can be dragged" onclick={() => { sort = "board"; }}>Board order</button>
      </div>
    {/if}
    {#if canLoadMore}<button type="button" class="gp-btn" onclick={onLoadMore} disabled={loading}>Load more tasks</button>{/if}
  </div>
{/if}

<style>
  .facets{display:flex;gap:6px;align-items:center;flex-wrap:wrap;padding:8px 14px}
  button,select{font-size:12px}
  button:disabled{opacity:.5}
  .chip{display:inline-flex;align-items:center;gap:4px;padding:2px 4px 2px 8px;border-radius:999px;font-size:11px;border:1px solid rgb(var(--c-border) / 0.8);color:rgb(var(--c-text))}
  .chip.negated{border-style:dashed;color:rgb(var(--c-text-muted))}
  .chip button{display:inline-flex;align-items:center;justify-content:center;width:14px;height:14px;border-radius:999px;color:rgb(var(--c-text-muted))}
  .chip button:hover{background:rgb(var(--c-surface-hover) / 0.8)}
  .report{display:flex;align-items:center;gap:8px;flex-wrap:wrap;padding:0 14px 8px}
  .report p{margin:0;flex:1;min-width:200px;font-size:11px;line-height:1.4;color:rgb(var(--c-text-muted))}
  .report p.relaxed{color:rgb(var(--c-text))}
</style>
