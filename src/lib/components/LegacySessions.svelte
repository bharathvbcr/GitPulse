<script lang="ts">
  /**
   * Sessions left running in the previous GitPulse version's terminal host,
   * inside the Sessions list. This version cannot attach to them or type into
   * them (that is a later stage); it can stop them and read what they printed.
   * The decisions are in `terminal/legacySessions.ts`; this only draws them.
   */
  import { Archive } from "@lucide/svelte";
  import { describeError } from "../ui/copy/describeFailure";
  import { askConfirm } from "../stores/modalStore";
  import {
    legacySessionLabel,
    legacyView,
    readLegacyHosts,
    readLegacyTranscript,
    stopLegacySession,
    type LegacyHostsRead,
    type LegacySession,
  } from "../terminal/legacySessions";
  import { transcriptNotices, transcriptPlainText } from "../terminal/savedTranscript";

  let { open }: { open: boolean } = $props();

  let read = $state<LegacyHostsRead | null>(null);
  let loading = $state(false);
  let actionError = $state<string | null>(null);
  let stopping = $state<string | null>(null);
  /** The output being shown, by row key. */
  let shown = $state<{ key: string; label: string; body: string; notices: string[] } | null>(null);
  let showing = $state<string | null>(null);

  const view = $derived(legacyView(read));

  async function refresh() {
    loading = true;
    try {
      read = await readLegacyHosts();
    } finally {
      loading = false;
    }
  }

  // Asked when the list opens, not at start-up: most pages never have a
  // previous host, and a command no fixture answers must not run on every mount.
  $effect(() => {
    if (open) void refresh();
    else shown = null;
  });

  async function stop(protocol: number, session: LegacySession, key: string) {
    actionError = null;
    stopping = key;
    try {
      if (await stopLegacySession(protocol, session, { confirm: askConfirm })) {
        if (shown?.key === key) shown = null;
        // The host's own answer, not our memory of it: a stopped session
        // leaves the list only once the host no longer lists it.
        await refresh();
      }
    } catch (error: unknown) {
      actionError = describeError(error, "terminal");
    } finally {
      stopping = null;
    }
  }

  async function showOutput(protocol: number, session: LegacySession, key: string) {
    actionError = null;
    if (shown?.key === key) {
      shown = null;
      return;
    }
    showing = key;
    try {
      const transcript = await readLegacyTranscript(protocol, session.id);
      const label = legacySessionLabel(session);
      if (transcript.kind === "found") {
        shown = { key, label, body: transcriptPlainText(transcript.text), notices: transcriptNotices(transcript) };
      } else if (transcript.kind === "missing") {
        shown = { key, label, body: "", notices: ["The host has no saved output for this session."] };
      } else {
        shown = { key, label, body: "", notices: [transcript.kind === "unavailable" ? transcript.reason : `The output could not be read: ${transcript.reason}`] };
      }
    } finally {
      showing = null;
    }
  }
</script>

{#if view.rows.length > 0 || view.problems.length > 0}
  <div class="legacy" data-testid="legacy-sessions" role="group" aria-label="Sessions in the previous GitPulse version's terminal host">
    <div class="legacy-head" data-testid="legacy-sessions-head">
      <Archive size={14} aria-hidden="true" />
      <span class="font-medium">Running in the previous GitPulse version's terminal host</span>
      {#if loading}<span class="text-textMuted">Updating…</span>{/if}
    </div>
    {#each view.problems as problem (problem)}
      <p class="text-textMuted legacy-problem" data-testid="legacy-sessions-problem">{problem}</p>
    {/each}
    {#each view.rows as row (row.key)}
      <div class="legacy-row" data-testid="legacy-session-row">
        <span class="min-w-0 truncate">
          <span class="font-medium text-textPrimary">{legacySessionLabel(row.session)}</span>
          <span class="text-textMuted"> · {row.session.ending ? "Ending — its process has not exited" : "Still running — cannot be shown here"}</span>
        </span>
        <span class="legacy-path truncate font-mono text-textMuted" title={row.session.repo}>{row.session.repo}</span>
        <span class="flex items-center gap-1 shrink-0">
          <button
            type="button"
            class="gp-btn"
            data-size="sm"
            data-testid="legacy-session-output"
            aria-expanded={shown?.key === row.key}
            disabled={showing === row.key}
            onclick={() => void showOutput(row.protocol, row.session, row.key)}
          >{shown?.key === row.key ? "Hide output" : "Show output"}</button>
          <button
            type="button"
            class="gp-btn"
            data-variant="ghost"
            data-size="sm"
            data-testid="legacy-session-stop"
            disabled={stopping === row.key}
            onclick={() => void stop(row.protocol, row.session, row.key)}
          >Stop</button>
        </span>
        {#if shown?.key === row.key}
          <div class="legacy-output" data-testid="legacy-session-output-view">
            {#each shown.notices as notice (notice)}<p class="text-textMuted">{notice}</p>{/each}
            {#if shown.body}
              <!-- Justified: a scrollable region must be focusable so the keyboard can scroll it. -->
              <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
              <pre class="legacy-pre" role="region" tabindex="0" aria-label={`Saved output of ${shown.label}`}>{shown.body}</pre>
            {/if}
          </div>
        {/if}
      </div>
    {/each}
    {#if actionError}
      <p class="text-danger" role="alert" data-testid="legacy-sessions-error">{actionError}</p>
    {/if}
  </div>
{/if}

<style>
  .legacy { margin-top: 0.5rem; border-top: 1px solid var(--color-border, currentColor); padding-top: 0.375rem; }
  .legacy-head { display: flex; align-items: center; gap: 0.5rem; padding-bottom: 0.125rem; }
  .legacy-problem { margin: 0.125rem 0; }
  .legacy-row {
    display: grid;
    grid-template-columns: minmax(8rem, 16rem) minmax(6rem, 1fr) auto;
    column-gap: 0.75rem;
    align-items: center;
    padding: 0.125rem 0;
  }
  .legacy-path { min-width: 0; }
  .legacy-output { grid-column: 1 / -1; padding: 0.25rem 0; }
  .legacy-pre {
    margin: 0.25rem 0 0;
    max-height: 10rem;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-family: var(--font-mono, monospace);
    font-size: var(--text-xs);
  }
</style>
