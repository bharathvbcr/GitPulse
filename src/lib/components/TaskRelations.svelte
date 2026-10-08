<script lang="ts">
  /**
   * A task's checklist and its links to other tasks, in the task sheet.
   *
   * Both are stored fields (dc-store schema 11), not a convention in the
   * description: a checklist entry has a done state of its own, and a link
   * names another task by id, so it survives that task being renamed and is
   * read from both ends — the other task's brief says "Blocked by" or
   * "Subtask" for a link written here.
   *
   * The store checks what a link may be: a live task other than this one, at
   * most one parent, and no parent that is already beneath this task. This
   * sheet offers only live tasks and one parent, and leaves the cycle to the
   * store's refusal, which names it.
   */
  import { untrack } from "svelte";
  import { Link2, Plus, X } from "@lucide/svelte";
  import {
    LINK_KINDS, MAX_CHECKLIST, MAX_CHECKLIST_TEXT, MAX_TASK_LINKS,
    WorkbenchError, explainError, getTask, listTasks,
    type ChecklistEntry, type LinkKind, type TaskCard, type TaskLink,
  } from "../workbench/client";

  let { checklist = $bindable(), links = $bindable(), taskId = null, disabled = false, onchange }: {
    checklist: ChecklistEntry[];
    links: TaskLink[];
    /** The task being edited; null for one not saved yet, which has no id to exclude. */
    taskId?: string | null;
    disabled?: boolean;
    onchange: () => void;
  } = $props();

  /** How a link reads from this task. */
  const KIND_LABELS: Record<LinkKind, string> = {
    parent: "Subtask of",
    blocks: "Blocks",
    related: "Related to",
    duplicate_of: "Duplicate of",
  };

  let entry = $state("");
  let kind = $state<LinkKind>("related");
  let query = $state("");
  let results = $state<TaskCard[]>([]);
  let searching = $state(false), error = $state("");
  /** Titles for linked ids; `null` once a lookup found the task deleted. */
  let titles = $state<Record<string, string | null>>({});

  const done = $derived(checklist.filter((item) => item.done).length);
  const hasParent = $derived(links.some((link) => link.kind === "parent"));

  /** Ids already asked for, so a lookup in flight is not asked for again. Not state: nothing renders it. */
  const requested = new Set<string>();
  function name(id: string, title: string | null) { titles = { ...titles, [id]: title }; }
  function resolve(id: string) {
    requested.add(id);
    void getTask(id).then(
      (task) => name(id, task.title),
      // A deleted target is named as one; any other failure leaves the id on screen.
      (cause) => name(id, cause instanceof WorkbenchError && cause.code === "not_found" ? null : id),
    );
  }
  // Resolve titles for links whose target this sheet has not asked about yet.
  $effect(() => {
    for (const link of links) if (!requested.has(link.item_id)) untrack(() => resolve(link.item_id));
  });

  function addEntry() {
    const text = entry.trim();
    if (!text || checklist.length >= MAX_CHECKLIST || new TextEncoder().encode(text).length > MAX_CHECKLIST_TEXT) return;
    checklist = [...checklist, { text, done: false }];
    entry = "";
    onchange();
  }
  function toggle(index: number, value: boolean) {
    checklist = checklist.map((item, at) => at === index ? { ...item, done: value } : item);
    onchange();
  }
  function removeEntry(index: number) {
    checklist = checklist.filter((_, at) => at !== index);
    onchange();
  }

  async function search() {
    const text = query.trim();
    if (!text) { results = []; return; }
    searching = true; error = "";
    try {
      const page = await listTasks({ kind: "global" }, null, text, undefined, 8);
      results = page.items.filter((card) => card.id !== taskId);
    } catch (cause) { error = explainError(cause); }
    finally { searching = false; }
  }
  function link(card: TaskCard) {
    if (links.length >= MAX_TASK_LINKS) { error = `A task holds at most ${MAX_TASK_LINKS} links.`; return; }
    if (kind === "parent" && hasParent) { error = "A task has one parent. Remove the current one first."; return; }
    if (links.some((link) => link.kind === kind && link.item_id === card.id)) return;
    requested.add(card.id); name(card.id, card.title);
    links = [...links, { kind, item_id: card.id }];
    results = []; query = ""; error = "";
    onchange();
  }
  function unlink(index: number) {
    links = links.filter((_, at) => at !== index);
    onchange();
  }
</script>

<section class="relations" aria-label="Checklist and linked tasks" data-testid="task-relations">
  <div class="block">
    <div class="head"><span>Checklist</span>{#if checklist.length}<span class="count" data-testid="task-checklist-count">{done} of {checklist.length} done</span>{/if}</div>
    {#if checklist.length}
      <ul data-testid="task-checklist">
        {#each checklist as item, index (index)}
          <li>
            <label><input type="checkbox" checked={item.done} {disabled} onchange={(e) => toggle(index, e.currentTarget.checked)} /> <span class:done={item.done}>{item.text}</span></label>
            <button type="button" class="remove" {disabled} aria-label="Remove checklist item: {item.text}" onclick={() => removeEntry(index)}><X size={12} /></button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="add">
      <input class="gp-field" data-testid="task-checklist-add" bind:value={entry} {disabled} maxlength={MAX_CHECKLIST_TEXT} placeholder="Add a checklist item…" onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); addEntry(); } }} />
      <button type="button" class="gp-btn" onclick={addEntry} disabled={disabled || !entry.trim() || checklist.length >= MAX_CHECKLIST}><Plus size={12} /> Add</button>
    </div>
  </div>

  <div class="block">
    <div class="head"><span>Linked tasks</span>{#if links.length}<span class="count">{links.length}</span>{/if}</div>
    {#if links.length}
      <ul data-testid="task-links">
        {#each links as entry_, index (`${entry_.kind}:${entry_.item_id}`)}
          <li>
            <span class="kind">{KIND_LABELS[entry_.kind]}</span>
            <span class="title" class:gone={titles[entry_.item_id] === null}>{titles[entry_.item_id] === null ? `${entry_.item_id} (deleted)` : titles[entry_.item_id] ?? entry_.item_id}</span>
            <button type="button" class="remove" {disabled} aria-label="Remove link to {titles[entry_.item_id] ?? entry_.item_id}" onclick={() => unlink(index)}><X size={12} /></button>
          </li>
        {/each}
      </ul>
    {/if}
    <div class="add">
      <select class="gp-select" aria-label="Link kind" bind:value={kind} {disabled}>
        {#each LINK_KINDS as option (option)}<option value={option} disabled={option === "parent" && hasParent}>{KIND_LABELS[option]}</option>{/each}
      </select>
      <input class="gp-field" type="search" data-testid="task-link-search" bind:value={query} {disabled} maxlength="512" placeholder="Find a task by title…" onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); void search(); } }} />
      <button type="button" class="gp-btn" disabled={disabled || searching || !query.trim()} onclick={() => void search()}><Link2 size={12} /> Find</button>
    </div>
    {#if results.length}
      <ul class="results" data-testid="task-link-results">
        {#each results as card (card.id)}
          <li><button type="button" class="pick" {disabled} onclick={() => link(card)}>{card.title}</button></li>
        {/each}
      </ul>
    {/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
</section>

<style>
  .relations{display:flex;flex-direction:column;gap:12px;margin-bottom:13px}
  .block{display:flex;flex-direction:column;gap:6px}
  .head{display:flex;align-items:baseline;justify-content:space-between;font-weight:500}
  .count{font-weight:400;color:rgb(var(--c-text-muted));font-size:11px}
  ul{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:4px;max-height:160px;overflow-y:auto}
  li{display:flex;align-items:center;gap:8px;padding:4px 8px;border-radius:6px;border:1px solid rgb(var(--c-border) / 0.5);font-size:11px}
  li label{display:flex;align-items:center;gap:6px;flex:1;min-width:0}
  li label span,.title{overflow-wrap:anywhere;flex:1;min-width:0}
  .done{text-decoration:line-through;color:rgb(var(--c-text-muted))}
  .gone{color:rgb(var(--c-text-muted));font-style:italic}
  .kind{color:rgb(var(--c-text-muted));flex-shrink:0}
  .remove{background:transparent;border:0;cursor:pointer;padding:2px;display:inline-flex;color:rgb(var(--c-text-muted));border-radius:4px}
  .remove:hover{color:rgb(var(--c-text))}
  .add{display:flex;gap:6px;align-items:center}
  .add input{flex:1;font-size:11px;padding:5px 8px}
  .add button,.add select{flex-shrink:0;font-size:11px;padding:5px 10px}
  .results .pick{background:transparent;border:0;cursor:pointer;color:inherit;text-align:left;width:100%;font:inherit}
  .error{color:#dc6565;margin:0;font-size:11px}
</style>
