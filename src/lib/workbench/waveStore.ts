/**
 * Where a wave lives between restarts: on the task cards it is working on.
 *
 * No table of its own. A running wave marks each card of its current batch
 * with the label `wave:<id>` (so a restart finds it by label) and keeps its
 * whole record as one `[gitpulse-wave] {json}` line in that card's logs. The
 * runs, verdicts and merges the record refers to are already durable in the
 * store; this holds only the plan around them. A card whose line does not
 * decode, or whose record is for another wave, is ignored rather than guessed
 * at, and a read that could not see every card says so.
 */

import { nextLabels } from "./taskActions";
import { MAX_ATTEMPTED, MAX_WAVE_SIZE, MAX_WAVES, type MemberPhase, type WaveMember, type WaveRecord, type WaveState } from "./waveLaunch";
import type { Task, TaskCard, TaskDraft, TaskStatus } from "./client";

export const WAVE_MARK = "[gitpulse-wave] ";
export const WAVE_LABEL_PREFIX = "wave:";
/** Pages of one status that discovery reads before it reports the answer as partial. */
export const DISCOVERY_PAGES = 4;

export interface WaveTaskApi {
  getTask(id: string): Promise<Task>;
  /** Saves at `revision`; rejects when the card moved. */
  saveTask(id: string, revision: number, draft: TaskDraft): Promise<Task>;
  listCards(status: TaskStatus, cursor?: string): Promise<{ items: TaskCard[]; has_more: boolean; next_cursor: string | null }>;
}

const PHASES: readonly MemberPhase[] = ["launching", "running", "verifying", "ready", "merging", "merged", "held", "stopped"];
const STATES: readonly WaveState[] = ["running", "done", "cancelled", "halted", "failed", "refused"];
const MAX_TEXT = 4096;

export const waveLabel = (id: string) => `${WAVE_LABEL_PREFIX}${id}`;

export function encodeWave(record: WaveRecord): string {
  return `${WAVE_MARK}${JSON.stringify(record)}`;
}

const str = (value: unknown, max = MAX_TEXT): string | null => (typeof value === "string" && value.length <= max ? value : null);
const int = (value: unknown, lo: number, hi: number): number | null => (typeof value === "number" && Number.isInteger(value) && value >= lo && value <= hi ? value : null);

const nullable = (value: unknown, max: number): { ok: true; value: string | null } | { ok: false } =>
  value === null ? { ok: true, value: null } : typeof value === "string" && value.length <= max ? { ok: true, value } : { ok: false };

function decodeMember(value: unknown): WaveMember | null {
  if (typeof value !== "object" || value === null) return null;
  const m = value as Record<string, unknown>;
  const phase = PHASES.find((p) => p === m.phase);
  const taskId = str(m.taskId, 128), title = str(m.title), runId = str(m.runId, 128), requestId = str(m.requestId, 128), detail = str(m.detail);
  const cwd = nullable(m.cwd, 4096), head = nullable(m.head, 128);
  const startedAt = typeof m.startedAt === "number" && Number.isFinite(m.startedAt) ? m.startedAt : null;
  if (!phase || !taskId || title === null || !runId || !requestId || detail === null || !cwd.ok || !head.ok || startedAt === null) return null;
  return { taskId, title, phase, runId, requestId, cwd: cwd.value, head: head.value, detail, startedAt };
}

/** The record in a card's logs, or null when there is none or it is not a record this build can trust. */
export function decodeWave(logs: string | undefined): WaveRecord | null {
  if (!logs) return null;
  const line = logs.split("\n").find((entry) => entry.startsWith(WAVE_MARK));
  if (!line) return null;
  let raw: unknown;
  try { raw = JSON.parse(line.slice(WAVE_MARK.length)); } catch { return null; }
  if (typeof raw !== "object" || raw === null) return null;
  const r = raw as Record<string, unknown>;
  const state = STATES.find((s) => s === r.state);
  const id = str(r.id, 128), repositoryId = str(r.repositoryId, 128), note = str(r.note);
  const n = int(r.n, 1, MAX_WAVE_SIZE), batchesLeft = int(r.batchesLeft, 0, MAX_WAVES), batch = int(r.batch, 0, 1_000), seq = int(r.seq, 0, Number.MAX_SAFE_INTEGER);
  if (r.v !== 1 || !state || !id || !repositoryId || note === null || n === null || batchesLeft === null || batch === null || seq === null) return null;
  if (!Array.isArray(r.members) || r.members.length > MAX_WAVE_SIZE || !Array.isArray(r.attempted) || r.attempted.length > MAX_ATTEMPTED) return null;
  const members = r.members.map(decodeMember);
  const attempted = r.attempted.map((entry) => str(entry, 128));
  if (members.some((m) => m === null) || attempted.some((a) => a === null)) return null;
  if (r.acceptUnchecked !== undefined && typeof r.acceptUnchecked !== "boolean") return null;
  const decoded: WaveRecord = { v: 1, id, repositoryId, n, batchesLeft, batch, state, members: members as WaveMember[], attempted: attempted as string[], note, seq };
  if (r.acceptUnchecked !== undefined) decoded.acceptUnchecked = r.acceptUnchecked;
  return decoded;
}

function withLine(logs: string | undefined, line: string): string {
  const kept = (logs ?? "").split("\n").filter((entry) => !entry.startsWith(WAVE_MARK));
  while (kept.length && kept[kept.length - 1] === "") kept.pop();
  return [...kept, line].join("\n");
}

async function stamp(api: WaveTaskApi, taskId: string, change: (task: Task) => { labels: string[]; logs: string }): Promise<void> {
  // One re-read for a card that moved under the write; a second conflict is real and stops the wave.
  for (let attempt = 0; ; attempt += 1) {
    const task = await api.getTask(taskId);
    const next = change(task);
    try { await api.saveTask(taskId, task.revision, { ...stripStamps(task), labels: next.labels, logs: next.logs }); return; }
    catch (cause) {
      if (attempt >= 1) throw cause;
    }
  }
}

function stripStamps(task: Task): TaskDraft {
  const { id: _id, revision: _revision, updated_at: _updated, completed_at: _completed, ...draft } = task;
  return draft;
}

/**
 * A persister for one wave: the record goes to every card of the current
 * batch; cards that left the batch, or every card once the wave stops
 * running, lose the label (their last record stays in their logs as history).
 */
export function createWavePersister(api: WaveTaskApi, held: Iterable<string> = []): (record: WaveRecord) => Promise<void> {
  const labelled = new Set<string>(held);
  return async (record) => {
    const label = waveLabel(record.id), line = encodeWave(record);
    const active = record.state === "running";
    const current = new Set(record.members.map((m) => m.taskId));
    for (const taskId of current) {
      await stamp(api, taskId, (task) => ({ labels: nextLabels(task.labels, label, active), logs: withLine(task.logs, line) }));
      if (active) labelled.add(taskId); else labelled.delete(taskId);
    }
    for (const taskId of [...labelled]) {
      if (active && current.has(taskId)) continue;
      await stamp(api, taskId, (task) => ({ labels: nextLabels(task.labels, label, false), logs: task.logs ?? "" }));
      labelled.delete(taskId);
    }
  };
}

export interface ActiveWave { record: WaveRecord | null; complete: boolean }

/**
 * The running wave a restart should continue, found by its label among the
 * cards that can carry it. `complete` is false when a page cap or a failed
 * read kept some cards unseen, so "no wave" is never claimed from a partial look.
 */
export async function findActiveWave(api: WaveTaskApi): Promise<ActiveWave> {
  let complete = true;
  const found = new Map<string, WaveRecord>();
  for (const status of ["in_progress", "ready", "backlog", "inbox", "review"] as const) {
    let cursor: string | undefined;
    for (let page = 0; page < DISCOVERY_PAGES; page += 1) {
      const result = await api.listCards(status, cursor);
      for (const card of result.items) {
        const label = card.labels.find((entry) => entry.startsWith(WAVE_LABEL_PREFIX));
        if (!label) continue;
        const record = decodeWave((await api.getTask(card.id)).logs);
        if (!record || record.state !== "running" || waveLabel(record.id) !== label) continue;
        const best = found.get(record.id);
        if (!best || record.seq > best.seq) found.set(record.id, record);
      }
      if (!result.has_more || !result.next_cursor) break;
      cursor = result.next_cursor;
      if (page === DISCOVERY_PAGES - 1) complete = false;
    }
  }
  const newest = [...found.values()].sort((a, b) => b.seq - a.seq)[0];
  return { record: newest ?? null, complete };
}
