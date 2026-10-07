<script lang="ts">
  /**
   * The task sheet's Agents pane: the agents working on this task, a way to
   * start another, and the attempts that have ended.
   *
   * Launching used to take the reader off this sheet: every launch opened the
   * agent's checkout, brought the repository surface forward and opened its
   * terminal dock, so the sheet that launched it vanished behind it. A launch
   * now starts the agent where the reader stands (`taskTerminal.ts`), which
   * makes this pane the place they follow it from. So it leads with what is
   * working right now, each with the state of its terminal in *this* window —
   * starting, running, waiting for a slot, running somewhere unseen — and a
   * "Show terminal" for when they want to watch. Nothing here moves the reader
   * except that button.
   *
   * The launch form is `TaskHandoffForm`, shared with `TaskHandoffSheet`. When
   * an attempt is working the form folds away behind "New attempt", because
   * then that attempt is what the reader came for; with none, starting one is
   * the only thing this pane is for, and the form is open.
   */
  import { onDestroy, untrack } from "svelte";
  import { ChevronDown, ChevronRight, RotateCw, SquareTerminal } from "@lucide/svelte";
  import AgentDecisions from "./AgentDecisions.svelte";
  import TaskHandoffForm from "./TaskHandoffForm.svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { attemptNotices, clearAttemptNotice, consumeTaskTerminal, noteAttempt, pruneTaskTerminals, taskTerminalRequests } from "../terminal/taskLaunches";
  import { closeWithConfirmation, terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
  import { terminalSessionLimit } from "../terminal/sessionLimit";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { PERMISSION_LABELS } from "../terminal/agentDefaults";
  import { attemptStartNote, openAttemptCheckout, queuedTerminalNote, resumeTaskConversation, showAttemptTerminal, stopWatchingAttempt, trustAttemptCheckout } from "../workbench/taskTerminal";
  import { attemptWorktreeOffer, discardAttemptWorktree, mergeAttemptWorktree, mergeTargetLabel, reviewAttemptChanges } from "../workbench/attemptWorktree";
  import { describeCheckout } from "../terminal/checkoutLabel";
  import { asksForReader, attemptStage, checkoutChanges, monitorAttempt, orderMonitored, readPendingRequests, releasedNote, requestsLine, waitingLabel, type MonitorContext, type MonitoredAttempt, type PendingRequests } from "../workbench/taskSessions";
  import { sessionActivity } from "../terminal/sessionActivity";
  import { repoStore } from "../stores/repoStore";
  import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
  import { formatRelativeTime } from "../format";
  import {
    cancelTaskRun,
    explainError,
    getTaskRun,
    launchManagedRun,
    listPendingDecisions,
    listLiveTaskRuns,
    listTaskRuns,
    releaseTaskRun,
    stopManagedRun,
    type Repository,
    type Task,
    type TaskRun,
  } from "../workbench/client";
  import {
    PROVIDER_LABELS,
    canRelease,
    runExpired,
    runHoldsCheckout,
    runStatusLabel,
    sanitizeHandoff,
    type HandoffGate,
    type HandoffSettings,
  } from "../workbench/taskHandoff";
  import { TASK_RUN_POLL_MS, nextTaskRunPollDelay } from "../workbench/runPoll";
  import { readEventLoopDelay } from "../runtime/loadCadence";
  import { bindForegroundChanges, readBackgroundDocument } from "../runtime/foreground";
  import type { OpenTabRef } from "../workbench/openMembership";

  let { task, repositories, openTabs = [], disabled = false, dirty = false, active = true, onCount, onAttention }: {
    task: Task;
    repositories: Repository[];
    openTabs?: OpenTabRef[];
    /** Blocked for a reason other than unsaved edits (saving, deleting, Manvi). */
    disabled?: boolean;
    dirty?: boolean;
    active?: boolean;
    /** Attempts working right now (holding a checkout), for the tab badge. */
    onCount?: (count: number) => void;
    /** Of those, how many are asking for the reader (a request, an error). */
    onAttention?: (count: number) => void;
  } = $props();

  /** States whose terminal can still be shown (an `unresolved` one cannot). */
  const OPENABLE = ["prepared", "starting", "running"];
  /**
   * Ended states whose Claude Code conversation may be resumable. Offered, not
   * promised: the host answers from the transcript Claude Code saved, and says
   * why when there is none.
   */
  const RESUMABLE = ["exited", "failed", "cancelled"];
  /**
   * The history's page ceiling. The store pages runs; this bounds how many a
   * sheet keeps in memory and re-renders on every poll.
   */
  const MAX_LOADED_RUNS = 180;
  /**
   * One instant per render pass, refreshed wherever polling is reconsidered.
   *
   * A prepared attempt stops being live at a wall-clock moment, not on any
   * event, so without a value the view can depend on, an expired preparation
   * keeps rendering as "Prepared" — which is precisely what it did: the panel
   * stopped polling it the second it expired and then showed that last frame
   * forever, offering a recover button the store would always refuse.
   */
  let clock = $state(Date.now());

  let settings = $state<HandoffSettings>(untrack(() => sanitizeHandoff($interfaceStore.taskHandoff)));
  let gate = $state<HandoffGate>({ ok: false, reason: "" });
  let launching = $state(false);
  let preparePending = $state(false);
  let form = $state<{ launch: () => Promise<void> }>();

  let detail = $state<TaskRun | null>(null);
  let busy = $state(false);
  let loading = $state(false);
  let historyError = $state("");
  /**
   * Each attempt's one message slot: what its last action said, or why it
   * failed. Keyed by run, so a message stays with the attempt it is about —
   * the pane used to say everything in one line at the top, and the form said
   * what happened after a launch inside itself, after it had folded away.
   */
  let messages = $state<Record<string, { tone: "note" | "error"; text: string }>>({});
  /**
   * The attempt being prepared, from the moment Launch is pressed until the
   * store answers. Keyed by the id the run will have, so the row the run
   * then fills is the same row.
   */
  let preparing = $state<{ id: string; provider: TaskRun["provider"]; kind: TaskRun["kind"]; worktree: boolean } | null>(null);
  let runs = $state<TaskRun[]>([]);
  let total = $state(0);
  let cursor = $state<string | null>(null);
  let reviewingRunID = $state<string | null>(null);
  let decisionRefresh = $state(0);
  let expandedForm = $state<boolean | null>(null);
  let disposed = false;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Every attempt holding a checkout, read apart from the history's pages:
   * "the agents working on this task" must not depend on how far the reader
   * has paged. `liveComplete` is false when a state overflowed its page, and
   * the summary then says "at least".
   */
  let liveRuns = $state<TaskRun[]>([]);
  let liveComplete = $state(true);
  /** Pending managed requests per run, from each one's first page. */
  let pending = $state<ReadonlyMap<string, PendingRequests>>(new Map());
  /** A one-second clock for "output 4s ago", running only while it is read. */
  let now = $state(Date.now());
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };

  const capacityFull = $derived($terminalSessions.length >= $terminalSessionLimit);
  /** History and live reads joined by id; the newer revision of a run wins. */
  const allRuns = $derived.by(() => {
    const byId = new Map<string, TaskRun>();
    for (const run of [...runs, ...liveRuns]) {
      const seen = byId.get(run.id);
      if (!seen || run.revision >= seen.revision) byId.set(run.id, run);
    }
    return [...byId.values()];
  });
  /** What this window knows beside the store, as of the last reads. */
  const context = $derived<MonitorContext>({
    records: $terminalSessions,
    requests: $taskTerminalRequests,
    capacityFull,
    activity: (sessionId) => $sessionActivity.get(sessionId),
    pending: (runId) => pending.get(runId),
    now,
    clock,
    notices: (runId) => $attemptNotices.get(runId),
    trustPending: (repoPath) => {
      const key = identityKey(repoPath, pathOpts);
      return !!key && $repoStore.openTabs.some((tab) => tab.trustRequired && identityKey(tab.path, pathOpts) === key);
    },
  });
  /** One attempt as the pane describes it (`taskSessions.ts::monitorAttempt`, shared with the board). */
  function monitor(run: TaskRun): MonitoredAttempt {
    return monitorAttempt(run, context);
  }
  /**
   * What each working attempt is doing, most urgent first. One owner for the
   * row, the order, the summary and the badge, so they cannot disagree.
   */
  const monitored = $derived.by((): MonitoredAttempt[] => {
    const rows = allRuns.filter((run) => runHoldsCheckout(run, clock)).map(monitor);
    return orderMonitored(rows);
  });
  const live = $derived(monitored.map((row) => row.run));
  /** The attempt being prepared, until a run with its id is on the pane. */
  const pendingRow = $derived(preparing && !allRuns.some((run) => run.id === preparing?.id) ? preparing : null);
  const ended = $derived(allRuns.filter((run) => !runHoldsCheckout(run, clock)).sort((a, b) => b.created_at - a.created_at));
  const askingCount = $derived(monitored.filter((row) => asksForReader(row.tone)).length);
  // Null means "nobody has chosen"; the default follows whether a run is live.
  const formOpen = $derived(expandedForm ?? live.length === 0);
  const summary = $derived.by(() => {
    if (!total && !allRuns.length) return loading ? "Loading attempts…" : "No attempts yet.";
    const floor = liveComplete ? "" : "at least ";
    const working = live.length ? `${floor}${live.length} working now` : "Nothing working now";
    const asking = askingCount ? ` · ${askingCount} ${askingCount === 1 ? "needs" : "need"} you` : "";
    const count = Math.max(total, allRuns.length);
    return `${working}${asking} · ${count} ${count === 1 ? "attempt" : "attempts"}`;
  });

  $effect(() => { onCount?.(live.length); });
  $effect(() => { onAttention?.(askingCount); });

  $effect(() => {
    // Ticks only while there is a working attempt to describe and the page
    // is in front; a hidden sheet or a background window costs nothing.
    if (!active || live.length === 0) return;
    let interval: ReturnType<typeof setInterval> | null = null;
    const start = () => { if (!interval && !readBackgroundDocument()) { now = Date.now(); interval = setInterval(() => { now = Date.now(); }, 1000); } };
    const stop = () => { if (interval) clearInterval(interval); interval = null; };
    start();
    const unbind = bindForegroundChanges(document, typeof window === "undefined" ? null : window, () => { if (readBackgroundDocument()) stop(); else start(); });
    return () => { unbind(); stop(); };
  });

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (disposed || !active) return;
    // Advanced before the decision below, so "keep polling" and "still live"
    // are answered against one instant. The final tick after an expiry is what
    // flips the row from Prepared to expired and then stops the loop.
    clock = Date.now();
    const delay = nextTaskRunPollDelay({
      baseMs: TASK_RUN_POLL_MS,
      lagMs: readEventLoopDelay(),
      background: readBackgroundDocument(),
      live: allRuns.some((run) => ["starting", "running"].includes(run.state) || (run.state === "prepared" && run.expires_at * 1000 > clock)),
      active: true,
    });
    if (delay === null) return;
    timer = setTimeout(() => { timer = null; void refresh(); }, delay);
  }
  async function refresh(more = false) {
    if (loading || disposed || !active) return;
    loading = true;
    const ticket = generation;
    try {
      const [page, working] = await Promise.all([
        listTaskRuns(task.id, more ? cursor ?? undefined : undefined),
        more ? null : listLiveTaskRuns(task.id),
      ]);
      if (disposed || ticket !== generation) return;
      runs = more ? [...new Map([...runs, ...page.items].map((run) => [run.id, run])).values()] : page.items;
      if (working) { liveRuns = working.runs; liveComplete = working.complete; }
      total = page.total; cursor = page.next_cursor;
      // A terminal request whose attempt can no longer start (ended, expired
      // while it waited, cancelled elsewhere) must not outlive it: judged
      // against these same reads, never against a run they did not return.
      pruneTaskTerminals(working ? [...page.items, ...working.runs] : page.items, Date.now());
      decisionRefresh += 1;
      historyError = "";
      if (working) void readPending(working.runs, ticket);
    } catch (cause) { if (!disposed && ticket === generation) historyError = explainError(cause); }
    finally { loading = false; schedule(); }
  }
  /**
   * How many requests each working managed attempt is waiting on. A run whose
   * read fails shows no count rather than zero: "none pending" would be a
   * claim nothing checked.
   */
  async function readPending(working: TaskRun[], ticket: number) {
    const read = await readPendingRequests(working, listPendingDecisions);
    if (disposed || ticket !== generation) return;
    pending = read;
  }
  /** Replaces a run wherever this pane holds it. */
  function replaceRun(next: TaskRun) {
    runs = runs.map((item) => item.id === next.id ? next : item);
    liveRuns = liveRuns.map((item) => item.id === next.id ? next : item);
  }
  $effect(() => {
    if (!active) { if (timer) clearTimeout(timer); timer = null; return; }
    untrack(() => { void refresh(); });
    const wake = () => {
      if (readBackgroundDocument()) { if (timer) { clearTimeout(timer); timer = null; } return; }
      void refresh();
    };
    const unbind = bindForegroundChanges(document, typeof window === "undefined" ? null : window, wake);
    return () => { unbind(); if (timer) clearTimeout(timer); timer = null; };
  });
  onDestroy(() => { disposed = true; generation += 1; if (timer) clearTimeout(timer); });

  /** Puts a run on the pane (history first), counted once whichever callback brings it. */
  function admit(run: TaskRun) {
    const known = runs.some((item) => item.id === run.id);
    runs = [run, ...runs.filter((item) => item.id !== run.id)];
    if (!known) total += 1;
    total = Math.max(total, runs.length);
    if (preparing?.id === run.id) preparing = null;
    schedule();
  }
  /**
   * The store accepted the attempt. It is listed now, before its start can
   * fail — but the form stays open: a start that then fails is said on the
   * attempt's row, and the reader may want to launch again.
   */
  function prepared(run: TaskRun) {
    admit(run);
  }
  /**
   * The launch was handed off. The form folds away once the attempt has
   * really started — its process reported running, or the store saw it
   * claimed — because that is the answer to the form's question. A start that
   * fails leaves the form open for another try, with the failure on the row.
   */
  function launched(run: TaskRun) {
    admit(run);
    if (run.kind === "managed") reviewingRunID = run.id;
    awaitingStart = run.id;
  }
  let awaitingStart = $state<string | null>(null);
  $effect(() => {
    const id = awaitingStart;
    if (!id) return;
    const run = allRuns.find((item) => item.id === id);
    const notice = $attemptNotices.get(id);
    const started = notice?.phase === "running" || (!!run && ["starting", "running"].includes(run.state));
    // A recorded failure does not end the wait: a start whose reply this
    // window lost is still claimed by the host, which the next read shows.
    if (!started && !(run && !runHoldsCheckout(run, clock))) return;
    untrack(() => {
      if (started) expandedForm = false;
      awaitingStart = null;
    });
  });
  function say(runId: string, tone: "note" | "error", text: string) {
    if (!disposed) messages = { ...messages, [runId]: { tone, text } };
  }
  function unsay(runId: string) {
    if (!messages[runId]) return;
    const next = { ...messages };
    delete next[runId];
    messages = next;
  }
  /** Runs one row action with the shared busy flag and that row's message slot. */
  async function act(run: Pick<TaskRun, "id">, work: () => Promise<void>) {
    busy = true; unsay(run.id);
    try { await work(); }
    catch (cause) { say(run.id, "error", explainError(cause)); }
    finally { if (!disposed) { busy = false; schedule(); } }
  }
  function showTerminal(run: TaskRun) {
    return act(run, async () => {
      const outcome = await showAttemptTerminal(run);
      const said = attemptStartNote(outcome);
      if (said) say(run.id, outcome.kind === "failed" ? "error" : "note", said);
    });
  }
  function trust(run: TaskRun) {
    return act(run, async () => {
      if (!(await trustAttemptCheckout(run))) say(run.id, "note", `${run.cwd} is not open in GitPulse. Show terminal opens it and asks.`);
    });
  }
  function showSession(run: TaskRun, record: TerminalSessionRecord) {
    return act(run, async () => {
      const outcome = await focusTerminalSession(record);
      if (!outcome.ok) say(run.id, "error", "That session can no longer be shown. It may have just ended.");
    });
  }
  function stopSession(run: TaskRun, record: TerminalSessionRecord) {
    return act(run, async () => {
      if (!(await closeWithConfirmation(record))) return;
      say(run.id, "note", record.taskRunId === run.id
        ? `${PROVIDER_LABELS[run.provider] ?? run.provider} was stopped. The attempt has ended; start a new one to continue.`
        : "The resumed conversation was stopped.");
    });
  }
  function cancel(run: TaskRun) {
    return act(run, async () => {
      const saved = await cancelTaskRun(run);
      // Withdrawn everywhere this window was starting it: the request, the
      // watch that would have toasted its start, and what its row said.
      consumeTaskTerminal(run.id);
      stopWatchingAttempt(run.id);
      clearAttemptNotice(run.id);
      replaceRun(saved);
    });
  }
  async function manage(run: TaskRun) {
    const current = await launchManagedRun(run.id);
    if (disposed) return;
    replaceRun(current);
    reviewingRunID = current.id;
    // Recovered: whatever failure was recorded for its start is history.
    if (current.state === "running") noteAttempt(current.id, "running", `Managed ${PROVIDER_LABELS[current.provider]} is running.`);
    say(current.id, "note", current.state === "running" ? `Managed ${PROVIDER_LABELS[current.provider]} started. Review its requests below.` : `Managed attempt: ${current.state}.`);
  }
  function resumeManaged(run: TaskRun) { return act(run, () => manage(run)); }
  function stopManaged(run: TaskRun) {
    return act(run, async () => { await stopManagedRun(run.id); say(run.id, "note", "Stop requested. Waiting for process confirmation."); });
  }
  /**
   * Ask the host to free a checkout this attempt still holds. It decides on
   * process evidence, so the answer is either a release or the reason there
   * was none — never a silent no-op.
   */
  function release(run: TaskRun) {
    return act(run, async () => {
      const result = await releaseTaskRun(run.id);
      if (disposed) return;
      replaceRun(result.run);
      say(run.id, "note", result.released ? releasedNote(result.run) : result.reason);
    });
  }
  function inspect(run: TaskRun) {
    return act(run, async () => { const current = await getTaskRun(run.id); if (!disposed) detail = current; });
  }
  function resume(run: TaskRun) {
    return act(run, async () => {
      const result = await resumeTaskConversation(run);
      if (result.outcome === "unavailable") say(run.id, "note", result.reason);
      else if (result.outcome === "queued") say(run.id, "note", queuedTerminalNote(run.cwd));
      else say(run.id, "note", "Resuming the conversation in its checkout. It is listed under this attempt; Show conversation to continue it.");
    });
  }

  function showWaitingConversation(run: TaskRun) {
    return act(run, async () => {
      const result = await resumeTaskConversation(run, "show");
      if (result.outcome === "unavailable") say(run.id, "note", result.reason);
      else if (result.outcome === "queued") say(run.id, "note", queuedTerminalNote(run.cwd));
    });
  }

  /**
   * Brings the attempt's checkout forward as the active repository tab. Only
   * on request, like Show terminal; it starts nothing.
   */
  function openCheckout(run: TaskRun) {
    return act(run, async () => {
      if (!(await openAttemptCheckout(run))) say(run.id, "error", `${run.cwd} did not open. If its repository tab shows an error, that is why; otherwise try again.`);
    });
  }
  /** Its uncommitted changes, in the repository view. Moves the reader, on request. */
  function review(run: TaskRun) {
    return act(run, () => reviewAttemptChanges(run));
  }
  /**
   * Merge or discard. The host refuses either while a live run works in the
   * worktree; that refusal (and any other) lands in this row's message slot.
   */
  function merge(run: TaskRun) {
    return act(run, async () => {
      const result = await mergeAttemptWorktree(run);
      if (result) say(run.id, "note", result.message);
    });
  }
  function discard(run: TaskRun) {
    return act(run, async () => {
      const result = await discardAttemptWorktree(run);
      if (result) say(run.id, "note", result.message);
    });
  }
  function permissionLabel(run: TaskRun): string {
    return PERMISSION_LABELS[run.permission_mode]?.label ?? run.permission_mode;
  }
</script>

{#snippet runCard(run: TaskRun)}
  {@const expired = runExpired(run, clock)}
  {@const holding = runHoldsCheckout(run, clock)}
  {@const row = monitor(run)}
  {@const view = row.view}
  {@const own = view.sessions.find((session) => session.role === "attempt")}
  {@const changes = holding ? checkoutChanges(run.cwd, $repoStore.openTabs, pathOpts, { runId: run.id, peers: live }) : null}
  {@const place = describeCheckout(run.cwd, $repoStore.openTabs, pathOpts)}
  {@const asked = requestsLine(holding ? pending.get(run.id) : undefined)}
  {@const stage = attemptStage(row, clock)}
  {@const offer = attemptWorktreeOffer(run, clock)}
  {@const message = messages[run.id]}
  <article class="run" class:is-live={holding} data-tone={holding ? row.tone : null} data-testid="agent-run" data-run-id={run.id} data-run-state={run.state}>
    <div class="run-head">
      <strong>{PROVIDER_LABELS[run.provider] ?? run.provider}</strong>
      <span class="kind">{run.kind === "managed" ? "Managed" : "Terminal"}</span>
      <!-- One ladder for every attempt (taskSessions.ts::attemptStage). Polite,
           so a transition is announced without interrupting; fixed in the
           head row, so a longer label wraps the row rather than moving it. -->
      <span class="state" role="status" aria-live="polite" data-tone={stage.tone} data-stage={stage.stage} data-testid="agent-stage" title={runStatusLabel(run, clock)}>{stage.label}</span>
    </div>
    <small class="facts">
      <span title={run.cwd} data-testid="agent-checkout">{place.repository}{#if place.checkout} / {place.checkout}{/if}</span>
      · Revision {run.source_revision} · {permissionLabel(run)}
      {#if run.created_at} · started {formatRelativeTime(run.created_at, Math.floor(now / 1000))}{/if}
      {#if run.exit_code !== null} · exit {run.exit_code}{/if}
    </small>
    {#if changes}
      <!-- From the repository tab GitPulse has open on this checkout, and only
           when that tab has read its status: a 0 from a tab still loading is a
           read that did not happen. A shared checkout's changes may be anyone's. -->
      <small class="changes" data-testid="agent-changes" title={changes.shared ? "This attempt runs in a checkout it shares, so these changes may not all be its own." : "Uncommitted changes in this attempt's worktree."}>
        {changes.files === 0 ? "No uncommitted changes" : `${changes.files} ${changes.files === 1 ? "file" : "files"} changed`}{changes.shared ? " in this shared checkout" : ""}{changes.branch ? ` · ${changes.branch}` : ""}
      </small>
    {/if}

    {#if view.sessions.length || view.waiting || row.disconnected || row.unstarted || asked || row.failure}
      <ul class="sessions" aria-label="What this attempt is doing">
        {#each view.sessions as session, index (session.record.key)}
          {@const glance = row.glances[index]}
          <li data-phase={session.phase} data-tone={glance.tone} data-testid="agent-session">
            <span class="dot" aria-hidden="true"></span>
            <span class="glance">
              <span class="headline">{session.role === "resumed" ? "Resumed conversation · " : ""}{glance.headline}</span>
              {#if glance.detail}<span class="said" data-testid="agent-said">{glance.detail}</span>{/if}
              {#if glance.title}<span class="doing" title="What the program calls itself right now" data-testid="agent-title">{glance.title}</span>{/if}
            </span>
          </li>
        {/each}
        {#if asked}
          <li data-phase="requests" data-tone={asked.tone} data-testid="agent-requests"><span class="dot" aria-hidden="true"></span><span class="glance"><span class="headline">{asked.text}</span></span></li>
        {/if}
        {#if view.waiting}
          <li data-phase="waiting" data-reason={view.waiting.reason} data-tone={view.waiting.reason === "trust" ? "needs-you" : "starting"} data-testid="agent-session-waiting"><span class="dot" aria-hidden="true"></span><span class="glance">{waitingLabel(view.waiting, $terminalSessionLimit)}</span></li>
        {/if}
        {#if row.failure}
          <!-- Recorded by this window when the start failed: the checkout
               would not open, the host refused the spawn (a missing or old
               CLI), a managed start failed. Without it, a spawn refused in a
               hidden tab read as a quiet "not connected". -->
          <li data-phase="failed" data-tone="error" data-testid="agent-session-failed"><span class="dot" aria-hidden="true"></span><span class="glance"><span class="headline">Did not start</span><span class="cause">{row.failure}</span></span></li>
        {/if}
        {#if row.disconnected}
          <!-- The store says it runs; no session in this window is attached
               to it (its start reply was lost, another window holds it, or a
               reload has not adopted it). Never "running here". -->
          <li data-phase="elsewhere" data-tone="problem" data-testid="agent-session-elsewhere"><span class="dot" aria-hidden="true"></span><span class="glance">Running, but no terminal in this window is connected to it. Show terminal to reconnect.</span></li>
        {/if}
        {#if row.unstarted}
          <!-- Prepared, and no session here is attached: never started in
               this window (the launch queue is per window, so a reload
               forgets it), or its start reply was lost. Either way the one
               honest statement is "not connected", and Show terminal is the
               path for both: it starts the attempt or reconnects to it. -->
          <li data-phase="unstarted" data-tone="quiet" data-testid="agent-session-unstarted"><span class="dot" aria-hidden="true"></span><span class="glance">Not connected to a terminal in this window. Show terminal starts it or reconnects to it; Cancel preparation withdraws it.</span></li>
        {/if}
      </ul>
    {/if}

    {#if run.reason}<p>{run.reason}</p>{/if}
    {#if run.state === "unresolved"}<p class="error">This attempt's outcome is unresolved and it still holds its checkout. Release it once its agent has stopped; another worktree can run meanwhile.</p>
    {:else if run.outcome_uncertain}<p>Released without an observed exit. Review what it changed before relying on it.</p>{/if}
    {#if expired}<p>This preparation expired before it started, so it no longer holds its checkout. Cancel it to clear it from the history, then prepare a new attempt.</p>{/if}
    {#if message}<p class="row-message" data-tone={message.tone} role={message.tone === "error" ? "alert" : "status"} data-testid="agent-run-message">{message.text}</p>{/if}

    <div class="actions">
      {#if run.kind === "external_terminal" && OPENABLE.includes(run.state) && !expired}
        <button class="gp-btn-primary" type="button" onclick={() => showTerminal(run)} disabled={busy}><SquareTerminal size={12} /> Show terminal</button>
      {/if}
      {#if view.waiting?.role === "attempt" && view.waiting.reason === "trust"}
        <button class="gp-btn-primary" type="button" onclick={() => trust(run)} disabled={busy} data-testid="agent-trust" title="Asks whether to trust this checkout's repository. The agent starts once you do.">Trust checkout…</button>
      {/if}
      {#if own && own.phase !== "adopted"}
        <button class="gp-btn" type="button" onclick={() => stopSession(run, own.record)} disabled={busy} title="Stops the agent. You are asked first, because stopping ends this attempt.">Stop agent</button>
      {/if}
      {#if view.waiting?.role === "resumed" && view.waiting.reason === "checkout"}
        <!-- Its checkout did not open in the background; opening it in front
             is what starts the conversation, and says why if it cannot. -->
        <button class="gp-btn-primary" type="button" onclick={() => showWaitingConversation(run)} disabled={busy}><SquareTerminal size={12} /> Show conversation</button>
      {/if}
      {#each view.sessions.filter((session) => session.role === "resumed") as session (session.record.key)}
        <button class="gp-btn" type="button" onclick={() => showSession(run, session.record)} disabled={busy}>Show conversation</button>
        <button class="gp-btn" type="button" onclick={() => stopSession(run, session.record)} disabled={busy}>Stop conversation</button>
      {/each}
      {#if run.kind === "managed"}
        {#if ["prepared", "starting"].includes(run.state) && !expired}<button class="gp-btn" type="button" onclick={() => resumeManaged(run)} disabled={busy}>Start or recover managed launch</button>{/if}
        {#if ["starting", "running", "unresolved"].includes(run.state)}<button class="gp-btn" type="button" onclick={() => stopManaged(run)} disabled={busy}>Stop managed run</button>{/if}
        <button class="gp-btn" type="button" onclick={() => inspect(run)} disabled={busy}>View output and settings</button>
      {/if}
      {#if run.provider === "claude" && RESUMABLE.includes(run.state) && !view.sessions.some((session) => session.role === "resumed")}<button class="gp-btn" type="button" onclick={() => resume(run)} disabled={busy} title="Starts a Claude Code tab in this attempt's checkout that continues its conversation, in the same permission mode. You stay on this task.">Resume conversation</button>{/if}
      {#if run.state === "prepared"}<button class="gp-btn" type="button" onclick={() => cancel(run)} disabled={busy}>Cancel preparation</button>{/if}
      <button class="gp-btn" type="button" onclick={() => openCheckout(run)} disabled={busy} data-testid="agent-open-checkout" title={`Opens ${run.cwd} as the active repository tab. Starts nothing.`}>Open checkout</button>
      {#if offer.review}<button class="gp-btn" type="button" onclick={() => review(run)} disabled={busy} data-testid="agent-review" title={`Opens ${run.cwd} on its uncommitted changes.`}>Review changes</button>{/if}
      {#if offer.ownWorktree}
        {@const target = mergeTargetLabel(run.cwd)}
        <button class="gp-btn" type="button" onclick={() => merge(run)} disabled={busy} data-testid="agent-merge" title="Merges this attempt's branch into the main checkout's branch and removes its worktree. You are asked first.">{target ? `Merge into ${target}` : "Merge into main checkout"}</button>
        <button class="gp-btn" type="button" onclick={() => discard(run)} disabled={busy} data-testid="agent-discard" title="Removes this attempt's worktree. You are told how many uncommitted files are lost, and asked first.">Discard worktree</button>
      {/if}
      {#if canRelease(run)}<button class="gp-btn" type="button" onclick={() => release(run)} disabled={busy} title="Frees the checkout if the agent and the GitPulse that launched it have both stopped. A running agent is never released. The worktree and its changes stay on disk.">Release checkout</button>{/if}
      {#if run.session_id && run.kind === "managed"}<button class="gp-btn" type="button" onclick={() => { reviewingRunID = reviewingRunID === run.id ? null : run.id; }}>{reviewingRunID === run.id ? "Hide requests" : "Review requests"}</button>{/if}
    </div>

    {#if run.kind === "managed" && run.provider_state}<p>Provider turn: {run.provider_state}. {run.provider_state === "completed" ? "Review the result before accepting the task." : ""}</p>{/if}
    {#if run.kind === "managed" && detail?.id === run.id}
      <section aria-label="Managed run details">
        <p>Saved output · revision {detail.revision}</p>
        <textarea class="gp-field gp-field-multi" readonly rows="7" aria-label="Managed agent output" value={detail.output ?? "No output saved yet."}></textarea>
        {#if detail.output_truncated}<p>Output reached the 128 KiB retention limit.</p>{/if}
        <details><summary>Effective provider settings</summary><textarea class="gp-field gp-field-multi" readonly rows="7" aria-label="Effective provider settings" value={detail.effective_configuration ?? "Settings are not recorded yet."}></textarea></details>
        <div class="actions"><button class="gp-btn" type="button" onclick={() => inspect(run)} disabled={busy}>Refresh output</button><button class="gp-btn" type="button" onclick={() => { detail = null; }}>Close details</button></div>
      </section>
    {/if}
    {#if reviewingRunID === run.id}<AgentDecisions {run} {active} refreshToken={decisionRefresh} />{/if}
  </article>
{/snippet}

<section class="task-runs" aria-label="Task agent runs" data-testid="task-agent-panel">
  <header class="agents-head">
    <div class="min-w-0">
      <h3>Agents</h3>
      <p class="meta" data-testid="agents-summary">{summary}</p>
    </div>
    <button class="gp-btn" type="button" onclick={() => refresh()} disabled={loading} title="Refresh this task's attempts"><RotateCw size={12} /> Refresh</button>
  </header>

  {#if historyError}<p class="error" role="alert">{historyError}</p>{/if}

  {#if live.length || pendingRow}
    <h4 class="group-heading">Working now</h4>
    <div class="run-list" data-testid="agents-working">
      {#if pendingRow}
        <!-- Optimistic: shown from the moment Launch is pressed until the
             store answers, then replaced by the run, which has this id. -->
        <article class="run is-live" data-testid="agent-run-pending" data-run-id={pendingRow.id}>
          <div class="run-head">
            <strong>{PROVIDER_LABELS[pendingRow.provider] ?? pendingRow.provider}</strong>
            <span class="kind">{pendingRow.kind === "managed" ? "Managed" : "Terminal"}</span>
            <span class="state" role="status" aria-live="polite" data-tone="live" data-stage="preparing" data-testid="agent-stage">{pendingRow.worktree ? "Preparing worktree" : "Preparing"}</span>
          </div>
          <small class="facts">{pendingRow.worktree ? "Creating its worktree and running the repository's setup, then the agent starts." : "Checking the task and the checkout, then the agent starts."}</small>
        </article>
      {/if}
      {#each live as run (run.id)}{@render runCard(run)}{/each}
    </div>
  {/if}

  <button
    type="button"
    class="form-toggle"
    data-testid="task-agent-toggle"
    aria-expanded={formOpen}
    aria-controls="task-agent-launch"
    onclick={() => { expandedForm = !formOpen; }}
  >
    {#if formOpen}<ChevronDown size={12} />{:else}<ChevronRight size={12} />{/if}
    <span class="flex-1 text-left" data-testid="task-agent-toggle-label">{live.length ? "New attempt" : "Send to an agent"}</span>
    <span class="meta">Revision {task.revision}</span>
  </button>

  <div id="task-agent-launch" hidden={!formOpen}>
    <TaskHandoffForm
      bind:this={form}
      bind:settings
      taskId={task.id}
      revision={task.revision}
      repositoryIds={task.repository_ids}
      primaryRepositoryId={task.primary_repository_id}
      {repositories}
      {openTabs}
      {dirty}
      {disabled}
      onPrepared={prepared}
      onLaunched={launched}
      onPreparing={(draft) => {
        preparing = draft;
        // Pinned open for the launch in flight: the run becoming live must
        // not fold the form before its start is confirmed (see `launched`).
        if (draft && formOpen) expandedForm = true;
      }}
      onGate={(next) => { gate = next; }}
      onBusy={(next) => { launching = next; }}
      onPending={(next) => { preparePending = next; }}
    />
    <div class="launch-row">
      <button class="gp-btn-primary" type="button" onclick={() => void form?.launch()} disabled={!gate.ok}>
        {launching ? "Preparing…" : preparePending ? "Retry preparation" : settings.kind === "managed" ? `Start managed ${PROVIDER_LABELS[settings.provider]}` : `Launch in ${PROVIDER_LABELS[settings.provider]}`}
      </button>
      {#if !gate.ok && gate.reason}<span class="meta gate">{gate.reason}</span>{/if}
    </div>
    <p class="meta">The agent starts in the background and you stay on this task; it is listed above with a Show terminal button. An agent with the GitPulse MCP server moves this task to Done — or to Review, saying what remains — when it finishes. Its process ending never does.</p>
  </div>

  <div class="history-heading">
    <h4>History{#if ended.length}<span class="meta"> · {ended.length}{cursor ? "+" : ""}</span>{/if}</h4>
    {#if total}<span class="meta">{runs.length} of {total} loaded</span>{/if}
  </div>
  {#if !runs.length}<p>{loading ? "Loading runs…" : "No runs yet."}</p>
  {:else if !ended.length}<p>No ended attempts{cursor ? " loaded yet" : ""}.</p>{/if}
  <div class="run-list" data-testid="agents-history">
    {#each ended as run (run.id)}{@render runCard(run)}{/each}
  </div>
  {#if cursor}<button class="gp-btn" type="button" onclick={() => refresh(true)} disabled={loading || runs.length >= MAX_LOADED_RUNS}>Load more ({runs.length} of {total})</button>{/if}
</section>

<style>
  .task-runs{font-size:12px;padding-top:2px;display:flex;flex-direction:column;gap:10px}
  .agents-head{display:flex;align-items:flex-start;justify-content:space-between;gap:10px}
  .agents-head h3{margin:0;font-size:13px;font-weight:650}
  .agents-head .meta{margin:2px 0 0}
  h4{margin:0;font-weight:650}
  .group-heading{font-size:10px;letter-spacing:.04em;text-transform:uppercase;color:rgb(var(--c-text-muted))}
  p{color:rgb(var(--c-text-muted));line-height:1.5;margin:0}
  .meta{color:rgb(var(--c-text-muted));font-size:11px}
  .form-toggle{display:flex;align-items:center;gap:7px;width:100%;padding:7px 8px;border:1px solid rgb(var(--c-border));border-radius:8px;font-weight:600;background:rgb(var(--c-bg)/.35)}
  .form-toggle:hover{background:rgb(var(--c-surface-hover)/.7)}
  .form-toggle:focus-visible{outline:2px solid rgb(var(--c-accent)/.6);outline-offset:-1px}
  #task-agent-launch[hidden]{display:none}
  .launch-row{display:flex;align-items:center;gap:9px;flex-wrap:wrap;margin:12px 0 8px}
  .gate{flex:1;min-width:8rem}
  .history-heading{display:flex;align-items:center;justify-content:space-between;gap:8px;margin-top:8px;padding-top:12px;border-top:1px solid rgb(var(--c-border))}
  .run-list{display:flex;flex-direction:column;gap:8px}
  .run{padding:10px 11px;border:1px solid rgb(var(--c-border));border-radius:9px;background:rgb(var(--c-bg)/.3);min-width:0}
  .run.is-live{border-color:rgb(var(--c-accent)/.55);box-shadow:inset 3px 0 0 rgb(var(--c-accent))}
  .run p{margin:7px 0 0}
  .run-head{display:flex;align-items:center;gap:7px;flex-wrap:wrap;min-width:0}
  .kind{font-size:11px;color:rgb(var(--c-text-muted))}
  .state{margin-left:auto;font-size:10.5px;font-weight:600;padding:1px 7px;border-radius:999px;background:rgb(var(--c-text-muted)/.12);color:rgb(var(--c-text-muted));white-space:nowrap}
  .state[data-tone="live"]{background:rgb(var(--c-accent)/.14);color:rgb(var(--c-accent))}
  .state[data-tone="bad"]{background:rgb(220 101 101/.14);color:#dc6565}
  .state[data-tone="ask"]{background:rgb(210 153 34/.16);color:#d29922}
  .row-message{padding:6px 8px;border-radius:7px;background:rgb(var(--c-accent)/.08);color:rgb(var(--c-text));overflow-wrap:anywhere}
  .row-message[data-tone="error"]{background:rgb(220 101 101/.1);color:#dc6565}
  .cause{color:rgb(var(--c-text));overflow-wrap:anywhere}
  li[data-tone="error"] .cause{color:#dc6565}
  .facts{display:block;color:rgb(var(--c-text-muted));margin:5px 0 0;overflow-wrap:anywhere}
  .sessions{list-style:none;margin:8px 0 0;padding:0;display:flex;flex-direction:column;gap:5px}
  .sessions li{display:flex;align-items:baseline;gap:7px;color:rgb(var(--c-text));line-height:1.45;min-width:0}
  .glance{display:flex;flex-direction:column;min-width:0;flex:1}
  .headline{font-weight:550}
  .said{color:rgb(var(--c-text));overflow-wrap:anywhere}
  .said::before{content:"“"}
  .said::after{content:"”"}
  .doing{color:rgb(var(--c-text-muted));font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
  .changes{display:block;margin-top:3px;color:rgb(var(--c-text-muted))}
  .dot{width:7px;height:7px;border-radius:999px;flex-shrink:0;transform:translateY(-1px);background:rgb(var(--c-text-muted)/.6)}
  li[data-tone="active"] .dot{background:#3fb950}
  li[data-tone="needs-you"] .dot{background:#d29922;box-shadow:0 0 0 3px rgb(210 153 34/.22)}
  li[data-tone="starting"] .dot,li[data-tone="signalled"] .dot{background:#d29922}
  li[data-tone="finished"] .dot{background:rgb(var(--c-accent))}
  li[data-tone="error"] .dot,li[data-tone="problem"] .dot{background:#dc6565}
  li[data-tone="adopted"] .dot{background:rgb(var(--c-accent)/.6)}
  li[data-tone="needs-you"] .headline{color:#d29922}
  li[data-tone="error"] .headline{color:#dc6565}
  .run[data-tone="needs-you"],.run[data-tone="error"]{border-color:rgb(210 153 34/.6);box-shadow:inset 3px 0 0 #d29922}
  .run[data-tone="error"]{border-color:rgb(220 101 101/.6);box-shadow:inset 3px 0 0 #dc6565}
  @media (prefers-reduced-motion:no-preference){li[data-tone="active"] .dot{animation:gp-agent-pulse 1.6s ease-in-out infinite}}
  @keyframes gp-agent-pulse{50%{opacity:.45}}
  .actions{display:flex;flex-wrap:wrap;gap:6px;margin-top:9px}
  .actions :global(svg){display:inline;vertical-align:-2px;margin-right:3px}
  textarea{min-width:0;width:100%;padding:7px;border:1px solid rgb(var(--c-border));border-radius:6px;background:var(--mac-fill-bg,rgb(var(--c-bg)));color:inherit;margin-top:6px}
  button:disabled{opacity:.5}
  .error{color:#dc6565}
</style>
