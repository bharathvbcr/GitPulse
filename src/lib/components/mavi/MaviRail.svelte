<script lang="ts">
  /**
   * Threads, grouped by where they run, each with one status. The most urgent
   * status sorts first within a group ("Needs you" above "Working" above
   * finished work not yet looked at), then the most recent.
   *
   * Each thread can be renamed in place (the pencil, or a double-click on its
   * title) and removed after a confirmation that says what removal ends.
   */
  import { tick } from "svelte";
  import { Plus, Server, Laptop, Trash2, Pencil, Search, X, MessagesSquare } from "@lucide/svelte";
  import { askConfirm } from "../../stores/modalStore";
  import { maviStore } from "../../mavi/store.svelte";
  import { STATUS_LABEL, STATUS_PRIORITY, statusTone, targetLabel, type ThreadRecord } from "../../mavi/threads";
  import { formatAge } from "../../storage/format";
  import { createAdaptiveTimer } from "../../runtime/adaptiveTimer";

  let { onNew, onOpen }: { onNew: () => void; onOpen: (key: string) => void } = $props();

  // Relative ages only need re-rendering while the rail is on screen.
  let now = $state(Date.now());
  $effect(() => createAdaptiveTimer(() => (now = Date.now()), 30_000));

  let query = $state("");

  let groups = $derived.by(() => {
    const tokens = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const byTarget = new Map<string, { label: string; ssh: boolean; records: ThreadRecord[] }>();
    for (const record of maviStore.records) {
      const label = targetLabel(record.target);
      if (tokens.length > 0) {
        const statusText = STATUS_LABEL[maviStore.status(record)].toLowerCase();
        const haystack = `${record.title.toLowerCase()} ${label.toLowerCase()} ${statusText}`;
        if (!tokens.every((token) => haystack.includes(token))) continue;
      }
      const key = `${record.target.kind}:${label}`;
      const group = byTarget.get(key) ?? { label, ssh: record.target.kind === "ssh", records: [] };
      group.records.push(record);
      byTarget.set(key, group);
    }
    // Each status once, not once per comparison.
    const rank = new Map(maviStore.records.map((r) => [r.key, STATUS_PRIORITY.indexOf(maviStore.status(r))]));
    for (const group of byTarget.values()) {
      group.records.sort((a, b) => rank.get(a.key)! - rank.get(b.key)! || b.lastActivityAt - a.lastActivityAt);
    }
    return [...byTarget.values()];
  });

  let renaming = $state<string | null>(null);
  let draftTitle = $state("");
  let renameField: HTMLInputElement | undefined = $state();

  async function startRename(record: ThreadRecord) {
    renaming = record.key;
    draftTitle = record.title;
    await tick();
    renameField?.select();
  }

  function finishRename(save: boolean) {
    if (renaming && save) maviStore.rename(renaming, draftTitle);
    renaming = null;
  }

  async function remove(record: ThreadRecord) {
    const summary = maviStore.summaryOf(record);
    const live = !!summary && summary.phase !== "ended";
    const ok = await askConfirm({
      title: "Remove this thread",
      message:
        `Removes “${record.title}” from Mavi's list` +
        (live ? " and ends its agent" : "") +
        ". Claude Code keeps the conversation's transcript" +
        (record.conversationId ? `, so it can still be resumed in a terminal with its id ${record.conversationId}.` : "."),
      confirmLabel: "Remove",
    });
    if (ok) maviStore.forget(record.key);
  }

  function age(ms: number): string {
    void now;
    return ms ? formatAge(ms) : "";
  }
</script>

<nav class="mavi-rail flex flex-col min-h-0 h-full" aria-label="Mavi threads">
  <div class="mavi-rail-head flex items-center gap-2">
    <span class="text-xs font-medium text-textPrimary">Threads</span>
    {#if maviStore.records.length > 0}
      <span class="gp-chip" data-kind="count" data-tone="neutral">{maviStore.records.length}</span>
    {/if}
    <button class="gp-btn ml-auto" data-variant="primary" onclick={onNew} title="Start a new thread">
      <Plus size={12} aria-hidden="true" /> New
    </button>
  </div>
  {#if maviStore.records.length >= 6}
    <div class="mavi-rail-search">
      <div class="relative flex items-center">
        <Search size={12} class="mavi-search-icon text-faint pointer-events-none" aria-hidden="true" />
        <input
          type="text"
          bind:value={query}
          class="gp-field w-full pl-7 {query.length > 0 ? 'pr-7' : ''}"
          data-size="sm"
          placeholder="Filter threads…"
          aria-label="Filter threads"
        />
        {#if query.length > 0}
          <span class="mavi-search-clear">
            <button
              type="button"
              class="gp-icon-btn"
              data-size="sm"
              aria-label="Clear thread filter"
              title="Clear filter"
              onclick={() => (query = "")}
            >
              <X size={11} aria-hidden="true" />
            </button>
          </span>
        {/if}
      </div>
    </div>
  {/if}
  <div class="flex-1 min-h-0 overflow-y-auto py-1">
    {#if maviStore.records.length === 0}
      <div class="flex flex-col items-center text-center px-4 py-8 gap-2">
        <span class="mavi-empty-badge flex items-center justify-center rounded-xl text-textMuted">
          <MessagesSquare size={16} aria-hidden="true" />
        </span>
        <p class="text-xs font-medium text-textPrimary">No threads yet</p>
        <p class="text-meta text-textMuted">A thread is one conversation with an agent in one repository or on one host.</p>
      </div>
    {:else if groups.length === 0}
      <div class="px-3 py-4 text-meta text-textMuted">
        <p>No threads match “{query.trim()}”.</p>
        <button class="gp-btn mt-1.5" data-variant="ghost" onclick={() => (query = "")}>Clear filter</button>
      </div>
    {/if}
    {#each groups as group (group.label + group.ssh)}
      <div class="mavi-group-label flex items-center gap-1.5 gp-label">
        {#if group.ssh}<Server size={11} aria-hidden="true" />{:else}<Laptop size={11} aria-hidden="true" />{/if}
        <span class="truncate" title={group.label}>{group.label}</span>
      </div>
      <ul>
        {#each group.records as record (record.key)}
          {@const status = maviStore.status(record)}
          {@const tone = statusTone(status)}
          <li class="mavi-thread" class:mavi-thread-open={maviStore.openKey === record.key}>
            {#if renaming === record.key}
              <form
                class="mavi-thread-row rounded-lg flex items-center gap-2"
                onsubmit={(e) => {
                  e.preventDefault();
                  finishRename(true);
                }}
              >
                <input
                  bind:this={renameField}
                  bind:value={draftTitle}
                  class="gp-field w-full text-xs"
                  aria-label="Thread name"
                  maxlength="200"
                  onblur={() => finishRename(true)}
                  onkeydown={(e) => {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      finishRename(false);
                    }
                  }}
                />
              </form>
            {:else}
              <button
                class="mavi-thread-row rounded-lg flex items-start gap-2 w-full text-left"
                aria-current={maviStore.openKey === record.key ? "true" : undefined}
                onclick={() => onOpen(record.key)}
                ondblclick={() => void startRename(record)}
              >
                <span class="mavi-dot mt-1.5" class:animate-pulse={tone === "accent"} data-tone={tone} aria-hidden="true"></span>
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-xs text-textPrimary">{record.title}</span>
                  <span class="block text-micro" class:text-warn={tone === "warn"} class:text-danger={tone === "danger"} class:text-textMuted={tone !== "warn" && tone !== "danger"}>
                    {STATUS_LABEL[status]}{record.lastActivityAt ? ` · ${age(record.lastActivityAt)}` : ""}
                  </span>
                </span>
              </button>
              <span class="mavi-thread-actions rounded-full flex items-center gap-0.5">
                <button
                  class="gp-icon-btn"
                  data-size="sm"
                  aria-label="Rename {record.title}"
                  title="Rename"
                  onclick={() => void startRename(record)}
                >
                  <Pencil size={11} aria-hidden="true" />
                </button>
                <button
                  class="gp-icon-btn"
                  data-size="sm"
                  aria-label="Remove {record.title} from the list"
                  title="Remove from the list (ends its agent)"
                  onclick={() => void remove(record)}
                >
                  <Trash2 size={11} aria-hidden="true" />
                </button>
              </span>
            {/if}
          </li>
        {/each}
      </ul>
    {/each}
  </div>
</nav>

<style>
  .mavi-rail {
    border-right: 1px solid var(--gp-edge-soft);
  }
  .mavi-rail-head {
    min-height: 44px;
    padding: 6px 10px 6px 12px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-empty-badge {
    width: 32px;
    height: 32px;
    background: rgb(var(--c-surface) / 0.9);
    border: 1px solid var(--gp-edge-soft);
  }
  .mavi-rail-search {
    padding: 8px 10px 4px;
  }
  .mavi-rail-search :global(.mavi-search-icon) {
    position: absolute;
    left: 9px;
  }
  .mavi-search-clear {
    position: absolute;
    right: 2px;
  }
  .mavi-group-label {
    padding: 10px 12px 4px;
  }
  .mavi-thread {
    position: relative;
    margin: 2px 6px;
  }
  .mavi-thread-row {
    padding: 8px 10px;
    transition: background-color var(--gp-dur-instant) var(--gp-ease-effect);
  }
  .mavi-thread-row:hover,
  .mavi-thread-open .mavi-thread-row {
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-thread-open .mavi-thread-row {
    box-shadow: inset 2px 0 0 rgb(var(--c-accent));
  }
  .mavi-thread-actions {
    position: absolute;
    right: 6px;
    top: 50%;
    transform: translateY(-50%);
    padding: 1px 2px;
    background: rgb(var(--c-surface) / 0.9);
    border: 1px solid var(--gp-edge-soft);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--gp-dur-fast) var(--gp-ease-effect);
  }
  .mavi-thread:hover .mavi-thread-actions,
  .mavi-thread:focus-within .mavi-thread-actions {
    opacity: 1;
    pointer-events: auto;
  }
  .mavi-dot {
    flex: 0 0 auto;
    width: 7px;
    height: 7px;
    border-radius: var(--gp-r-full);
    background: rgb(var(--c-text-faint));
  }
  .mavi-dot[data-tone="warn"] {
    background: rgb(var(--c-warn));
    box-shadow: 0 0 0 3px rgb(var(--c-warn) / 0.2);
  }
  .mavi-dot[data-tone="accent"] {
    background: rgb(var(--c-accent));
  }
  .mavi-dot[data-tone="danger"] {
    background: rgb(var(--c-danger));
  }
  .mavi-dot[data-tone="ok"] {
    background: rgb(var(--c-ok));
  }
</style>
