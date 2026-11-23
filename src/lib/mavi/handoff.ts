/**
 * Moving a Mavi conversation to the terminal dock and back.
 *
 * **One process per conversation.** Mavi (`--resume` in stream-json) and a
 * terminal tab (`--resume` in a PTY) would both append to one Claude Code
 * transcript, and nothing in the CLI stops that. So a move ends the source
 * side and waits until it has exited before the other side starts, and
 * either side refuses to start while the other still holds the conversation.
 *
 * **Nothing is stopped until everything checkable has passed.** A move is
 * offered only between turns (no turn running, nothing queued), so there is
 * no interrupt to race and no queued message to lose. Before the Mavi agent is
 * ended, the move checks: the thread runs on this computer, the agent saved a
 * transcript for the conversation, no terminal tab already has it, the dock
 * has room for another session, and the repository opens as a tab. A failure
 * after that point says the conversation is intact and how to continue it.
 */
import { get } from "svelte/store";
import { checkTranscripts } from "../terminal/transcripts";
import { terminalSessions, runningAgents, type TerminalSessionRecord } from "../terminal/sessionRegistry";
import { hostedClaude } from "../terminal/hostedAgent";
import { terminalLaunchRequests } from "../terminal/launchRequests";
import { terminalSessionLimit } from "../terminal/sessionLimit";
import { focusTerminalSession } from "../terminal/sessionFocus";
import { isPermissionMode, type PermissionMode } from "../terminal/agentDefaults";
import { repoStore } from "../stores/repoStore";
import { maviStore } from "./store.svelte";
import { maviClient, type MaviModeId } from "./client";
import type { ThreadRecord } from "./threads";

/** The host's own bound on live threads (`MAX_LIVE` in src-tauri/src/mavi/mod.rs). */
export const MAX_LIVE_THREADS = 12;

export type HandoffResult = { ok: true; note?: string } | { ok: false; reason: string };

/** The terminal tab running `conversationId`, if one is live. */
export function terminalHolding(
  conversationId: string,
  records: readonly TerminalSessionRecord[] = get(terminalSessions),
  byTerminal: (ptyId: string) => { sessionId: string } | undefined = (id) => hostedClaude.byTerminal(id),
): TerminalSessionRecord | null {
  const id = conversationId.toLowerCase();
  for (const record of runningAgents(records)) {
    if (record.launcher !== "claude" || !record.sessionId) continue;
    // An ended tab keeps its record (and session id) until it is closed.
    if (record.status === "exited" || record.status === "error") continue;
    if (byTerminal(record.sessionId)?.sessionId?.toLowerCase() === id) return record;
  }
  return null;
}

/** Whether "Continue in terminal" can be offered now, and why not when it cannot. */
export function canContinueInTerminal(record: ThreadRecord): HandoffResult {
  if (record.target.kind !== "local") return { ok: false, reason: "A thread on an SSH host cannot move to this computer's terminal." };
  if (!record.conversationId) return { ok: false, reason: "The agent has not saved a conversation yet." };
  if (record.movedToTerminalAt) return { ok: false, reason: "This conversation is already in a terminal tab." };
  const summary = maviStore.summaryOf(record);
  if (summary && (summary.phase === "working" || summary.phase === "approval")) {
    return { ok: false, reason: "Wait for the turn to end (or stop it) before moving the conversation." };
  }
  if ((maviStore.queues[record.key] ?? []).length > 0) {
    return { ok: false, reason: "Messages are queued for this thread; send or remove them first." };
  }
  return { ok: true };
}

/** Opens (or activates) the repository tab for `root`; its canonical path, or null. */
async function openTab(root: string): Promise<string | null> {
  let canonical: string | null = null;
  const opened = await repoStore.openRepo(root, {
    activate: true,
    onReady: (path) => {
      canonical = path;
    },
  });
  if (!opened) return null;
  return canonical ?? get(repoStore).currentPath ?? root;
}

/**
 * Moves a local thread's conversation to a Claude Code tab in the dock,
 * resuming it with the thread's permission mode. Bypass is confirmed by the
 * caller before this runs, and the tab asks for its own acknowledgement too.
 */
export async function continueInTerminal(key: string): Promise<HandoffResult> {
  const record = maviStore.record(key);
  if (!record) return { ok: false, reason: "That thread is gone." };
  const allowed = canContinueInTerminal(record);
  if (!allowed.ok) return allowed;
  const conversationId = record.conversationId!;
  const root = record.target.kind === "local" ? record.target.root : "";

  // Preflight: nothing below stops anything.
  const transcript = (await checkTranscripts([conversationId])).get(conversationId.toLowerCase());
  if (transcript?.state === "absent") {
    return { ok: false, reason: "Claude Code saved no conversation for this thread yet, so there is nothing to move." };
  }
  if (transcript?.state !== "found") {
    return { ok: false, reason: `Couldn't confirm the conversation was saved: ${transcript?.reason ?? "no answer"}. Nothing was moved.` };
  }
  const holder = terminalHolding(conversationId);
  if (holder) {
    // Both sides must never run it: a Mavi agent still on it ends first.
    const live = maviStore.summaryOf(record);
    if (live && live.phase !== "ended") {
      await maviStore.stop(key);
      if (!(await maviStore.waitForEnd(key))) {
        return { ok: false, reason: "The Mavi agent did not stop in time. Nothing was moved." };
      }
    }
    maviStore.markMoved(key, true);
    await focusTerminalSession(holder);
    return { ok: true, note: "This conversation was already open in a terminal tab; showing it." };
  }
  if (get(terminalSessions).length >= get(terminalSessionLimit)) {
    return { ok: false, reason: "The terminal dock is at its session limit. Close a session first; nothing was moved." };
  }
  const tabPath = await openTab(root);
  if (!tabPath) return { ok: false, reason: "Couldn't open this repository as a tab. Nothing was moved." };

  // From here the Mavi side ends first, then the terminal starts. The thread
  // reads as moved for the whole launch, so its Resume is not offered while
  // the tab starts; a failed launch takes that back.
  maviStore.markMoved(key, true);
  const summary = maviStore.summaryOf(record);
  if (summary && summary.phase !== "ended") {
    await maviStore.stop(key);
    if (!(await maviStore.waitForEnd(key))) {
      maviStore.markMoved(key, false);
      return { ok: false, reason: "The Mavi agent did not stop in time. Nothing was moved." };
    }
  }
  const mode: PermissionMode | null = isPermissionMode(record.mode) ? record.mode : null;
  try {
    const launched = terminalLaunchRequests.request(tabPath, "claude", undefined, undefined, {
      resume: { sessionId: conversationId, mode },
      name: `Mavi: ${record.title}`.slice(0, 80),
    });
    repoStore.setTerminalOpen(true);
    await launched;
  } catch (error) {
    maviStore.markMoved(key, false);
    const why = error instanceof Error ? error.message : String(error);
    return { ok: false, reason: `Couldn't start it in the terminal: ${why} The conversation is intact; resume it here.` };
  }
  return { ok: true };
}

/** Resumes a moved conversation in Mavi, once no terminal tab holds it. */
export async function bringBackToMavi(key: string, acknowledged = false): Promise<HandoffResult> {
  const record = maviStore.record(key);
  if (!record?.conversationId) return { ok: false, reason: "This thread has no conversation to resume." };
  const holder = terminalHolding(record.conversationId);
  if (holder) {
    await focusTerminalSession(holder);
    return { ok: false, reason: "The conversation is still running in a terminal tab (shown now). End it there, then bring it back." };
  }
  try {
    await maviStore.resume(key, acknowledged);
  } catch (error) {
    return { ok: false, reason: error instanceof Error ? error.message : String(error) };
  }
  maviStore.markMoved(key, false);
  return { ok: true };
}

/** A plain shell in the thread's worktree, in the dock. Starts nothing in Mavi. */
export async function openShellHere(key: string): Promise<HandoffResult> {
  const record = maviStore.record(key);
  if (!record || record.target.kind !== "local") return { ok: false, reason: "Only a thread on this computer has a local worktree." };
  const tabPath = await openTab(record.target.root);
  if (!tabPath) return { ok: false, reason: "Couldn't open this repository as a tab." };
  try {
    const launched = terminalLaunchRequests.request(tabPath, "shell");
    repoStore.setTerminalOpen(true);
    await launched;
    return { ok: true };
  } catch (error) {
    return { ok: false, reason: error instanceof Error ? error.message : String(error) };
  }
}

/** What "Continue in Mavi" needs from a terminal tab, read before anything ends. */
export interface TerminalConversation {
  readonly tabKey: string;
  readonly conversationId: string;
  /** Where the agent ran: the checkout, or the subdirectory it started in. */
  readonly cwd: string;
  /** The tab's permission mode; null when it ran with the CLI's own default. */
  readonly mode: PermissionMode | null;
  readonly title: string;
}

/** The conversation a Claude Code tab is running, or why it cannot be moved. */
export function terminalConversation(
  tabKey: string,
  records: readonly TerminalSessionRecord[] = get(terminalSessions),
  byTerminal: (ptyId: string) => { sessionId: string; repoPath: string; startDir: string | null; mode: PermissionMode | null } | undefined = (id) =>
    hostedClaude.byTerminal(id),
): TerminalConversation | { reason: string } {
  const record = records.find((r) => r.key === tabKey);
  if (!record) return { reason: "That terminal tab is gone." };
  if (record.launcher !== "claude") return { reason: "Only a Claude Code tab's conversation can move to Mavi." };
  const hosted = record.sessionId ? byTerminal(record.sessionId) : undefined;
  if (!hosted) return { reason: "GitPulse cannot tell which Claude Code conversation this tab is running." };
  const sub = hosted.startDir?.trim();
  const cwd = !sub || sub === "." ? hosted.repoPath : sub.startsWith("/") || /^[A-Za-z]:[\\/]/.test(sub) ? sub : `${hosted.repoPath}/${sub}`;
  return {
    tabKey,
    conversationId: hosted.sessionId.toLowerCase(),
    cwd,
    mode: hosted.mode,
    title: record.title?.trim() || "Continued from a terminal",
  };
}

/** Waits until the tab's process has ended, up to `ms`. */
async function waitForTabEnd(tabKey: string, conversationId: string, ms = 5000): Promise<boolean> {
  const started = Date.now();
  for (;;) {
    const record = get(terminalSessions).find((r) => r.key === tabKey);
    const ended = !record || record.status === "exited" || record.status === "error";
    if (ended && !terminalHolding(conversationId)) return true;
    if (Date.now() - started > ms) return false;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
}

/**
 * Moves a Claude Code tab's conversation into a Mavi thread, with its earlier
 * turns read from the transcript. Everything refusable is checked before the
 * tab ends: the conversation is saved for this checkout (not a subdirectory or
 * another worktree, where Claude Code alone can resume it), no Mavi thread
 * already works in that worktree, and Mavi has room for another agent. Then
 * `confirm` asks the person (who also acknowledges Bypass there), the tab's
 * process ends, and only once it has exited does the Mavi agent start.
 */
export async function continueInMavi(
  tabKey: string,
  confirm: (move: TerminalConversation & { root: string; mode: MaviModeId }) => Promise<boolean>,
): Promise<HandoffResult & { key?: string }> {
  const found = terminalConversation(tabKey);
  if ("reason" in found) return { ok: false, reason: found.reason };
  const check = await maviClient.conversationCheck(found.cwd, found.conversationId).catch((error: unknown) => ({
    state: "unknown" as const,
    reason: error instanceof Error ? error.message : String(error),
    root: null,
  }));
  if (check.state !== "found" || !check.root) {
    const why =
      check.state === "elsewhere"
        ? "Claude Code saved this conversation for a subdirectory or another worktree, and resumes it only there. Keep it in the terminal."
        : check.reason ?? "Couldn't find the conversation.";
    return { ok: false, reason: `${why} Nothing was moved.` };
  }
  const root = check.root;
  const busy = maviStore.records.some((r) => {
    const phase = maviStore.summaryOf(r)?.phase;
    return r.target.kind === "local" && r.target.root === root && !!phase && phase !== "ended";
  });
  if (busy) {
    return { ok: false, reason: "A Mavi thread is already working in this worktree. End it first; nothing was moved." };
  }
  const live = Object.values(maviStore.summaries).filter((s) => s.phase !== "ended").length;
  if (live >= MAX_LIVE_THREADS) {
    return { ok: false, reason: `${MAX_LIVE_THREADS} Mavi threads are already running. End one first; nothing was moved.` };
  }
  const mode: MaviModeId = found.mode ?? "ask";
  if (!(await confirm({ ...found, root, mode }))) return { ok: false, reason: "" };

  const record = get(terminalSessions).find((r) => r.key === tabKey);
  try {
    await record?.close();
  } catch (error) {
    return { ok: false, reason: `Couldn't end the terminal session: ${error instanceof Error ? error.message : String(error)}` };
  }
  if (!(await waitForTabEnd(tabKey, found.conversationId))) {
    return {
      ok: false,
      reason:
        "The terminal session was asked to end but has not yet. Once its tab closes, resume the conversation from the tab's history or start it again here.",
    };
  }
  try {
    const key = await maviStore.adopt({
      root,
      conversationId: found.conversationId,
      mode,
      acknowledged: mode === "bypass",
      title: found.title === "Continued from a terminal" ? undefined : found.title,
    });
    return { ok: true, key };
  } catch (error) {
    const why = error instanceof Error ? error.message : String(error);
    return {
      ok: false,
      reason: `Couldn't start it in Mavi: ${why} The conversation is intact; resume it in a Claude Code tab with its id ${found.conversationId}.`,
    };
  }
}
