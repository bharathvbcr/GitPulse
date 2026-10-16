import { WorkbenchError, request } from "./client";

/**
 * A task's verdict from Manvi's `work.verify.task`, as the host reads it.
 *
 * `could_not_run` is its own state on purpose: verification that never ran
 * (sidecar down, start refused, no answer) says nothing about the work, and
 * must not read as a failure or as a pass. `degraded` is a verdict that could
 * not be trusted and is never a pass either.
 */
export const VERDICT_STATES = ["none", "running", "passed", "failed", "degraded", "could_not_run"] as const;
export type VerdictState = (typeof VERDICT_STATES)[number];

export interface CriterionResult { index: number; text: string; status: string; findings: string[] }
export interface VerificationRecord {
  request_id: string; head: string; worktree: string;
  criteria: CriterionResult[]; degraded: string[]; critic_summary: string;
}
export interface Verification {
  state: VerdictState;
  /** The verdict was reached for a commit the worktree has since moved past. */
  stale: boolean;
  record: VerificationRecord | null;
}

export const VERDICT_LABELS: Readonly<Record<VerdictState, string>> = Object.freeze({
  none: "Not verified",
  running: "Verifying",
  passed: "Passed",
  failed: "Failed",
  degraded: "Degraded",
  could_not_run: "Could not run",
});

/** What a reader should take from each state, shown beside the chip. */
export const VERDICT_HINTS: Readonly<Record<VerdictState, string>> = Object.freeze({
  none: "No verification has judged this task.",
  running: "Manvi is checking the work against the acceptance criteria.",
  passed: "Every check ran and every criterion was met.",
  failed: "A check found a defect or an unmet criterion.",
  degraded: "A check could not run or could not be trusted. This is not a pass.",
  could_not_run: "Verification did not run, so there is no verdict. This is not a failure.",
});

const MAX_CRITERIA = 200, MAX_TEXT = 8192;

function bounded(value: unknown): string {
  return typeof value === "string" ? value.slice(0, MAX_TEXT) : "";
}

function criterion(value: unknown): CriterionResult | null {
  if (typeof value !== "object" || value === null) return null;
  const raw = value as Record<string, unknown>;
  return {
    index: typeof raw.index === "number" && Number.isFinite(raw.index) ? raw.index : 0,
    text: bounded(raw.text),
    status: bounded(raw.status) || "unknown",
    findings: Array.isArray(raw.findings) ? raw.findings.slice(0, 50).map(bounded) : [],
  };
}

/** Decodes the host's answer; anything unrecognised is refused rather than shown as a state. */
export function decodeVerification(response: unknown): Verification {
  const raw = typeof response === "object" && response !== null ? (response as Record<string, unknown>) : null;
  const state = VERDICT_STATES.find((s) => s === raw?.state);
  if (!raw || raw.ok !== true || !state) throw new WorkbenchError("protocol_error", "Task storage returned an invalid verification.");
  let record: VerificationRecord | null = null;
  if (typeof raw.record === "object" && raw.record !== null) {
    const r = raw.record as Record<string, unknown>;
    record = {
      request_id: bounded(r.request_id), head: bounded(r.head), worktree: bounded(r.worktree),
      criteria: (Array.isArray(r.criteria) ? r.criteria.slice(0, MAX_CRITERIA) : []).map(criterion).filter((c): c is CriterionResult => c !== null),
      degraded: Array.isArray(r.degraded) ? r.degraded.slice(0, 50).map(bounded) : [],
      critic_summary: bounded(r.critic_summary),
    };
  }
  return { state, stale: raw.stale === true, record };
}

export async function getVerification(id: string): Promise<Verification> {
  return decodeVerification(await request("verifications.status", { id }));
}

/** Starts a verification of the task's current work. The verdict arrives through {@link getVerification}. */
export async function runVerification(id: string): Promise<void> {
  const raw = await request("verifications.run", { id });
  if (typeof raw !== "object" || raw === null) throw new WorkbenchError("protocol_error", "Task storage returned an invalid verification.");
}

/** What a board card shows: the state, and whether it is for an earlier commit. */
export interface CardVerdict { state: Exclude<VerdictState, "none">; stale: boolean }

/** Whether two verdict reads say the same about every card; a board keeps the one it shows when they do. */
export function sameVerdicts(a: ReadonlyMap<string, CardVerdict>, b: ReadonlyMap<string, CardVerdict>): boolean {
  if (a.size !== b.size) return false;
  for (const [id, verdict] of b) {
    const shown = a.get(id);
    if (!shown || shown.state !== verdict.state || shown.stale !== verdict.stale) return false;
  }
  return true;
}

/** The host's cap on one batched read (a board page is 30 cards in each of 7 columns). */
export const MAX_VERDICT_BATCH = 256;

/**
 * Verdicts for every card on the board in one read. Cards never verified are
 * absent from the answer; a read that fails throws, so the caller can say so
 * instead of drawing every card as unverified.
 */
export async function getVerdicts(ids: readonly string[]): Promise<Map<string, CardVerdict>> {
  const unique = [...new Set(ids)];
  const verdicts = new Map<string, CardVerdict>();
  if (!unique.length) return verdicts;
  if (unique.length > MAX_VERDICT_BATCH) throw new WorkbenchError("invalid_input", "Too many tasks for one verdict read.");
  const raw = await request("verifications.board", { ids: unique });
  const items = typeof raw === "object" && raw !== null && (raw as Record<string, unknown>).ok === true ? (raw as Record<string, unknown>).items : undefined;
  if (typeof items !== "object" || items === null) throw new WorkbenchError("protocol_error", "Task storage returned an invalid verification.");
  for (const [id, value] of Object.entries(items)) {
    const one = decodeVerification(value);
    if (one.state !== "none" && unique.includes(id)) verdicts.set(id, { state: one.state, stale: one.stale });
  }
  return verdicts;
}
