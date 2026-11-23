<script module lang="ts">
  import type { TabKind, SessionKind } from "../terminal/tabs";

  export interface DockAgentRow {
    /** Tab id for `here` rows; session registry key for `elsewhere` rows. */
    id: string;
    launcher: TabKind;
    title: string;
    /** Secondary line: checkout/branch for here, "repo/checkout" for elsewhere. */
    detail?: string | null;
    /** OSC status badge, e.g. { label: "Working", cls: "text-accent" }. */
    status?: { label: string; cls: string } | null;
    /** Needs the reader: e.g. "Waiting", "Failed". Drawn as a warn/danger chip. */
    attention?: string | null;
    active?: boolean;
    unread?: boolean;
    /** Elsewhere rows that cannot be revealed. */
    disabled?: boolean;
  }
</script>

<script lang="ts">
  import { ArrowRight, Eye } from "@lucide/svelte";
  import LauncherIcon from "./LauncherIcon.svelte";

  interface Props {
    here: DockAgentRow[];
    elsewhere: DockAgentRow[];
    launchers: { kind: SessionKind; label: string; disabled: boolean; title: string }[];
    followAgent: boolean;
    canFollow: boolean;
    onSelect: (id: string) => void;
    onGoTo: (id: string) => void;
    onLaunch: (kind: SessionKind) => void;
    onToggleFollow: () => void;
  }

  let { here, elsewhere, launchers, followAgent, canFollow, onSelect, onGoTo, onLaunch, onToggleFollow }: Props = $props();

  const empty = $derived(here.length === 0 && elsewhere.length === 0);

  function rowTitle(row: DockAgentRow, here: boolean): string {
    if (here) return `Show in Shell \u2014 ${row.title}`;
    return row.detail ? `Go to ${row.title} in ${row.detail}` : `Go to ${row.title}`;
  }

  function attentionTone(label: string): "danger" | "warn" {
    return /fail|error/i.test(label) ? "danger" : "warn";
  }

  /** ArrowUp/ArrowDown roving between the row buttons of the whole section. */
  function onListKeydown(event: KeyboardEvent) {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const root = (event.currentTarget as HTMLElement).closest<HTMLElement>('[data-testid="dock-agents"]');
    if (!root) return;
    const rows = Array.from(root.querySelectorAll<HTMLButtonElement>('[data-testid="dock-agent-row"]:not(:disabled)'));
    const at = rows.indexOf(document.activeElement as HTMLButtonElement);
    if (at < 0) return;
    const next = rows[at + (event.key === "ArrowDown" ? 1 : -1)];
    if (!next) return;
    event.preventDefault();
    next.focus();
  }
</script>

{#snippet rowList(rows: DockAgentRow[], onPick: (id: string) => void, isHere: boolean)}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -- arrow roving for the native row buttons inside -->
  <ul role="list" class="flex flex-col gap-0.5" onkeydown={onListKeydown}>
    {#each rows as row (row.id)}
      <li>
        <button
          type="button"
          class="gp-menu-item group min-w-0 focus-visible:outline-2 focus-visible:outline-accent {row.active ? 'bg-accent/10' : ''} {row.disabled ? 'opacity-50' : ''}"
          data-testid="dock-agent-row"
          data-agent-id={row.id}
          aria-current={row.active ? "true" : undefined}
          disabled={row.disabled}
          title={rowTitle(row, isHere)}
          onclick={() => onPick(row.id)}
        >
          <LauncherIcon kind={row.launcher} size={14} class="shrink-0" />
          <span class="flex min-w-0 flex-1 flex-col">
            <span class="flex min-w-0 items-center gap-2">
              <span class="truncate font-medium text-textPrimary">{row.title}</span>
              {#if row.unread}
                <span class="h-1.5 w-1.5 shrink-0 rounded-full bg-accent" aria-hidden="true"></span>
                <span class="sr-only">Unread output</span>
              {/if}
              {#if row.attention}
                <span class="gp-chip shrink-0" data-tone={attentionTone(row.attention)} data-kind="label">{row.attention}</span>
              {/if}
            </span>
            {#if row.detail}
              <span class="truncate font-mono text-micro text-textMuted">{row.detail}</span>
            {/if}
          </span>
          <span class="ml-auto shrink-0 text-micro {row.status ? row.status.cls : 'text-textMuted'}" data-testid="dock-agent-status">{row.status ? row.status.label : "Idle"}</span>
          <ArrowRight size={12} class="shrink-0 text-textMuted opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100" aria-hidden="true" />
        </button>
      </li>
    {/each}
  </ul>
{/snippet}

<section aria-label="Agents" data-testid="dock-agents" class="h-full min-h-0 overflow-y-auto px-3 py-3">
  <div class="flex flex-wrap items-center gap-1.5 pb-3">
    <span class="text-micro text-textMuted">Start an agent</span>
    {#each launchers as launcher (launcher.kind)}
      <button
        type="button"
        class="gp-btn"
        data-size="sm"
        data-testid="dock-agents-launch-{launcher.kind}"
        disabled={launcher.disabled}
        title={launcher.title}
        onclick={() => onLaunch(launcher.kind)}
      >
        <LauncherIcon kind={launcher.kind} size={12} class="shrink-0" />
        {launcher.label}
      </button>
    {/each}
    <button
      type="button"
      class="gp-btn ml-auto {followAgent ? 'text-accent bg-accent/15' : ''}"
      data-size="sm"
      data-testid="dock-agents-follow"
      aria-pressed={followAgent}
      disabled={!canFollow}
      title={canFollow ? "Show the files agents modify in one split pane" : "Open a repository to follow agent edits"}
      onclick={onToggleFollow}
    >
      <Eye size={12} aria-hidden="true" />
      Follow edits
    </button>
  </div>
  {#if launchers.length > 0 && launchers.every((launcher) => launcher.disabled)}
    <!-- A disabled button cannot take focus, so its tooltip is hover-only:
         say in view why nothing can start. -->
    <p class="mt-1.5 text-micro text-warn" role="status" data-testid="dock-agents-blocked">{launchers[0].title}</p>
  {/if}

  {#if empty}
    <div class="flex flex-col items-center gap-1 py-10 text-center" data-testid="dock-agents-empty">
      <p class="text-meta font-medium text-textPrimary">No agents running</p>
      <p class="text-micro text-textMuted">Start one above — it opens as a tab in Shell, beside your terminals.</p>
    </div>
  {:else}
    <div class="mt-5">
      <h3 class="gp-label mb-1.5">In this repository <span class="gp-chip" data-kind="count">{here.length}</span></h3>
      {#if here.length > 0}
        {@render rowList(here, onSelect, true)}
      {:else}
        <p class="px-2.5 text-micro text-textMuted">None in this repository</p>
      {/if}
    </div>
    {#if elsewhere.length > 0}
      <div class="mt-5">
        <h3 class="gp-label mb-1.5">Other repositories <span class="gp-chip" data-kind="count">{elsewhere.length}</span></h3>
        {@render rowList(elsewhere, onGoTo, false)}
      </div>
    {/if}
  {/if}
</section>
