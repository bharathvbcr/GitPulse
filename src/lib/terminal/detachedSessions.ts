/**
 * Sessions left running with no tab: a reloaded page's, or a tab that went
 * away without being closed (its repository tab closed, its pane failed).
 *
 * A webview reload replaces every renderer object — tabs, the session
 * registry, the event bus — while the processes those tabs started keep
 * running in the native host. Nothing here used to know they existed: the
 * Sessions list showed none of them, the 32-session limit counted from zero
 * while the host still held them (so new tabs were refused with a limit the
 * reader could not see), and an agent left working in one could neither be
 * watched nor stopped. The host now detaches them on reload, so they no
 * longer wait on a page that is gone; this adopts them into the new page.
 *
 * Each detached session becomes a Sessions record that holds its slot, says
 * it is still running, can be stopped, and can be shown again: "Go to"
 * switches to its repository and the dock opens a tab that takes it over
 * (`cmd_terminal_attach`; a task attempt's goes through its own launch path,
 * which takes over its tracked session). The tab has none of the output from
 * before the reload — the host keeps no copy — so a full-screen program is
 * made to repaint and a shell shows its next prompt.
 */

import { get } from "svelte/store";
import { invoke as tauriInvoke } from "../ipc/invoke";
import { ptyBus as tauriBus } from "./ptyBus.tauri";
import type { PtyBus } from "./ptyBus";
import { FAMILY_LOOKUP_MS, terminalDeadline } from "./inputQueue";
import type { TerminalListing } from "./runResult";
import { hostedClaude, settleConversation, type createHostedClaudeStore, type ExitFacts } from "./hostedClaude";
import { terminalSessions, type createSessionRegistry, type TerminalSessionRecord } from "./sessionRegistry";
import { enqueueTaskTerminal } from "./taskLaunches";
import { LAUNCHERS, launcherLabel, type LauncherKind } from "./tabs";
import { askConfirm } from "../stores/modalStore";

export const DETACHED_STATUS = "Still running — not shown in any tab";
/** A session whose terminal closed but whose process has not exited: stoppable, never shown. */
export const ENDING_STATUS = "Ending — its process has not exited";

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
type Confirm = (options: { title: string; message: string; confirmLabel: string; destructive: boolean }) => Promise<boolean>;

const KEY_PREFIX = "detached:";

/**
 * Whether a record is an adopted session rather than a tab's own. Its
 * `reveal` hands the process over to a new tab instead of showing one, so a
 * caller that wants "the tab running this" must not take it for one.
 */
export function isAdoptedSession(record: Pick<TerminalSessionRecord, "key">): boolean {
  return record.key.startsWith(KEY_PREFIX);
}

/**
 * The question before stopping an adopted session. Always asked: it is
 * running by definition, and nothing on this page has seen what it is doing.
 */
export function stopQuestion(listing: Pick<TerminalListing, "run_id">, label: string): { title: string; message: string } {
  return {
    title: `Stop ${label}?`,
    message: listing.run_id
      ? `${label} is still working on a task, not shown in any tab. Stopping it ends that attempt; it cannot be restarted, only resumed as a new conversation.`
      : `${label} is still running, not shown in any tab. Stopping it ends whatever it is doing.`,
  };
}

/**
 * Hands an adopted task session to its task's own launch path, freeing the
 * adopted record's slot first. Called by "Open terminal": with every slot
 * held, the tab that takes the session over could otherwise never open.
 * Returns whether there was one.
 */
export function handOverDetachedRun(
  runId: string,
  registry: ReturnType<typeof createSessionRegistry> = terminalSessions,
): boolean {
  const adopted = get(registry).find((record) => isAdoptedSession(record) && record.taskRunId === runId);
  adopted?.reveal?.();
  return Boolean(adopted);
}

/** Validates the payload at the boundary. A malformed row is no row. */
export function parseListing(raw: unknown): TerminalListing | null {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) return null;
  const row = raw as Record<string, unknown>;
  const text = (value: unknown, max = 4096) => typeof value === "string" && value.length > 0 && value.length <= max && !value.includes("\0");
  const optional = (value: unknown) => value === null || text(value, 128);
  if (!text(row.id, 128) || !text(row.shell) || !text(row.cwd) || !text(row.repo)) return null;
  if (!optional(row.launcher) || !optional(row.run_id) || typeof row.detached !== "boolean") return null;
  return {
    id: row.id as string, shell: row.shell as string, cwd: row.cwd as string, repo: row.repo as string,
    launcher: (row.launcher as string | null) ?? null, run_id: (row.run_id as string | null) ?? null,
    detached: row.detached,
    ...(row.ending === true ? { ending: true } : {}),
  };
}

type Slot = ReturnType<ReturnType<typeof createSessionRegistry>["reserve"]>;

/**
 * Lists one detached session in Sessions: a record that holds its slot, says
 * it is still running, can be stopped, and can be shown again, and that goes
 * when the process exits. `take` admits the record — a fresh reservation for
 * a session found after a reload, or a tab's own slot handed over
 * (`keepDetached`). The exit watch is subscribed before `take` returns, so
 * an exit cannot fall between the old owner and this one.
 */
/** Where hosted Claude conversations are recorded (`hostedClaude`); a seam for tests. */
type Conversations = Pick<ReturnType<typeof createHostedClaudeStore>, "byTerminal" | "ended" | "forget">;

function listDetached(
  listing: TerminalListing,
  family: string | null,
  take: (record: TerminalSessionRecord) => Slot,
  deps: { invoke: Invoke; confirm: Confirm; bus: PtyBus; conversations?: Conversations },
): void {
  const { invoke, confirm, bus } = deps;
  const conversations = deps.conversations ?? hostedClaude;
  /**
   * The end of this process settles its Claude conversation's record, as the
   * tab that ran it would have (`settleConversation`): a Stop or a clean end
   * forgets it, a signal from outside keeps it for an incident's "Resume N".
   * Showing it again does not end anything and settles nothing.
   */
  function settle(end: ExitFacts | "closed") {
    const record = conversations.byTerminal(listing.id);
    if (record) settleConversation(conversations, record.sessionId, end);
  }
  const launcher = launcherOf(listing);
  const key = `${KEY_PREFIX}${listing.id}`;
  const label = launcherLabel(launcher);
  let unsubscribe: (() => void) | null = null;
  let slot: Slot | null = take({
    key,
    repoPath: listing.repo,
    label,
    launcher,
    ...(listing.run_id ? { title: "Task attempt", taskRunId: listing.run_id } : {}),
    ...(family ? { family } : {}),
    status: listing.ending ? ENDING_STATUS : DETACHED_STATUS,
    sessionId: listing.id,
    async close() {
      await invoke("cmd_terminal_kill", { sessionId: listing.id });
      settle("closed");
      drop();
    },
    confirmClose: () => confirm({ ...stopQuestion(listing, label), confirmLabel: "Stop", destructive: true }),
    // An ending session has no terminal left to show; Stop is all it offers.
    ...(listing.ending ? {} : { reveal }),
  });
  function reveal() {
    // Queued first, so a refusal leaves the session listed. Then this record
    // and its exit watch give way, before the tab that takes the session over
    // reserves its own slot: never listed twice, never counted twice against
    // the limit.
    enqueueTaskTerminal({
      runId: listing.run_id ?? key,
      repoPath: listing.repo,
      provider: launcher,
      title: listing.run_id ? "Task attempt" : label,
      ...(listing.run_id ? {} : { attach: { sessionId: listing.id } }),
      ...(family ? { family } : {}),
    });
    drop();
  }
  function drop() {
    unsubscribe?.(); unsubscribe = null;
    slot?.release(); slot = null;
  }
  slot.identify(listing.id);
  unsubscribe = bus.subscribe(listing.id, {
    onOutput() {},
    onExit(event) {
      settle(event);
      drop();
    },
  });
}

/**
 * A tab's view went away while its process runs on (`sessionLifecycle`
 * `dispose`). Its slot becomes the Sessions record for that process in one
 * step, so the limit never sees the slot free and the exit is never
 * unwatched. The host has already detached the session.
 */
export function keepDetached(
  listing: TerminalListing,
  slot: Slot,
  deps: { invoke?: Invoke; confirm?: Confirm; bus?: PtyBus; family?: string | null; conversations?: Conversations } = {},
): void {
  listDetached(listing, deps.family ?? null, (record) => slot.handOver(record), {
    invoke: deps.invoke ?? (tauriInvoke as Invoke),
    confirm: deps.confirm ?? askConfirm,
    bus: deps.bus ?? tauriBus,
    ...(deps.conversations ? { conversations: deps.conversations } : {}),
  });
}

/**
 * The repository family a task attempt's session is shown under, or null.
 * Advisory: null means "stands alone", the same answer an unreadable family
 * already gives. It is bounded on its own because the lookup is the git pool's
 * (`cmd_resolve_repo`), which a burst of git commands holds for minutes; left
 * inside the one deadline around `keep`, a slow family lookup took the whole
 * hand-over with it and the session was never listed in Sessions.
 */
export async function listingFamily(
  listing: Pick<TerminalListing, "run_id" | "repo">,
  familyOf: (repo: string) => Promise<string | null>,
): Promise<string | null> {
  if (!listing.run_id) return null;
  try {
    return await terminalDeadline(familyOf(listing.repo), FAMILY_LOOKUP_MS, "Resolving repository family");
  } catch {
    return null;
  }
}

function launcherOf(listing: TerminalListing): LauncherKind {
  return LAUNCHERS.find((launcher) => launcher.kind === listing.launcher)?.kind ?? "shell";
}

/**
 * Lists the host's sessions and adopts each detached one into `registry`.
 * Returns how many were adopted; a listing that fails adopts none and says
 * so by throwing, because "none" and "could not look" are not the same.
 */
export async function adoptDetachedSessions(deps: {
  invoke?: Invoke;
  confirm?: Confirm;
  bus?: PtyBus;
  registry?: ReturnType<typeof createSessionRegistry>;
  /**
   * A checkout's repository family (`repoStore.familyOf`), so an attempt's
   * session in a worktree with no tab is shown in an open checkout of the
   * same repository (`taskLaunches.hostTabFor`). Absent: none is looked up.
   */
  familyOf?: (path: string) => Promise<string | null>;
} = {}): Promise<number> {
  const invoke = deps.invoke ?? (tauriInvoke as Invoke);
  const bus = deps.bus ?? tauriBus;
  const confirm = deps.confirm ?? askConfirm;
  const registry = deps.registry ?? terminalSessions;
  const raw = await invoke<unknown>("cmd_terminal_sessions");
  if (!Array.isArray(raw)) throw new Error("The terminal host returned an invalid session list.");
  let adopted = 0;
  for (const listing of raw.map(parseListing)) {
    if (!listing?.detached) continue;
    // Adopting twice must not list a session twice.
    if (get(registry).some((record) => record.sessionId === listing.id)) continue;
    // An attempt's terminal is re-opened as a task tab, which any checkout of
    // its repository can host; a plain session is taken over in its own.
    const family = listing.run_id && deps.familyOf ? await deps.familyOf(listing.repo).catch(() => null) : null;
    if (get(registry).some((record) => record.sessionId === listing.id)) continue;
    // Adopted past the limit if need be: the host holds the process either
    // way, and a refused reserve aborted the loop, leaving every session after
    // the first over-limit one running and unlisted.
    listDetached(listing, family, (record) => registry.adopt(record), { invoke, confirm, bus });
    adopted += 1;
  }
  return adopted;
}
