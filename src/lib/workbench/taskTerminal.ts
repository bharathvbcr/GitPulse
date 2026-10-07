/**
 * A prepared task attempt's terminal: starting it, and showing it. One
 * implementation, used by the handoff form after a launch and by the task's
 * Agents pane.
 *
 * Starting and showing are two decisions, and they used to be one. Every
 * launch opened the checkout as the active repository, brought the repository
 * surface forward and opened its dock — because only a shown dock could
 * start the process. So pressing Launch in a task sheet took the reader off
 * the sheet they had just used, and every further attempt from it meant
 * finding the task again. Now a launch starts the agent where it stands: the
 * checkout opens as a background tab, the dock hosts its panel unseen (see
 * `repoHosts.ts`), and the reader stays on the task, which lists the session
 * with a "Show terminal" for when they want it.
 *
 * The request is queued before the checkout is opened (see `taskLaunches.ts`
 * for why), so an open that is superseded by later navigation, or refused,
 * does not lose the terminal: it waits, and starts when that checkout opens.
 */

import { get } from "svelte/store";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { toastStore, type ToastAction } from "../stores/toastStore";
import {
  attemptNotices,
  consumeTaskTerminal,
  enqueueTaskTerminal,
  hasTaskTerminalRequest,
  noteAttempt,
  type AttemptNotice,
  type TaskTerminalRequest,
} from "../terminal/taskLaunches";
import { handOverDetachedRun, isAdoptedSession } from "../terminal/detachedSessions";
import { focusTerminalSession } from "../terminal/sessionFocus";
import { terminalSessions, type TerminalSessionRecord } from "../terminal/sessionRegistry";
import { explainError, findConversation, launchManagedRun, type TaskRun } from "./client";
import { resolveGitRoot } from "../desktop/nativeShell";
import { identityKey, isCaseInsensitiveFs, normalizeRepoPath } from "../repos/paths";
import { relativeStartDir } from "../terminal/tabs";
import { formatError } from "../ui/formatError";
import { PROVIDER_LABELS } from "./vocabulary";
import { defaultHandoff, type HandoffSettings } from "./taskHandoff";
import { bounded } from "./taskActions";

/**
 * `opened`: on screen now. `started`: its checkout is open and the terminal
 * starts out of sight. `queued`: the checkout has not opened yet, so the
 * terminal waits until it does. The short form, for callers that only need
 * to know whether to say "waiting"; `AttemptStart` carries the why.
 */
export type TaskTerminalOutcome = "opened" | "started" | "queued";

/**
 * What became of a request to start or show an attempt's terminal.
 *
 * `waiting` is a request that stands and will be served without the reader:
 * its checkout is still opening, or opened as a repository nobody has trusted
 * yet. `failed` is one that will not be — the checkout could not be opened,
 * so nothing is queued and the reason is given. "Queued" used to cover all
 * of these, including a refusal that no amount of waiting would undo.
 */
export type AttemptStart =
  | { kind: "opened" }
  | { kind: "started" }
  | { kind: "waiting"; reason: "checkout" | "trust"; checkout: string }
  | { kind: "failed"; checkout: string; reason: string };

type RunRef = Pick<TaskRun, "id" | "cwd" | "provider" | "task_title">;

/** The last segment of a checkout path, for a sentence. */
export function checkoutLabel(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

/**
 * Starts an attempt's terminal without changing what is on screen.
 */
export async function startTaskTerminal(run: RunRef): Promise<AttemptStart> {
  const place = await checkoutFor(run.cwd);
  const refused = openRefusal(place.root);
  if (refused) return { kind: "failed", checkout: place.root, reason: refused };
  queueAttempt(run, place);
  const opened = await repoStore.openRepo(place.root, backgroundOpen());
  return afterOpen(run, place.root, opened, "started");
}

/**
 * What an open's answer means for a request that was queued before it.
 *
 * A refusal the store gives no reason for still leaves the request standing:
 * an open superseded by later navigation is answered `false` too, and that
 * checkout may open in a moment. A refusal for capacity will not resolve by
 * waiting, so the request is withdrawn and the reason is the answer.
 */
function afterOpen(run: Pick<TaskRun, "id">, root: string, opened: boolean, success: "opened" | "started"): AttemptStart {
  const refused = openRefusal(root);
  if (refused) {
    consumeTaskTerminal(run.id);
    return { kind: "failed", checkout: root, reason: refused };
  }
  if (trustPending(root)) return { kind: "waiting", reason: "trust", checkout: root };
  return opened ? { kind: success } : { kind: "waiting", reason: "checkout", checkout: root };
}

/** Open tabs as the store publishes them; a partial test double may omit them. */
function openTabs(): readonly { id: string; path: string; trustRequired?: boolean }[] {
  return get(repoStore).openTabs ?? [];
}

function tabFor(root: string): { id: string; path: string; trustRequired?: boolean } | undefined {
  const options = { caseInsensitive: isCaseInsensitiveFs() };
  const key = identityKey(root, options);
  return key ? openTabs().find((tab) => identityKey(tab.path, options) === key) : undefined;
}

/**
 * Why the repository store will refuse to open `root`, when that is knowable
 * before asking — the store's own answer (`repoStore.openRefusal`), so the
 * bound and its words have one owner.
 */
function openRefusal(root: string): string | null {
  const refused = repoStore.openRefusal(root);
  return refused ? `${refused} Show terminal starts it once one is free.` : null;
}

/** The checkout opened as a repository GitPulse has not been told to trust. */
function trustPending(root: string): boolean {
  return tabFor(root)?.trustRequired === true;
}

/**
 * Asks the reader to trust the checkout an attempt is waiting for — the
 * repository's own trust prompt, without switching the repository behind the
 * task sheet. Resolves to whether a tab was there to ask about.
 */
export async function trustAttemptCheckout(run: Pick<TaskRun, "cwd">): Promise<boolean> {
  const place = await checkoutFor(run.cwd);
  const tab = tabFor(place.root);
  if (!tab) return false;
  await repoStore.trustTab(tab.id);
  return true;
}

/** The checkout a directory belongs to, and where below its root the directory is. */
interface Placement {
  root: string;
  startDir?: string;
}

/**
 * Finds the checkout that holds `cwd`, through the same native walk a dropped
 * folder uses (`find_git_root`).
 *
 * An attempt's directory need not be a checkout root, and the repository
 * store opens only roots: opening a subdirectory was refused every time, so
 * the launch reported "queued" and waited for a tab that could never open.
 * When no checkout holds the directory at all, that is said now, and nothing
 * is queued — a request nothing can ever take is not "waiting".
 *
 * The host returns the canonical root. A recorded directory that names it
 * through a link (`/tmp` for `/private/tmp`) is the root itself: the host only
 * ever spawned a session in a canonical checkout root, so that is where the
 * work ran, and no start directory is carried.
 */
async function checkoutFor(cwd: string): Promise<Placement> {
  let root: string;
  try {
    root = await resolveGitRoot(cwd);
  } catch (cause) {
    throw new Error(`No Git checkout contains ${cwd}, so its terminal cannot start. ${formatError(cause)}`);
  }
  const options = { caseInsensitive: isCaseInsensitiveFs() };
  const rootKey = identityKey(root, options);
  const cwdKey = identityKey(cwd, options);
  if (!rootKey) throw new Error(`No Git checkout contains ${cwd}, so its terminal cannot start.`);
  if (cwdKey === rootKey) return { root };
  const normalizedRoot = normalizeRepoPath(root) ?? root;
  const normalizedCwd = normalizeRepoPath(cwd) ?? cwd;
  const startDir = cwdKey.startsWith(`${rootKey}/`) ? relativeStartDir(normalizedCwd.slice(normalizedRoot.length + 1)) : null;
  return startDir ? { root, startDir } : { root };
}

/**
 * How to open a checkout without showing it.
 *
 * As a background tab — except when no repository is active. The dock lives
 * in the repository view, and App renders that view only while some
 * repository is current, so with none a background tab waits for a dock that
 * is never mounted and the agent never starts. Then the checkout becomes the
 * active repository instead: on the repository surface, which the reader is
 * not looking at, so they still stay on the task.
 */
function backgroundOpen(): { activate: boolean; deferTrust: true } {
  // Never a modal. An open nobody is looking at must not raise a trust
  // prompt over whatever the reader is doing; the tab records that trust is
  // needed, the dock will not host it until it is (`awaitedTabIds`), and the
  // Agents pane says so with a button that asks.
  return { activate: get(repoStore).currentPath === null, deferTrust: true };
}

/**
 * Brings an attempt's terminal on screen — the one thing here that moves the
 * reader, and only ever on their request.
 *
 * A session already running is focused through `focusTerminalSession`, the
 * owner of the repository → dock → tab order. Anything else (not started
 * yet, or a session a reload left behind) is queued and its checkout opened
 * in front, where the panel finds the request and focuses or starts it.
 */
export async function showAttemptTerminal(run: RunRef): Promise<AttemptStart> {
  const live = liveAttemptSession(run.id);
  if (live) {
    const focused = await focusTerminalSession(live);
    if (focused.ok) return { kind: "opened" };
  }
  const place = await checkoutFor(run.cwd);
  const refused = openRefusal(place.root);
  if (refused) return { kind: "failed", checkout: place.root, reason: refused };
  queueAttempt(run, place);
  const shown = await showCheckout(place.root);
  return afterOpen(run, place.root, shown === "opened", "opened");
}

/**
 * `showAttemptTerminal` in the short form. A checkout that cannot be opened
 * is an error here, with its reason, rather than "queued": nothing is.
 */
export async function showTaskTerminal(run: RunRef): Promise<TaskTerminalOutcome> {
  const outcome = await showAttemptTerminal(run);
  if (outcome.kind === "failed") throw new Error(`Could not open ${checkoutLabel(outcome.checkout)}: ${outcome.reason}`);
  return outcome.kind === "waiting" ? "queued" : outcome.kind;
}

/** What a row says about an outcome that is not "on screen" or "starting". */
export function attemptStartNote(outcome: AttemptStart): string | null {
  switch (outcome.kind) {
    case "failed": return `Could not open ${checkoutLabel(outcome.checkout)}: ${outcome.reason}`;
    case "waiting": return outcome.reason === "trust"
      ? `Waiting for you to trust ${checkoutLabel(outcome.checkout)}. It opened without asking; trust it and the agent starts.`
      : `Waiting for ${checkoutLabel(outcome.checkout)} to open.`;
    default: return null;
  }
}

/**
 * Opens the checkout an attempt runs in as the active repository tab and
 * brings the repository surface forward — the reader asked to go there. It
 * starts nothing: the attempt's terminal is `showTaskTerminal`'s business.
 * Resolves to whether the checkout opened; throws when no checkout holds the
 * attempt's directory.
 */
export async function openAttemptCheckout(run: Pick<TaskRun, "cwd">): Promise<boolean> {
  const place = await checkoutFor(run.cwd);
  let ready = false;
  const opened = await repoStore.openRepo(place.root, {
    activate: true,
    onReady: () => {
      ready = true;
      interfaceStore.setGlobalSurface("repository");
    },
  });
  return opened && ready;
}

/** Which way a resumed conversation should arrive. */
export type ResumeDisposition = "background" | "show";

/**
 * Picks an ended attempt's Claude Code conversation back up in a new tab in
 * its checkout, under the attempt's own permission mode.
 *
 * The host decides whether there is a conversation (from the transcript
 * Claude Code saved), so an attempt with nothing to resume is answered with
 * the reason rather than a tab that opens only to say "No conversation found".
 */
export async function resumeTaskConversation(
  run: Pick<TaskRun, "id" | "task_title">,
  disposition: ResumeDisposition = "background",
): Promise<{ outcome: TaskTerminalOutcome } | { outcome: "unavailable"; reason: string }> {
  const conversation = await findConversation(run.id);
  if (!conversation.resumable) return { outcome: "unavailable", reason: conversation.reason };
  if (disposition === "show") {
    const live = get(terminalSessions).find((record) => record.continuesRunId === run.id && record.reveal);
    if (live && (await focusTerminalSession(live)).ok) return { outcome: "opened" };
  }
  // Claude Code keeps the conversation under the directory it ran in, which
  // `startDir` carries when that is below the root.
  const place = await checkoutFor(conversation.cwd);
  const refused = openRefusal(place.root);
  if (refused) return { outcome: "unavailable", reason: `Could not open ${checkoutLabel(place.root)}: ${refused}` };
  const request: TaskTerminalRequest = {
    runId: run.id,
    repoPath: place.root,
    provider: "claude",
    title: run.task_title,
    resume: { sessionId: conversation.sessionId, mode: conversation.mode, runId: run.id },
    ...(place.startDir ? { startDir: place.startDir } : {}),
  };
  enqueueTaskTerminal(request);
  if (disposition === "show") return { outcome: await showCheckout(place.root) };
  return { outcome: (await repoStore.openRepo(place.root, backgroundOpen())) ? "started" : "queued" };
}

/**
 * The renderer session running this attempt's own process, if one is.
 * An adopted record from before a reload is not one: its "reveal" hands the
 * process to a new tab, which is `showTaskTerminal`'s queued path.
 */
function liveAttemptSession(runId: string): TerminalSessionRecord | undefined {
  return get(terminalSessions).find(
    (record) => record.taskRunId === runId && !isAdoptedSession(record) && record.reveal,
  );
}

function queueAttempt(run: RunRef, place: Placement): void {
  enqueueTaskTerminal({
    runId: run.id, repoPath: place.root, provider: run.provider, title: run.task_title,
    ...(place.startDir ? { startDir: place.startDir } : {}),
  });
  // A session a reloaded page left running for this attempt gives up its
  // adopted record now, before the tab that takes the same process over
  // reserves a slot. Queued first, so the request that stands is this one.
  handOverDetachedRun(run.id);
}

async function showCheckout(cwd: string): Promise<TaskTerminalOutcome> {
  let ready = false;
  const opened = await repoStore.openRepo(cwd, {
    onReady: () => {
      ready = true;
      interfaceStore.setGlobalSurface("repository");
      repoStore.setTerminalOpen(true);
    },
  });
  return opened && ready ? "opened" : "queued";
}

/** What to tell the reader when the terminal is waiting rather than started. */
export function queuedTerminalNote(cwd: string): string {
  return `Waiting for ${checkoutLabel(cwd)} to open. The agent's terminal starts as soon as it does; Show terminal opens it now, and says why if it cannot.`;
}

// ---- After the store accepts a preparation --------------------------------

/**
 * How long past a preparation's expiry its start is still watched. The host
 * refuses a claim on an expired preparation, so after this a request that
 * never reached a tab can only be withdrawn.
 */
const EXPIRY_GRACE_MS = 5_000;
/** Bound on any one watch, whatever the run says its expiry is. */
const MAX_WATCH_MS = 15 * 60_000;

export interface PreparedAttemptDeps {
  startTerminal: (run: TaskRun) => Promise<AttemptStart>;
  launchManaged: (runId: string) => Promise<TaskRun>;
  remember: (settings: HandoffSettings) => void;
  show: (run: TaskRun) => Promise<unknown>;
  toast: {
    info: (message: string, action?: ToastAction, duration?: number) => string;
    success: (message: string, action?: ToastAction, duration?: number) => string;
    error: (message: string, action?: ToastAction, duration?: number) => string;
    dismiss: (id: string) => void;
  };
  now: () => number;
  setTimer: (run: () => void, ms: number) => ReturnType<typeof setTimeout>;
  clearTimer: (handle: ReturnType<typeof setTimeout>) => void;
}

const DEPS: PreparedAttemptDeps = {
  startTerminal: startTaskTerminal,
  launchManaged: (runId) => bounded(launchManagedRun(runId)),
  remember: (settings) => interfaceStore.setTaskHandoff(settings),
  show: showTaskTerminal,
  toast: toastStore,
  now: () => Date.now(),
  setTimer: (run, ms) => setTimeout(run, ms),
  clearTimer: (handle) => clearTimeout(handle),
};

export type PreparedAttemptResult = { ok: true; run: TaskRun } | { ok: false; run: TaskRun; error: string };

/** Watches still waiting for an attempt's process to start, by run. */
const watches = new Map<string, (withdrawn: boolean) => void>();

/**
 * Stops following a run's start because the reader withdrew it (cancelled
 * the preparation): nothing more is said about it, including by an open
 * that answers after the cancel.
 */
export function stopWatchingAttempt(runId: string): void {
  watches.get(runId)?.(true);
}

/** Run ids whose start is still being followed. For tests and diagnostics. */
export function watchedAttempts(): string[] {
  return [...watches.keys()];
}

/**
 * Everything that happens to an attempt after the store accepts it: the
 * settings remembered, the managed session or the terminal started, and the
 * reader told what became of it.
 *
 * It lives here, not in the form that pressed Launch, because none of it may
 * depend on that form still existing. The form is gone the moment its sheet
 * closes or its task tab is switched (`TaskBoard`'s `{#key session.pane}`),
 * and it used to return early when it was — so an accepted attempt was never
 * started, nobody was told, and its preparation expired holding a worktree.
 *
 * Every outcome is recorded against the run (`noteAttempt`), where the
 * task's Agents pane shows it on the attempt's row, and toasted. For a
 * terminal, "started" means only that its checkout opened, so the toast says
 * "Starting…" and the real start is confirmed when the attempt's tab reports
 * its process running (or the reason it did not).
 */
export async function startPreparedAttempt(
  run: TaskRun,
  options: { remember?: HandoffSettings } = {},
  deps: PreparedAttemptDeps = DEPS,
): Promise<PreparedAttemptResult> {
  if (options.remember) {
    // Remembered only after a preparation the store accepted, and never with
    // `bypass`: a mode that turns off the agent's sandbox is chosen again,
    // with its acknowledgement, every time.
    const settings = options.remember;
    deps.remember(settings.permission === "bypass" ? { ...settings, permission: defaultHandoff().permission } : settings);
  }
  const label = PROVIDER_LABELS[run.provider] ?? run.provider;
  if (run.kind === "managed") return startManaged(run, label, deps);
  return startTerminal(run, label, deps);
}

async function startManaged(run: TaskRun, label: string, deps: PreparedAttemptDeps): Promise<PreparedAttemptResult> {
  noteAttempt(run.id, "starting", `Starting managed ${label}…`, deps.now());
  try {
    const started = await deps.launchManaged(run.id);
    if (started.state === "running") {
      noteAttempt(run.id, "running", `Managed ${label} is running.`, deps.now());
      deps.toast.success(`Managed ${label} started.`);
    } else {
      noteAttempt(run.id, "starting", `Managed ${label}: ${started.state}.`, deps.now());
      deps.toast.info(`Managed ${label} ${started.state}.`);
    }
    return { ok: true, run: started };
  } catch (cause) {
    const error = `Managed ${label} did not start: ${explainError(cause)} Start or recover managed launch retries this same attempt.`;
    noteAttempt(run.id, "failed", error, deps.now());
    deps.toast.error(error);
    return { ok: false, run, error };
  }
}

async function startTerminal(run: TaskRun, label: string, deps: PreparedAttemptDeps): Promise<PreparedAttemptResult> {
  const showAction: ToastAction = {
    label: "Show terminal",
    onClick: () => deps.show(run).then(() => undefined, (cause: unknown) => { deps.toast.error(explainError(cause)); }),
  };
  // Recorded and watched before anything is asked of the host: the tab can
  // report its process running before the open below even resolves.
  noteAttempt(run.id, "starting", `Starting ${label}…`, deps.now());
  const watching = watch(run, label, showAction, deps);
  let outcome: AttemptStart;
  try {
    outcome = await deps.startTerminal(run);
  } catch (cause) {
    const error = explainError(cause);
    if (watching.withdrawn()) return { ok: false, run, error: "Cancelled." };
    consumeTaskTerminal(run.id);
    noteIfStarting(run.id, "failed", error, deps);
    return { ok: false, run, error };
  }
  // Cancelled while the open was answered: the reader withdrew it, and
  // whatever the open says now is about an attempt that no longer exists.
  if (watching.withdrawn()) {
    consumeTaskTerminal(run.id);
    return { ok: false, run, error: "Cancelled." };
  }
  const note = attemptStartNote(outcome);
  if (outcome.kind === "failed" && note) {
    noteIfStarting(run.id, "failed", note, deps);
    return { ok: false, run, error: note };
  }
  if (outcome.kind === "waiting" && note) noteIfStarting(run.id, "waiting", note, deps);
  return { ok: true, run };
}

/**
 * Records an outcome unless the attempt's tab has already reported a later
 * one: a process that started (or failed) while the open was still being
 * answered is the newer fact, and "waiting for the checkout" must not
 * overwrite it.
 */
function noteIfStarting(runId: string, phase: AttemptNotice["phase"], text: string, deps: PreparedAttemptDeps): void {
  const current = get(attemptNotices).get(runId);
  if (current && current.phase !== "starting" && current.phase !== "waiting") return;
  noteAttempt(runId, phase, text, deps.now());
}

/**
 * Follows one attempt's start until it is decided, and tells the reader.
 *
 * Decided means the tab reported its process running, or a failure was
 * recorded (by the tab, or by the open above). Bounded twice. At the
 * preparation's expiry a request still queued can never be served, so it is
 * withdrawn and recorded as the failure it is; a request a tab already took
 * is a spawn in flight, and is followed on until `MAX_WATCH_MS` — the host
 * answers a claim long before that, and the tab reports either way.
 */
function watch(run: TaskRun, label: string, showAction: ToastAction, deps: PreparedAttemptDeps): { withdrawn: () => boolean } {
  watches.get(run.id)?.(false);
  let done = false;
  let withdrawn = false;
  let unsubscribe: (() => void) | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let toastId: string | null = deps.toast.info(`Starting ${label} on revision ${run.source_revision}…`, showAction);
  const began = deps.now();
  const finish = (withdraw: boolean) => {
    if (done) return;
    done = true;
    withdrawn = withdraw;
    if (timer !== null) deps.clearTimer(timer);
    timer = null;
    unsubscribe?.();
    if (toastId) deps.toast.dismiss(toastId);
    toastId = null;
    if (watches.get(run.id) === finish) watches.delete(run.id);
  };
  watches.set(run.id, finish);
  const arm = (ms: number) => {
    timer = deps.setTimer(() => {
      timer = null;
      if (done) return;
      if (hasTaskTerminalRequest(run.id)) {
        consumeTaskTerminal(run.id);
        noteAttempt(run.id, "failed", "This attempt expired before its terminal could start, so it no longer holds its checkout. Prepare a new attempt.", deps.now());
        return;
      }
      const left = began + MAX_WATCH_MS - deps.now();
      if (left > 0) arm(left);
      else finish(false);
    }, ms);
  };
  const remaining = run.state === "prepared" ? run.expires_at * 1000 - deps.now() : MAX_WATCH_MS;
  arm(Math.min(MAX_WATCH_MS, Math.max(0, remaining) + EXPIRY_GRACE_MS));
  unsubscribe = attemptNotices.subscribe((map) => {
    if (done) return;
    const notice = map.get(run.id);
    if (notice?.phase === "running") {
      finish(false);
      deps.toast.success(`${label} started on revision ${run.source_revision}.`, showAction);
    } else if (notice?.phase === "failed") {
      finish(false);
      // Show terminal retries a start — except on a preparation that has
      // expired, which no start can claim.
      const retryable = run.state !== "prepared" || run.expires_at * 1000 > deps.now();
      deps.toast.error(`${label} did not start: ${notice.text}`, retryable ? showAction : undefined);
    }
  });
  if (done) unsubscribe();
  return { withdrawn: () => withdrawn };
}
