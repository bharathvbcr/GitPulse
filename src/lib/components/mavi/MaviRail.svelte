<script lang="ts">
  /**
   * Threads, grouped by where they run, each with one status. The most urgent
   * status sorts first within a group ("Needs you" above "Working" above
   * finished work not yet looked at), then the most recent.
   */
  import { Plus, Server, Laptop, Trash2 } from "@lucide/svelte";
  import { maviStore } from "../../mavi/store.svelte";
  import { STATUS_LABEL, STATUS_PRIORITY, statusTone, targetLabel, type ThreadRecord } from "../../mavi/threads";
  import { formatAge } from "../../storage/format";
  import { createAdaptiveTimer } from "../../runtime/adaptiveTimer";

  let { onNew, onOpen }: { onNew: () => void; onOpen: (key: string) => void } = $props();

  // Relative ages only need re-rendering while the rail is on screen.
  let now = $state(Date.now());
  $effect(() => createAdaptiveTimer(() => (now = Date.now()), 30_000));

  let groups = $derived.by(() => {
    const byTarget = new Map<string, { label: string; ssh: boolean; records: ThreadRecord[] }>();
    for (const record of maviStore.records) {
      const label = targetLabel(record.target);
      const key = `${record.target.kind}:${label}`;
      const group = byTarget.get(key) ?? { label, ssh: record.target.kind === "ssh", records: [] };
      group.records.push(record);
      byTarget.set(key, group);
    }
    for (const group of byTarget.values()) {
      group.records.sort((a, b) => {
        const pa = STATUS_PRIORITY.indexOf(maviStore.status(a));
        const pb = STATUS_PRIORITY.indexOf(maviStore.status(b));
        return pa - pb || b.lastActivityAt - a.lastActivityAt;
      });
    }
    return [...byTarget.values()];
  });

  function age(ms: number): string {
    void now;
    return ms ? formatAge(ms) : "";
  }
</script>

<nav class="mavi-rail flex flex-col min-h-0 h-full" aria-label="Mavi threads">
  <div class="mavi-rail-head flex items-center gap-2">
    <span class="text-xs font-medium text-textPrimary">Threads</span>
    <button class="gp-btn ml-auto" data-variant="primary" onclick={onNew} title="Start a new thread">
      <Plus size={12} aria-hidden="true" /> New
    </button>
  </div>
  <div class="flex-1 min-h-0 overflow-y-auto">
    {#if groups.length === 0}
      <p class="text-meta text-textMuted px-3 py-4">No threads yet. A thread is one conversation with an agent in one repository or on one host.</p>
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
            <button class="mavi-thread-row flex items-start gap-2 w-full text-left" onclick={() => onOpen(record.key)}>
              <span class="mavi-dot mt-1.5" class:animate-pulse={tone === "accent"} data-tone={tone} aria-hidden="true"></span>
              <span class="min-w-0 flex-1">
                <span class="block truncate text-xs text-textPrimary">{record.title}</span>
                <span class="block text-micro" class:text-warn={tone === "warn"} class:text-danger={tone === "danger"} class:text-textMuted={tone !== "warn" && tone !== "danger"}>
                  {STATUS_LABEL[status]}{record.lastActivityAt ? ` · ${age(record.lastActivityAt)}` : ""}
                </span>
              </span>
            </button>
            <button
              class="mavi-thread-remove gp-icon-btn"
              data-size="sm"
              aria-label="Remove {record.title} from the list"
              title="Remove from the list (stops its agent)"
              onclick={() => maviStore.forget(record.key)}
            >
              <Trash2 size={11} aria-hidden="true" />
            </button>
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
    padding: 8px 10px 8px 12px;
    border-bottom: 1px solid var(--gp-edge-soft);
  }
  .mavi-group-label {
    padding: 10px 12px 4px;
  }
  .mavi-thread {
    position: relative;
  }
  .mavi-thread-row {
    padding: 6px 30px 6px 12px;
  }
  .mavi-thread-row:hover,
  .mavi-thread-open .mavi-thread-row {
    background: rgb(var(--c-surface-hover) / 0.65);
  }
  .mavi-thread-open .mavi-thread-row {
    box-shadow: inset 2px 0 0 rgb(var(--c-accent));
  }
  .mavi-thread-remove {
    position: absolute;
    right: 6px;
    top: 6px;
    opacity: 0;
  }
  .mavi-thread:hover .mavi-thread-remove,
  .mavi-thread-remove:focus-visible {
    opacity: 1;
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
