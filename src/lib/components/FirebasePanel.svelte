<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  import type { FirebaseRolloutsReport, FirebaseStatus } from "../firebase/types";

  // Survive the per-tab remount so revisiting Remote renders the last-known
  // status and rollout listing instantly; the status fetch then refreshes it
  // in place. The rollout listing is NOT refetched on mount — see below.
  const statusCache = createRepoPanelCache<FirebaseStatus>();
  const rolloutsCache = createRepoPanelCache<FirebaseRolloutsReport>();
</script>

<script lang="ts">
  import { Flame, LoaderCircle, RefreshCw } from "@lucide/svelte";
  import { keyedList } from "../ui/eachKeys";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { harnessStore, verdictLabel } from "../stores/harnessStore";
  import { getFirebaseStatus, listFirebaseRollouts } from "../firebase/client";
  import {
    currentRollout,
    rolloutStateClass,
    rolloutStateLabel,
    shortSha,
  } from "../firebase/rolloutState";
  import { reportPanelError } from "../diagnostics/report";
  import { formatError } from "../ui/formatError";
  import EmptyState from "./EmptyState.svelte";

  /** Null while no repository is open; the panel renders nothing for it. */
  let { repoPath }: { repoPath: string | null } = $props();

  let status = $state<FirebaseStatus | null>(null);
  let statusError = $state<string | null>(null);
  let statusLoading = $state(false);

  let rollouts = $state<FirebaseRolloutsReport | null>(null);
  let rolloutsError = $state<string | null>(null);
  let rolloutsLoading = $state(false);
  let fetchedAt = $state<number | null>(null);

  /**
   * The alias the user picked.
   *
   * Never pre-filled from `default_alias`. `.firebaserc`'s `default` is very
   * often production, and a listing — let alone a rollout — aimed there by a
   * default the user never saw is the failure this selector exists to prevent.
   */
  let selectedAlias = $state<string>("");
  let selectedBackend = $state<string>("");

  let statusGuard: AsyncGuard | null = null;
  let rolloutsGuard: AsyncGuard | null = null;

  const selectedProjectId = $derived(
    status?.projects.find((p) => p.alias === selectedAlias)?.project_id ?? "",
  );
  const live = $derived(rollouts ? currentRollout(rollouts.rollouts) : null);

  /**
   * True when the listing on screen cannot be read as the whole picture.
   *
   * Either our display cap bit, or the producer telling us it stopped early.
   * They are kept apart in the payload and are rendered as separate sentences,
   * but "is this complete?" is one question.
   */
  const bounded = $derived(
    Boolean(rollouts && (rollouts.truncated || rollouts.walk_incomplete)),
  );

  async function loadStatus(repo: string): Promise<void> {
    statusGuard?.cancel();
    const guard = createAsyncGuard();
    statusGuard = guard;
    statusLoading = true;
    try {
      const next = await getFirebaseStatus(repo);
      if (!guard.isLive()) return;
      status = next;
      statusError = null;
      statusCache.set(repo, next);
      if (next.projects.length === 1) {
        // One alias is not a choice, so pre-selecting it hides nothing. Two or
        // more stays empty until the user names the target.
        selectedAlias = next.projects[0].alias;
      }
    } catch (err) {
      if (!guard.isLive()) return;
      status = null;
      statusError = formatError(err);
      reportPanelError("firebase", err);
    } finally {
      if (guard.isLive()) statusLoading = false;
    }
  }

  /**
   * Lists rollouts. Only ever called from the button.
   *
   * This is deliberately not wired to a mount effect: the Firebase CLI enables
   * the App Hosting API on the project when it is off, so the first listing for
   * a project can change the user's Google Cloud setup. That belongs to a click,
   * not to opening a tab.
   */
  async function loadRollouts(): Promise<void> {
    if (!repoPath || !selectedProjectId || !selectedBackend) return;
    rolloutsGuard?.cancel();
    const guard = createAsyncGuard();
    rolloutsGuard = guard;
    rolloutsLoading = true;
    try {
      const result = await listFirebaseRollouts(
        repoPath,
        selectedProjectId,
        selectedBackend,
        null,
      );
      if (!guard.isLive()) return;
      harnessStore.recordVerdict(result?.policy ?? null, repoPath);
      rollouts = result.output;
      rolloutsError = null;
      fetchedAt = Date.now();
      rolloutsCache.set(repoPath, result.output);
    } catch (err) {
      if (!guard.isLive()) return;
      rollouts = null;
      rolloutsError = formatError(err);
      reportPanelError("firebase", err);
    } finally {
      if (guard.isLive()) rolloutsLoading = false;
    }
  }

  $effect(() => {
    const repo = repoPath;
    if (!repo) {
      status = null;
      rollouts = null;
      return;
    }
    // Hydrate synchronously so a revisit renders instantly, then refresh the
    // free half. The cached rollout listing keeps no `fetchedAt`: a listing
    // carrying no timestamp reads as current however long it has been sitting
    // there, and this one was not fetched now.
    status = statusCache.get(repo) ?? null;
    rollouts = rolloutsCache.get(repo) ?? null;
    fetchedAt = null;
    void loadStatus(repo);
  });

  $effect(() => () => {
    statusGuard?.cancel();
    rolloutsGuard?.cancel();
  });
</script>

<section data-panel="firebase">
  <div class="flex items-center justify-between gap-3 mb-2">
    <h3 class="text-[11px] uppercase tracking-wider text-textMuted flex items-center gap-1.5">
      <Flame size={12} />
      Firebase App Hosting
    </h3>
    {#if statusLoading || rolloutsLoading}
      <LoaderCircle size={12} class="animate-spin text-textMuted" />
    {/if}
  </div>

  {#if statusError}
    <!-- Rose: the status read is local and free, so a failure here is a fault,
         not a configuration gap. -->
    <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300 text-xs">
      Firebase status could not be read: {statusError}
    </div>
  {:else if status && !status.configured}
    <EmptyState
      icon={Flame}
      title="No Firebase config in this repository"
      hint="A .firebaserc or firebase.json at the repository root enables this panel."
      compact
    />
  {:else if status}
    {#if status.firebaserc_error}
      <div class="mb-2 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
        .firebaserc could not be read: {status.firebaserc_error}
      </div>
    {/if}
    {#if status.firebasejson_error}
      <div class="mb-2 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs">
        firebase.json could not be read: {status.firebasejson_error}
      </div>
    {/if}

    {#if !status.cli.present}
      <!-- Amber: a missing CLI is a configuration gap, not a fault. The reason
           distinguishes "not installed" from "installed where a windowed
           launch cannot see it", because those have different remedies. -->
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
        {status.cli.reason ?? "The Firebase CLI is not available."}
      </div>
    {:else if status.projects.length === 0}
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
        No project alias is configured in .firebaserc, so there is nothing to query.
      </div>
    {:else}
      <div class="flex flex-wrap items-end gap-2 mb-3">
        <label class="flex flex-col gap-1 text-[11px] text-textMuted">
          Project
          <select class="gp-select text-xs" bind:value={selectedAlias}>
            <option value="">Select a project…</option>
            {#each keyedList(status.projects, (p) => p.alias) as { key, item } (key)}
              <option value={item.alias}>
                {item.alias} — {item.project_id}{item.alias === status.default_alias ? " (default)" : ""}
              </option>
            {/each}
          </select>
        </label>
        <label class="flex flex-col gap-1 text-[11px] text-textMuted">
          Backend
          <input
            class="gp-field text-xs font-mono"
            placeholder="backend id"
            bind:value={selectedBackend}
          />
        </label>
        <button
          class="px-2.5 py-1.5 rounded-lg border border-border/70 bg-surface hover:bg-surfaceHover text-xs inline-flex items-center gap-1.5 disabled:opacity-50"
          disabled={!selectedProjectId || !selectedBackend || rolloutsLoading}
          onclick={() => void loadRollouts()}
        >
          <RefreshCw size={12} />
          Check rollouts
        </button>
      </div>

      <!-- Said before the button is pressed, not after. The CLI enables the
           App Hosting API when it is off, and that is a change to the user's
           Google Cloud project, not a read. -->
      <p class="mb-3 text-[11px] text-textMuted max-w-xl">
        Checking rollouts runs the Firebase CLI as your signed-in Google account. If the App
        Hosting API is not enabled on <span class="font-mono">{selectedProjectId || "the project"}</span>,
        the CLI enables it — a change to your Google Cloud project.
      </p>

      {#if rolloutsError}
        <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
          Rollout listing unavailable: {rolloutsError}
        </div>
      {:else if rollouts && !rollouts.checked}
        <!-- `checked` is what separates "we could not ask" from "we asked and
             this backend has never deployed". Only the second may render as an
             empty listing. -->
        <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
          Rollout listing did not run: {rollouts.error ?? "no reason was reported"}
        </div>
      {:else if rollouts}
        {#if rollouts.walk_incomplete}
          <!-- Above the rows, not in a tooltip: a truncated answer wearing a
               complete answer's clothes is the thing being prevented. -->
          <div class="mb-2 p-2.5 rounded-lg border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400 text-[11px] max-w-xl">
            {rollouts.walk_incomplete}
          </div>
        {/if}

        {#if live}
          <div class="mb-3 text-xs">
            <span class="text-textMuted">Live:</span>
            {#if live.commit}
              <span class="font-mono">{shortSha(live.commit.hash)}</span>
              {#if !live.commit.present_locally}
                <span class="text-amber-700 dark:text-amber-400 text-[11px]">
                  (not in this checkout)
                </span>
              {/if}
              {#if live.commit.message}
                <span class="text-textMuted"> · {live.commit.message}</span>
              {/if}
            {:else}
              <span class="text-textMuted">rollout {live.id}, no source commit reported</span>
            {/if}
            {#if bounded}
              <span class="text-textMuted text-[11px]"> · within the rollouts shown</span>
            {/if}
          </div>
        {/if}

        {#if rollouts.rollouts.length === 0 && bounded}
          <EmptyState icon={Flame} title="No rollouts in the listing shown" compact />
        {:else if rollouts.rollouts.length === 0}
          <EmptyState icon={Flame} title="This backend has never deployed" compact />
        {:else}
          <div class="space-y-1">
            {#each keyedList(rollouts.rollouts, (r) => r.id) as { key, item } (key)}
              <div class="flex items-baseline gap-2 text-xs py-1 border-b border-border/40 last:border-0">
                <span class={`font-medium ${rolloutStateClass(item.state)}`}>
                  {rolloutStateLabel(item.state)}
                </span>
                {#if item.commit}
                  <span class="font-mono text-[11px]">{shortSha(item.commit.hash)}</span>
                  {#if !item.commit.present_locally}
                    <span
                      class="text-[10px] text-amber-700 dark:text-amber-400"
                      title="This commit is not in the opened checkout — a force-push, a fork, or a shallow clone."
                    >
                      not local
                    </span>
                  {/if}
                {/if}
                <span class="text-textMuted truncate min-w-0">
                  {item.commit?.message ?? item.id}
                </span>
                {#if item.error}
                  <span class="text-red-700 dark:text-red-400 text-[10px] truncate">{item.error}</span>
                {/if}
              </div>
            {/each}
          </div>
          {#if rollouts.truncated}
            <div class="mt-2 text-amber-600 dark:text-amber-400 text-[11px]">
              Showing {rollouts.rollouts.length} rollouts; more exist. This is not complete coverage.
            </div>
          {/if}
        {/if}

        {#if fetchedAt}
          <div class="mt-2 text-[10px] text-textMuted">
            Checked {new Date(fetchedAt).toLocaleTimeString()}
          </div>
        {/if}
        {#if $harnessStore.lastVerdict}
          <div class="mt-1 text-[10px] text-textMuted">
            Policy: {verdictLabel($harnessStore.lastVerdict)}
          </div>
        {/if}
      {/if}
    {/if}
  {/if}
</section>
