/**
 * The real effects behind a wave: Manvi's suggestions, the attempt launcher,
 * the verify gate, the worktree merge and the task store. Each is the seam the
 * rest of the app already uses; this file only adapts them to `WaveDeps`.
 */

import { get } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { identityKey, isCaseInsensitiveFs } from "../repos/paths";
import { interfaceStore } from "../stores/interfaceStore";
import { repoStore } from "../stores/repoStore";
import { loadTaskScopes } from "../tasks/scopes";
import type { WorktreeInfo } from "../branches/types";
import {
  MAX_SUGGESTIONS, WorkbenchError, getRepository, getTask, getTaskRun, listTasks, newID, prepareTaskRun, putTask, suggestTasks, taskWrite, type TaskRun,
} from "./client";
import { mergeVerifiedAttempt } from "./attemptWorktree";
import { WORKTREE_PREPARE_TIMEOUT_MS, bounded } from "./taskActions";
import { checkoutCandidates, normalizeCheckout, preferredCheckout } from "./taskHandoff";
import { startPreparedAttempt } from "./taskTerminal";
import { getVerification, runVerification } from "./verification";
import { supportsManaged } from "./vocabulary";
import { createWavePersister, findActiveWave, type WaveTaskApi } from "./waveStore";
import { resumeWave, startWave, type RunRead, type Suggestion, type WaveDeps, type WaveRunner } from "./waveLaunch";
import type { TaskSuggestions } from "./client";

/** `work.verify.task` can take minutes; a verdict still `running` after this is reported as such, never trusted. */
const VERIFY_WAIT_MS = 20 * 60_000;
const VERIFY_POLL_MS = 3_000;

export const taskApi: WaveTaskApi = {
  getTask,
  saveTask: (id, revision, draft) => putTask(taskWrite(id, revision, draft)),
  listCards: (status, cursor) => listTasks({ kind: "global" }, status, "", cursor, 100),
};

/** What a wave takes from a suggest answer: the ranking, and why it may not be a ranking of the whole board. */
export function readSuggestion(answer: TaskSuggestions): Suggestion {
  const reasons: string[] = [];
  if (!answer.board.complete) reasons.push(`the board was only partly read (${answer.board.scanned} tasks scanned)`);
  if (answer.board.unhydrated > 0) reasons.push(`${answer.board.unhydrated} open task${answer.board.unhydrated === 1 ? "'s" : "s'"} links were not read`);
  if (answer.excluded.unread > 0) reasons.push(`${answer.excluded.unread} task${answer.excluded.unread === 1 ? " was" : "s were"} left out unread`);
  const blocked = new Set(answer.blocked.map((entry) => entry.id));
  return {
    candidates: answer.suggestions.filter((entry) => !blocked.has(entry.id)).map((entry) => ({ id: entry.id, title: entry.title })),
    unread: reasons.length ? `${reasons.join("; ")}, so a blocker may be among what was not read.` : "",
    unchecked: answer.verification.unchecked,
  };
}

function readRun(run: TaskRun): RunRead {
  return { state: run.state, exitCode: run.exit_code, cwd: run.cwd, reason: run.reason };
}

/** The checkout a wave launches from: the repository's first open tab, else the folder beside its common dir. */
async function checkoutFor(repositoryId: string): Promise<string> {
  const repository = await getRepository(repositoryId);
  const tabs = get(repoStore).openTabs.map((tab) => ({ path: tab.path, label: tab.label }));
  const path = normalizeCheckout(preferredCheckout(checkoutCandidates(repositoryId, [repository], tabs, { caseInsensitive: isCaseInsensitiveFs() })));
  if (!path) throw new Error("No working checkout of this repository is open, so a wave has nowhere to launch from.");
  return path;
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}

export async function waveDeps(repositoryId: string, persist: WaveDeps["persist"]): Promise<WaveDeps> {
  const checkout = await checkoutFor(repositoryId);
  return {
    pollMs: 2_500,
    now: () => Date.now(),
    id: newID,
    sleep,
    persist,
    suggest: async (signal) => readSuggestion(await bounded(suggestTasks({ limit: MAX_SUGGESTIONS, repository_id: repositoryId }), undefined, undefined, signal)),
    scopes: async (ids) => {
      const found = await loadTaskScopes(invoke, checkout, ids);
      return new Map(Object.values(found).map((scope) => [scope.id, [...scope.planned_files, ...scope.agent_appended_files]]));
    },
    launch: async (taskId, ids, signal) => {
      // The provider the person last chose when it can be supervised, else Claude; never bypass, which is acknowledged afresh on every launch.
      const saved = get(interfaceStore).taskHandoff;
      const latest = await bounded(getTask(taskId), undefined, undefined, signal);
      const repository = await bounded(getRepository(repositoryId), undefined, undefined, signal);
      const run = await bounded(prepareTaskRun({
        kind: "managed", id: ids.runId, request_id: ids.requestId, task_id: latest.id, source_revision: latest.revision,
        repository_id: repository.id, repository_revision: repository.revision, repo_path: checkout, worktree: true,
        provider: supportsManaged(saved.provider) ? saved.provider : "claude",
        permission_mode: saved.permission === "bypass" ? "ask" : saved.permission, acknowledge_bypass: false,
      }), WORKTREE_PREPARE_TIMEOUT_MS, undefined, signal);
      const started = await startPreparedAttempt(run);
      if (!started.ok) throw new Error(started.error);
      return readRun(started.run);
    },
    getRun: async (runId) => {
      try { return readRun(await getTaskRun(runId)); }
      catch (cause) { if (cause instanceof WorkbenchError && cause.code === "not_found") return null; throw cause; }
    },
    verify: async (taskId, signal) => {
      await bounded(runVerification(taskId), undefined, undefined, signal);
      const until = Date.now() + VERIFY_WAIT_MS;
      for (;;) {
        const verdict = await bounded(getVerification(taskId), undefined, undefined, signal);
        if (verdict.state !== "running" || Date.now() > until) return { state: verdict.state, stale: verdict.stale, head: verdict.record?.head || null };
        await sleep(VERIFY_POLL_MS, signal);
        if (signal.aborted) throw new Error("The wave was stopped while waiting for verification.");
      }
    },
    headOf: async (cwd) => {
      const opts = { caseInsensitive: isCaseInsensitiveFs() };
      const key = identityKey(cwd, opts);
      const own = (await invoke<WorktreeInfo[]>("cmd_list_worktrees", { repoPath: checkout })).find((entry) => identityKey(entry.path, opts) === key);
      if (!own) throw new Error(`${cwd} is no longer a worktree of the repository.`);
      return own.head;
    },
    merge: async ({ runId, cwd, head }) => (await mergeVerifiedAttempt({ id: runId, cwd, state: "exited", expires_at: 0 }, head)).message,
  };
}

/** The one wave this window is running. A second never starts beside it: two would race for the same suggestions and checkouts. */
let current: WaveRunner | null = null;

export function currentWave(): WaveRunner | null {
  return current;
}

function adopt(runner: WaveRunner): WaveRunner {
  current = runner;
  return runner;
}

export async function beginWave(input: { repositoryId: string; n: number; waves: number; acceptUnchecked?: boolean }): Promise<WaveRunner> {
  if (current?.record().state === "running") throw new Error("A wave is already running. Stop it before starting another.");
  const found = await findActiveWave(taskApi);
  if (found.record) throw new Error("A wave from before a restart is still marked running on the board. Resume or stop it before starting another.");
  if (!found.complete) throw new Error("The board could not be read far enough to be sure no other wave is running, so none was started.");
  return adopt(await startWave(input, await waveDeps(input.repositoryId, createWavePersister(taskApi))));
}

/** Continues the wave a restart left running. `complete` false means some cards could not be read, so no wave found is not proof of none. */
export async function resumeActiveWave(): Promise<{ runner: WaveRunner | null; complete: boolean }> {
  if (current) return { runner: current, complete: true };
  const found = await findActiveWave(taskApi);
  if (!found.record) return { runner: null, complete: found.complete };
  const persist = createWavePersister(taskApi, found.record.members.map((m) => m.taskId));
  return { runner: adopt(await resumeWave(found.record, await waveDeps(found.record.repositoryId, persist))), complete: found.complete };
}
