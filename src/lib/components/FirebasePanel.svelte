<script module lang="ts">
  import { createRepoPanelCache } from "../panels/repoPanelCache";
  // Same treatment the diff pane's blast radius gets: the qualification stays
  // above the rows where it cannot be missed, but as one folded clause rather
  // than the kernel's full essay. The verbatim text stays one disclosure away.
  import { firstClause, summarizeWalkIncomplete } from "../codeintel/walkIncomplete";
  import type {
    FirebaseBackendsReport,
    FirebaseRolloutsReport,
    FirebaseStatus,
  } from "../firebase/types";

  /**
   * Everything a revisit needs to render the *same* picture it left.
   *
   * Cached as one record rather than one cache per report, because the reports
   * are only meaningful against the selection that produced them. Hydrating a
   * rollout listing beside an empty project selector renders rows with nothing
   * saying which backend they belong to — and a "Live: 0123abc" line naming no
   * target is a claim about production that the reader cannot check.
   *
   * The report fields are suffixed rather than named after the `$state`
   * variables they restore. `effect-loop-contract` counts reads by word
   * boundary, so `snapshot?.backends` inside the hydrating effect reads to it
   * as a read of the `backends` state that same effect writes — a
   * self-invalidating effect, which is the one class `npm test` cannot catch
   * because the node environment compiles `$effect` out. The analyser is
   * deliberately conservative; the suffix costs nothing and keeps it that way.
   */
  interface FirebasePanelSnapshot {
    alias: string;
    backend: string;
    backendsReport: FirebaseBackendsReport | null;
    rolloutsReport: FirebaseRolloutsReport | null;
  }

  // Survive the per-tab remount so revisiting Remote renders the last-known
  // state instantly; the status read then refreshes the free half in place.
  // Neither listing is refetched on mount — see `loadBackends` below.
  const statusCache = createRepoPanelCache<FirebaseStatus>();
  const snapshotCache = createRepoPanelCache<FirebasePanelSnapshot>();
</script>

<script lang="ts">
  import { Flame, LoaderCircle, RefreshCw, Rocket, Server } from "@lucide/svelte";
  import { keyedList } from "../ui/eachKeys";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { harnessStore, verdictLabel, type PolicyVerdict } from "../stores/harnessStore";
  import {
    createFirebaseRollout,
    getFirebaseStatus,
    listFirebaseBackends,
    listFirebaseRollouts,
  } from "../firebase/client";
  import type { RolloutCreateOutcome } from "../firebase/types";
  import {
    commitShaProblem,
    currentRollout,
    rolloutStateClass,
    rolloutStateLabel,
    rolloutTimelineRows,
    shortSha,
  } from "../firebase/rolloutState";
  import DeliveryTimeline from "./DeliveryTimeline.svelte";
  import { anyInFlight } from "../delivery/transitions";
  import { createLivePoll, type LiveState } from "../delivery/livePoll";
  import { createVisibleInterval } from "../dom/visibleInterval";
  import { reportPanelError } from "../diagnostics/report";
  import { formatError } from "../ui/formatError";
  import EmptyState from "./EmptyState.svelte";

  /** Null while no repository is open; the panel renders nothing for it. */
  let { repoPath }: { repoPath: string | null } = $props();

  let status = $state<FirebaseStatus | null>(null);
  let statusError = $state<string | null>(null);
  let statusLoading = $state(false);

  let backends = $state<FirebaseBackendsReport | null>(null);
  let backendsError = $state<string | null>(null);
  let backendsLoading = $state(false);

  let rollouts = $state<FirebaseRolloutsReport | null>(null);
  let rolloutsError = $state<string | null>(null);
  let rolloutsLoading = $state(false);
  let fetchedAt = $state<number | null>(null);

  /**
   * The gate's decision on *this* panel's last gated call, and which call it
   * was.
   *
   * Local rather than `$harnessStore.lastVerdict`, which is the last verdict
   * recorded anywhere in the app for this repository. Rendering that under a
   * rollout listing attributes a commit gate's or a conflict editor's decision
   * to a Firebase call that may never have been judged at all — the same
   * "unexamined reads as approved" substitution the reports themselves exist to
   * prevent. `CommitComposer` keeps its own for this reason.
   */
  let lastVerdict = $state<PolicyVerdict | null>(null);
  let lastVerdictAction = $state<string>("");

  /**
   * The alias the user picked.
   *
   * Never pre-filled from `default_alias`. `.firebaserc`'s `default` is very
   * often production, and a listing — let alone a rollout — aimed there by a
   * default the user never saw is the failure this selector exists to prevent.
   */
  let selectedAlias = $state<string>("");
  let selectedBackend = $state<string>("");

  /**
   * The deploy form. Two steps, not one.
   *
   * `armed` is what separates typing a SHA from deploying it. A rollout changes
   * what production serves, App Hosting has no rollback verb, and creating one
   * twice deploys twice — so the target is spelled out in full and confirmed
   * before the call exists, rather than sitting one mis-click away from a
   * pre-filled field.
   */
  let deploySha = $state<string>("");
  let deployArmed = $state(false);
  let deployRunning = $state(false);
  let deployError = $state<string | null>(null);
  let deployOutcome = $state<RolloutCreateOutcome | null>(null);

  let statusGuard: AsyncGuard | null = null;
  let backendsGuard: AsyncGuard | null = null;
  let rolloutsGuard: AsyncGuard | null = null;
  let deployGuard: AsyncGuard | null = null;

  const selectedProjectId = $derived(
    status?.projects.find((p) => p.alias === selectedAlias)?.project_id ?? "",
  );
  const live = $derived(rollouts ? currentRollout(rollouts.rollouts) : null);
  /** The listed rollouts as timeline rows, for the shared visualization. */
  const rolloutRows = $derived(rolloutTimelineRows(rollouts?.rollouts ?? []));

  /**
   * The project and backend a rollout listing has already SUCCEEDED for.
   *
   * This is what makes a live refresh legitimate here. Listing rollouts is
   * click-only because the Firebase CLI enables the App Hosting API on a
   * project where it is off, and that is a change to a Cloud project rather
   * than a read. But the enabling happens when the API is off — and a listing
   * that already returned is proof it is on. So the *first* call for a target
   * stays a click, exactly as documented, and only a target that has already
   * answered may be refreshed.
   *
   * Keyed on both ids together because a backend id is not unique across
   * projects: reusing one project's success to authorise another's poll would
   * defeat the whole point.
   */
  let pollableTarget = $state<string | null>(null);
  let liveState = $state<LiveState>({ kind: "idle", reason: "" });
  /** Advances in-flight bars; only while the poll is actually live. */
  let timelineNow = $state(Date.now());
  const currentTarget = $derived(
    selectedProjectId && selectedBackend ? `${selectedProjectId}/${selectedBackend}` : null,
  );

  /**
   * Backends this project is known to have, or empty when none were listed.
   *
   * Only a listing that actually ran may populate the picker. A report with
   * `checked: false` carries an empty array for the reason every report here
   * does — because nothing was asked — and turning that into "this project has
   * no backends" is the exact substitution these types exist to prevent.
   */
  const knownBackends = $derived(backends && backends.checked ? backends.backends : []);

  /** True when the listing on screen cannot be read as the whole picture. */
  const bounded = $derived(
    Boolean(rollouts && (rollouts.truncated || rollouts.walk_incomplete)),
  );
  const backendsBounded = $derived(
    Boolean(backends && (backends.truncated || backends.walk_incomplete)),
  );

  /**
   * Whether this CLI can list rollouts at all.
   *
   * Three states, not two. `checked && available` is the only one that may show
   * the action; `checked && !available` explains the experiment; and
   * `!checked` says the question could not be asked — a different sentence,
   * because it sends the reader to install the CLI rather than to enable
   * something they do not need.
   */
  const rolloutListing = $derived(status?.rollout_listing ?? null);

  /** Why the typed SHA cannot be deployed, or null when it can. */
  const deployProblem = $derived(commitShaProblem(deploySha));

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
      // A selection restored from a previous visit can name an alias
      // `.firebaserc` no longer has — it was renamed, or removed. Left in
      // place it resolves to no project id, which disables both actions with
      // nothing on screen saying why, while the previous project's listings
      // stay rendered beside it. Dropping it is the only reading that stays
      // true, and it must happen before the single-alias shortcut below or
      // that shortcut sees a stale non-empty alias and declines to fire.
      if (selectedAlias && !next.projects.some((p) => p.alias === selectedAlias)) {
        selectProject("");
      }
      if (next.projects.length === 1 && !selectedAlias) {
        // One alias is not a choice, so pre-selecting it hides nothing. Two or
        // more stays empty until the user names the target.
        selectedAlias = next.projects[0].alias;
        persist();
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
   * Drops everything that was true of the previous project.
   *
   * Backend ids are not unique across projects — `web` exists in most of them —
   * so a backend left selected across a project change silently retargets the
   * next call at a different Google Cloud project. Clearing is the only correct
   * behaviour; carrying the id over would be a convenience that occasionally
   * deploys somewhere nobody chose.
   */
  function selectProject(alias: string): void {
    if (alias === selectedAlias) return;
    backendsGuard?.cancel();
    rolloutsGuard?.cancel();
    selectedAlias = alias;
    selectedBackend = "";
    backends = null;
    backendsError = null;
    rollouts = null;
    rolloutsError = null;
    fetchedAt = null;
    lastVerdict = null;
    lastVerdictAction = "";
    disarmDeploy();
    persist();
  }

  function selectBackend(id: string): void {
    if (id === selectedBackend) return;
    rolloutsGuard?.cancel();
    selectedBackend = id;
    // The rollouts on screen belong to the previous backend. Keeping them while
    // the selector says something else is the detached-listing problem in
    // miniature.
    rollouts = null;
    rolloutsError = null;
    fetchedAt = null;
    // The previous backend's listing authorised the previous backend only.
    pollableTarget = null;
    disarmDeploy();
    persist();
  }

  /**
   * Disarms the deploy step.
   *
   * Called whenever the target changes. A confirmation that outlives the thing
   * it confirmed is worse than none: the user reads "deploy abc123 to web on
   * acme-prod", changes the backend, and the armed button now points somewhere
   * they never reviewed.
   */
  function disarmDeploy(): void {
    deployGuard?.cancel();
    deployArmed = false;
    deployError = null;
    deployOutcome = null;
  }

  function persist(): void {
    if (!repoPath) return;
    snapshotCache.set(repoPath, {
      alias: selectedAlias,
      backend: selectedBackend,
      backendsReport: backends,
      rolloutsReport: rollouts,
    });
  }

  /**
   * Lists App Hosting backends. Only ever called from the button.
   *
   * Deliberately not wired to a mount effect: the Firebase CLI enables the App
   * Hosting API on the project when it is off, so the first listing for a
   * project can change the user's Google Cloud setup. That belongs to a click,
   * not to opening a tab.
   */
  async function loadBackends(): Promise<void> {
    if (!repoPath || !selectedProjectId) return;
    backendsGuard?.cancel();
    const guard = createAsyncGuard();
    backendsGuard = guard;
    backendsLoading = true;
    try {
      const result = await listFirebaseBackends(repoPath, selectedProjectId);
      if (!guard.isLive()) return;
      harnessStore.recordVerdict(result?.policy ?? null, repoPath);
      lastVerdict = result?.policy ?? null;
      lastVerdictAction = "backends";
      backends = result.output;
      backendsError = null;
      // One backend is not a choice. Two or more stays unselected, for the
      // reason the project selector does.
      if (result.output.checked && result.output.backends.length === 1) {
        selectBackend(result.output.backends[0].id);
      }
      persist();
    } catch (err) {
      if (!guard.isLive()) return;
      backends = null;
      backendsError = formatError(err);
      reportPanelError("firebase", err);
    } finally {
      if (guard.isLive()) backendsLoading = false;
    }
  }

  /** Lists rollouts for the selected backend. Gated and click-only, as above. */
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
      );
      if (!guard.isLive()) return;
      harnessStore.recordVerdict(result?.policy ?? null, repoPath);
      lastVerdict = result?.policy ?? null;
      lastVerdictAction = "rollouts";
      rollouts = result.output;
      rolloutsError = null;
      fetchedAt = Date.now();
      // A listing that answered is proof the App Hosting API is on for this
      // target, which is what authorises refreshing it without another click.
      // Only `checked` counts: a report that could not run proves nothing.
      if (result.output.checked) {
        pollableTarget = `${selectedProjectId}/${selectedBackend}`;
        livePoll?.reset();
      }
      persist();
    } catch (err) {
      if (!guard.isLive()) return;
      rollouts = null;
      rolloutsError = formatError(err);
      // A failed listing withdraws the authorisation it never earned: the next
      // call may be the one that has to enable the API, so it is a click again.
      pollableTarget = null;
      reportPanelError("firebase", err);
    } finally {
      if (guard.isLive()) rolloutsLoading = false;
    }
  }

  /**
   * One poll of an already-listed backend.
   *
   * Refuses unless the exact target still matches the one a listing succeeded
   * for — a project or backend change between the tick and this call must not
   * inherit the previous target authorisation.
   */
  async function pollRolloutsOnce(): Promise<boolean> {
    if (!repoPath || !selectedProjectId || !selectedBackend) return false;
    const target = `${selectedProjectId}/${selectedBackend}`;
    if (pollableTarget !== target) return false;
    try {
      const result = await listFirebaseRollouts(repoPath, selectedProjectId, selectedBackend);
      // The selection can move while the CLI runs; applying the answer then
      // would show one backend's rollouts under another's name.
      if (pollableTarget !== target) return false;
      if (`${selectedProjectId}/${selectedBackend}` !== target) return false;
      harnessStore.recordVerdict(result?.policy ?? null, repoPath);
      if (!result.output.checked) return false;
      rollouts = result.output;
      rolloutsError = null;
      fetchedAt = Date.now();
      persist();
      return true;
    } catch {
      return false;
    }
  }

  /**
   * The live poll for deploys, rebuilt per target.
   *
   * Never started by mounting: `pollableTarget` is null until a listing the
   * user asked for has succeeded, and `sync` is what starts a timer.
   */
  let livePoll: ReturnType<typeof createLivePoll> | null = null;
  $effect(() => {
    const target = currentTarget;
    liveState = { kind: "idle", reason: "" };
    if (!target) return;
    const driver = createLivePoll({
      poll: pollRolloutsOnce,
      onState: (next) => (liveState = next),
    });
    livePoll = driver;
    return () => {
      driver.dispose();
      if (livePoll === driver) livePoll = null;
    };
  });

  $effect(() => {
    // Two conditions, both required: a rollout is still moving, AND this
    // target has already answered a listing at least once.
    const authorised = pollableTarget !== null && pollableTarget === currentTarget;
    livePoll?.sync(authorised && anyInFlight(rolloutRows));
  });

  $effect(() => {
    if (liveState.kind !== "live") return;
    return createVisibleInterval(() => (timelineNow = Date.now()), 1_000);
  });

  /**
   * Creates a rollout. Reachable only from the armed confirm button.
   *
   * The target is re-read from state at call time rather than captured when the
   * confirmation was shown, and `disarmDeploy` runs on every change to either
   * half of that target — so the values sent are the values confirmed, or the
   * button is not armed at all.
   */
  async function deploy(): Promise<void> {
    if (!repoPath || !selectedProjectId || !selectedBackend) return;
    if (deployProblem || !deployArmed) return;
    deployGuard?.cancel();
    const guard = createAsyncGuard();
    deployGuard = guard;
    deployRunning = true;
    try {
      const result = await createFirebaseRollout(
        repoPath,
        selectedProjectId,
        selectedBackend,
        deploySha.trim().toLowerCase(),
      );
      if (!guard.isLive()) return;
      harnessStore.recordVerdict(result?.policy ?? null, repoPath);
      lastVerdict = result?.policy ?? null;
      lastVerdictAction = "deploy";
      deployOutcome = result.output;
      deployError = null;
      // Disarmed on success so a second click cannot deploy again. Upstream
      // allocates a new rollout id per call, so a repeat is a second
      // deployment rather than a no-op.
      deployArmed = false;
    } catch (err) {
      if (!guard.isLive()) return;
      deployOutcome = null;
      deployError = formatError(err);
      deployArmed = false;
      reportPanelError("firebase", err, { severity: "error" });
    } finally {
      if (guard.isLive()) deployRunning = false;
    }
  }

  $effect(() => {
    const repo = repoPath;
    if (!repo) {
      status = null;
      backends = null;
      rollouts = null;
      return;
    }
    // Hydrate synchronously so a revisit renders instantly, then refresh the
    // free half. The cached listings keep no `fetchedAt`: a listing carrying no
    // timestamp reads as current however long it has been sitting there, and
    // this one was not fetched now. The deploy form is deliberately *not*
    // restored — a confirmation is about this moment, and one that survives a
    // tab switch is a button whose reasoning the user no longer has in view.
    deploySha = "";
    deployArmed = false;
    deployError = null;
    deployOutcome = null;
    status = statusCache.get(repo) ?? null;
    const snapshot = snapshotCache.get(repo);
    selectedAlias = snapshot?.alias ?? "";
    selectedBackend = snapshot?.backend ?? "";
    backends = snapshot?.backendsReport ?? null;
    rollouts = snapshot?.rolloutsReport ?? null;
    fetchedAt = null;
    lastVerdict = null;
    lastVerdictAction = "";
    void loadStatus(repo);
  });

  $effect(() => () => {
    statusGuard?.cancel();
    backendsGuard?.cancel();
    rolloutsGuard?.cancel();
    deployGuard?.cancel();
  });
</script>

<!--
  A listing that did not cover everything, said once and said short.

  Kept above the rows rather than in a tooltip — a truncated answer wearing a
  complete answer's clothes is the thing being prevented, and that was never
  the problem here. The problem was the *form*: the kernel's full essay is
  ~60 words of corpus statistics, so the warning that mattered read as a wall
  of amber and got skipped. The first clause names the cause, and the verbatim
  text is one disclosure away for whoever needs it.
-->
{#snippet qualification(walkIncomplete: string, what: string)}
  {@const folded = summarizeWalkIncomplete([walkIncomplete]) ?? walkIncomplete}
  {@const lead = firstClause(walkIncomplete)}
  <div
    class="mb-2 max-w-xl rounded-lg border border-amber-500/30 bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-700 dark:text-amber-400"
  >
    <span class="font-medium">{what} is incomplete</span>
    {#if lead}<span class="opacity-90"> — {lead}</span>{/if}
    <details class="mt-0.5">
      <summary class="cursor-pointer text-[10px] opacity-70 hover:opacity-100">
        What the engine reported
      </summary>
      <p class="mt-1 font-mono text-[10px] leading-relaxed opacity-90">{folded}</p>
    </details>
  </div>
{/snippet}

<section data-panel="firebase">
  <div class="flex items-center justify-between gap-3 mb-2">
    <h3 class="text-[11px] uppercase tracking-wider text-textMuted flex items-center gap-1.5">
      <Flame size={12} />
      Firebase App Hosting
    </h3>
    {#if statusLoading || backendsLoading || rolloutsLoading}
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
           distinguishes "not installed" from "installed where a windowed launch
           cannot see it", because those have different remedies. -->
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
        {status.cli.reason ?? "The Firebase CLI is not available."}
      </div>
    {:else if status.projects.length === 0}
      <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
        No project alias is configured in .firebaserc, so there is nothing to query.
      </div>
    {:else}
      {#if status.projects_truncated}
        <!-- `.firebaserc` is repository content, so its alias count is not
             ours to assume. A capped picker that looks complete would hide the
             very project the reader came to select. -->
        <div class="mb-2 p-2.5 rounded-lg border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400 text-[11px] max-w-xl">
          .firebaserc names more project aliases than are listed here. This is not complete
          coverage of the file.
        </div>
      {/if}

      {#if !status.has_apphosting_config && !status.firebasejson_error}
        <!-- Not an error: a project can hold App Hosting backends whether or not
             this checkout is wired to deploy to them, and the backends listing
             below still answers. Said plainly so an empty listing is not read as
             a broken panel. -->
        <div class="mb-2 text-[11px] text-textMuted max-w-xl">
          firebase.json carries no <span class="font-mono">apphosting</span> key, so this
          checkout is not configured to deploy to App Hosting. Backends that already exist
          on the project are still listed below.
        </div>
      {/if}

      <div class="flex flex-wrap items-end gap-2 mb-2">
        <label class="flex flex-col gap-1 text-[11px] text-textMuted">
          Project
          <select
            class="gp-select text-xs"
            value={selectedAlias}
            onchange={(event) => selectProject(event.currentTarget.value)}
          >
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
          {#if knownBackends.length > 0}
            <select
              class="gp-select text-xs font-mono"
              value={selectedBackend}
              onchange={(event) => selectBackend(event.currentTarget.value)}
            >
              <option value="">Select a backend…</option>
              {#each keyedList(knownBackends, (b) => b.id) as { key, item } (key)}
                <option value={item.id}>
                  {item.id}{item.location ? ` — ${item.location}` : ""}
                </option>
              {/each}
            </select>
          {:else}
            <!-- Before a listing has run there is nothing to pick from, and a
                 user who knows the id should not have to make a gated call to
                 type it. -->
            <input
              class="gp-field text-xs font-mono"
              placeholder="backend id"
              value={selectedBackend}
              oninput={(event) => selectBackend(event.currentTarget.value.trim())}
            />
          {/if}
        </label>

        <button
          class="px-2.5 py-1.5 rounded-lg border border-border/70 bg-surface hover:bg-surfaceHover text-xs inline-flex items-center gap-1.5 disabled:opacity-50"
          disabled={!selectedProjectId || backendsLoading}
          onclick={() => void loadBackends()}
        >
          <Server size={12} />
          List backends
        </button>

        {#if rolloutListing?.available}
          <button
            class="px-2.5 py-1.5 rounded-lg border border-border/70 bg-surface hover:bg-surfaceHover text-xs inline-flex items-center gap-1.5 disabled:opacity-50"
            disabled={!selectedProjectId || !selectedBackend || rolloutsLoading}
            onclick={() => void loadRollouts()}
          >
            <RefreshCw size={12} />
            Check rollouts
          </button>
        {/if}
      </div>

      <!-- Said before either button is pressed, not after. The CLI enables the
           App Hosting API when it is off, and that is a change to the user's
           Google Cloud project, not a read. -->
      <p class="mb-3 text-[11px] text-textMuted max-w-xl">
        Listing backends or rollouts runs the Firebase CLI as your signed-in Google account.
        If the App Hosting API is not enabled on
        <span class="font-mono">{selectedProjectId || "the project"}</span>,
        the CLI enables it — a change to your Google Cloud project.
      </p>

      {#if rolloutListing && !rolloutListing.available}
        <!-- Amber, and never silence. Without this the panel would simply have
             no rollout button and no reason, which reads as a feature nobody
             built rather than one this CLI does not expose. -->
        <div class="mb-2 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
          {rolloutListing.reason ?? "This Firebase CLI does not expose rollout listing."}
        </div>
      {/if}

      {#if backendsError}
        <div class="mb-2 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
          Backend listing unavailable: {backendsError}
        </div>
      {:else if backends && !backends.checked}
        <div class="mb-2 p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-300 text-xs max-w-xl">
          Backend listing did not run: {backends.error ?? "no reason was reported"}
        </div>
      {:else if backends}
        {#if backends.walk_incomplete}
          {@render qualification(backends.walk_incomplete, "Backend listing")}
        {/if}
        {#if backends.backends.length === 0}
          <div class="mb-2 text-[11px] text-textMuted">
            {backendsBounded
              ? "No backends in the listing shown."
              : `No App Hosting backends exist on ${backends.project_id}.`}
          </div>
        {:else}
          <div class="mb-2 text-[11px] text-textMuted">
            {backends.backends.length}
            {backends.backends.length === 1 ? "backend" : "backends"} on
            <span class="font-mono">{backends.project_id}</span>{backends.truncated
              ? "; more exist. This is not complete coverage."
              : "."}
          </div>
        {/if}
      {/if}

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
        <!-- The listing names its own target rather than relying on the
             selectors above. Both are restored from one snapshot, but a
             rendering whose correctness depends on two things agreeing is one
             that eventually shows rows from one backend under another's name. -->
        <div class="mb-1 text-[11px] text-textMuted">
          Rollouts for <span class="font-mono">{rollouts.backend_id}</span> in
          <span class="font-mono">{rollouts.project_id}</span>
        </div>

        {#if rollouts.walk_incomplete}
          {@render qualification(rollouts.walk_incomplete, "Rollout listing")}
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
          <!-- Deploy duration and outcome, drawn by the same component the
               Actions runs use, so a deploy and the run that produced it are
               read the same way.

               The live poll here is narrower than the run timeline's. Listing
               rollouts is click-only because the Firebase CLI enables the App
               Hosting API on a project where it is off, and that is a change
               to a Cloud project rather than a read. The enabling happens when
               the API is OFF, though — so a listing that has already answered
               is proof it is on, and refreshing that exact target enables
               nothing. The first call for a project and backend therefore
               stays a click (`pollableTarget` is null until one succeeds), and
               only an already-answered target is refreshed while a rollout is
               still moving. `now` ticks only while the poll is live; otherwise
               it is the fetch instant, because nothing grows between renders. -->
          <div class="mb-2 min-w-0">
            <DeliveryTimeline
              title="Deploy duration and outcome"
              rows={rolloutRows}
              now={liveState.kind === "live" ? timelineNow : (fetchedAt ?? 0)}
              checked={rollouts.checked}
              truncated={rollouts.truncated || rollouts.walk_incomplete !== null}
              error={rollouts.error}
              sampleNoun="rollouts"
              live={liveState}
            />
          </div>
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
      {/if}

      {#if selectedProjectId && selectedBackend}
        <!-- The one action here that changes what production serves. Kept
             visually last and behind two steps: App Hosting has no rollback
             verb, so this cannot be undone from GitPulse, and it is not
             idempotent — a second click is a second deployment. -->
        <div class="mt-3 pt-3 border-t border-border/50">
          <h4 class="text-[11px] uppercase tracking-wider text-textMuted mb-2 flex items-center gap-1.5">
            <Rocket size={12} />
            Deploy a commit
          </h4>
          <div class="flex flex-wrap items-end gap-2">
            <label class="flex flex-col gap-1 text-[11px] text-textMuted">
              Commit
              <input
                class="gp-field text-xs font-mono w-[26rem] max-w-full"
                placeholder="full 40-character commit SHA"
                value={deploySha}
                oninput={(event) => {
                  deploySha = event.currentTarget.value;
                  // Editing the target invalidates a confirmation of the old one.
                  disarmDeploy();
                }}
              />
            </label>
            {#if !deployArmed}
              <button
                class="px-2.5 py-1.5 rounded-lg border border-border/70 bg-surface hover:bg-surfaceHover text-xs disabled:opacity-50"
                disabled={Boolean(deployProblem) || deployRunning}
                onclick={() => {
                  deployError = null;
                  deployOutcome = null;
                  deployArmed = true;
                }}
              >
                Review deploy
              </button>
            {:else}
              <button
                class="px-2.5 py-1.5 rounded-lg border border-rose-500/40 bg-rose-500/10 text-rose-700 dark:text-rose-300 hover:bg-rose-500/20 text-xs disabled:opacity-50"
                disabled={deployRunning}
                onclick={() => void deploy()}
              >
                Deploy {shortSha(deploySha.trim())} to {selectedBackend}
              </button>
              <button
                class="px-2.5 py-1.5 rounded-lg border border-border/70 bg-surface hover:bg-surfaceHover text-xs"
                onclick={() => disarmDeploy()}
              >
                Cancel
              </button>
            {/if}
          </div>

          {#if deployProblem && deploySha.trim().length > 0}
            <!-- The reason, not just a disabled button: an unexplained refusal
                 is the kind people work around by pasting something else. -->
            <p class="mt-2 text-[11px] text-amber-700 dark:text-amber-400 max-w-xl">
              {deployProblem}
            </p>
          {/if}

          {#if deployArmed}
            <div class="mt-2 p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300 text-[11px] max-w-xl">
              This starts a build of <span class="font-mono">{shortSha(deploySha.trim())}</span>
              and moves production traffic on
              <span class="font-mono">{selectedBackend}</span> in
              <span class="font-mono">{selectedProjectId}</span> to it. App Hosting has no
              rollback command, so GitPulse cannot undo this — reverting means deploying an
              earlier commit. The commit must exist in the backend's connected GitHub
              repository, which is not necessarily this checkout.
            </div>
          {/if}

          {#if deployError}
            <div class="mt-2 p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-700 dark:text-rose-300 text-xs max-w-xl">
              Rollout was not created: {deployError}
            </div>
          {:else if deployOutcome}
            <div class="mt-2 p-3 rounded-xl border border-green-500/30 bg-green-500/10 text-green-800 dark:text-green-300 text-xs max-w-xl">
              Rollout started for <span class="font-mono">{shortSha(deployOutcome.git_commit)}</span>
              on <span class="font-mono">{deployOutcome.backend_id}</span>.
              {#if deployOutcome.unconfirmed}
                <!-- A zero exit means the rollout began. Saying "it worked"
                     would overclaim; saying "it failed" would invite a retry
                     that deploys a second time. -->
                <span class="block mt-1 text-amber-700 dark:text-amber-400">
                  {deployOutcome.unconfirmed}
                </span>
              {/if}
            </div>
          {/if}
        </div>
      {/if}

      {#if lastVerdict}
        <div class="mt-1 text-[10px] text-textMuted">
          Policy on {lastVerdictAction}: {verdictLabel(lastVerdict)}
        </div>
      {/if}
    {/if}
  {/if}
</section>
