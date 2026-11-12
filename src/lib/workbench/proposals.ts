/**
 * Manvi's refinement proposals, as the board's reviewer sees them.
 *
 * `work.enhancements.refine` only *proposes* acceptance criteria, splits and
 * duplicate links for one task. It does not write the board. This module is
 * the client side of the host method that starts that proposal, and of the one
 * that writes what a person accepted:
 *
 * - `proposals.apply` accepts the selected parts of the refinement Manvi stored,
 *   at the proposal and task revisions that were reviewed, and is refused if
 *   the task moved. It is the one apply path: dc-store's own accept, which
 *   appends the criteria, merges the duplicate links and creates the splits.
 *
 * Nothing here calls a write on its own: every function that writes is called
 * from a person's click.
 */
import { getRefinement, newID, request, REFINE_PARTS, WorkbenchError, type RefinePart, type StoredRefinement, type Task } from "./client";
import { selectionWire, type ModelSelection } from "./taskModel";

export type ProposalMethod =
  | "enhancements.refine"
  | "proposals.apply";

// ---------------------------------------------------------------------------
// Decoding. A reply that is not exactly the contract is refused whole.
// ---------------------------------------------------------------------------

function invalid(what: string): never {
  throw new WorkbenchError("protocol_error", `Manvi returned an invalid ${what}.`);
}
function obj(value: unknown, what: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return invalid(what);
  return value as Record<string, unknown>;
}
function str(value: unknown, what: string): string { return typeof value === "string" ? value : invalid(what); }
function optStr(value: unknown, what: string): string | undefined { return value === undefined ? undefined : str(value, what); }
function int(value: unknown, what: string): number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0 ? value : invalid(what);
}
function list<T>(value: unknown, what: string, decode: (item: unknown) => T, max = 1000): T[] {
  if (!Array.isArray(value) || value.length > max) return invalid(what);
  return value.map(decode);
}

export interface RelatedTask { item_id: string; title: string; status: string }

/**
 * The most `item_id`s one `reviewed_related` may carry. The host's own bound
 * (`MAX_REVIEWED_RELATED`, intake.rs), which the `gitpulse_add_task` schema
 * advertises too; a list longer than this is refused whole.
 */
export const MAX_REVIEWED_RELATED = 500;
/**
 * The reviewed set to send when the person confirms the related tasks they
 * were just shown: everything they confirmed before, plus these.
 *
 * A refusal names one page of related tasks (25) and omits those already
 * reviewed, so the next refusal is the next page. Sending back only the page on
 * screen drops the earlier ones, which then reappear: past 25 related tasks the
 * two refusals alternate for ever. This is the single owner of that set when
 * applying a refinement; the host is the other half of the protocol
 * (`is_reviewed` in intake.rs).
 */
export function confirmReviewed(reviewed: readonly string[], shown: readonly Pick<RelatedTask, "item_id">[]): string[] {
  const kept = new Set<string>();
  for (const id of [...reviewed, ...shown.map((t) => t.item_id)]) {
    const trimmed = id.trim();
    if (trimmed && kept.size < MAX_REVIEWED_RELATED) kept.add(trimmed);
  }
  return [...kept];
}

// ---------------------------------------------------------------------------
// Refinement
// ---------------------------------------------------------------------------

export interface RefineCite { text: string; cites: string[] }
export interface RefineGrounding { status: string; reason: string; index_fresh: boolean; symbols: { symbol: string; found: boolean; error: string; truncated: boolean; walk_incomplete: string }[] }
export interface RefineResult {
  /** The proposal Manvi stored in dc-store, and its revision when it was reviewed: what accepting is conditional on. */
  enhancement_id: string; enhancement_revision: number;
  task_id: string; request_id: string; source_revision: number; provider: string; model: string;
  criteria: RefineCite[]; splits: { title: string; description: string; cites: string[] }[];
  duplicates: { id: string; reason: string; cites: string[] }[]; rationale: string;
  dropped: { kind: string; ref: string; reason: string }[]; grounding: RefineGrounding;
}

const cites = (value: unknown): string[] => list(value ?? [], "refinement", (c) => str(c, "refinement"), 64);

export function refineResult(value: unknown): RefineResult {
  const raw = obj(value, "refinement");
  // `applied` is Manvi's promise that it changed nothing; a reply that says otherwise is not a proposal.
  if (raw.ok !== true || raw.applied !== false) return invalid("refinement");
  const proposal = obj(raw.proposal, "refinement");
  const grounding = obj(raw.grounding, "refinement grounding");
  return {
    enhancement_id: str(raw.enhancement_id, "refinement"), enhancement_revision: 0,
    task_id: str(raw.task_id, "refinement"), request_id: str(raw.request_id, "refinement"),
    source_revision: int(raw.source_revision, "refinement"), provider: str(raw.provider, "refinement"), model: str(raw.model, "refinement"),
    criteria: list(proposal.acceptance_criteria, "refinement", (c) => { const r = obj(c, "refinement"); return { text: str(r.text, "refinement"), cites: cites(r.cites) }; }, 64),
    splits: list(proposal.split_into, "refinement", (s) => { const r = obj(s, "refinement"); return { title: str(r.title, "refinement"), description: str(r.description, "refinement"), cites: cites(r.cites) }; }, 16),
    duplicates: list(proposal.duplicate_of, "refinement", (d) => { const r = obj(d, "refinement"); return { id: str(r.id, "refinement"), reason: str(r.reason, "refinement"), cites: cites(r.cites) }; }, 64),
    rationale: optStr(proposal.rationale, "refinement") ?? "",
    dropped: list(raw.dropped ?? [], "refinement", (d) => { const r = obj(d, "refinement"); return { kind: str(r.kind, "refinement"), ref: optStr(r.ref, "refinement") ?? "", reason: str(r.reason, "refinement") }; }, 256),
    grounding: {
      status: str(grounding.status, "refinement grounding"), reason: optStr(grounding.reason, "refinement grounding") ?? "",
      index_fresh: grounding.index_fresh === true,
      symbols: list(grounding.symbols ?? [], "refinement grounding", (s) => {
        const r = obj(s, "refinement grounding");
        const defs = obj(r.definitions ?? {}, "refinement grounding"), impact = r.impact === undefined ? {} : obj(r.impact, "refinement grounding");
        return { symbol: str(r.symbol, "refinement grounding"), found: r.found === true, error: optStr(r.error, "refinement grounding") ?? "",
          truncated: defs.truncated === true || impact.truncated === true, walk_incomplete: optStr(impact.walk_incomplete, "refinement grounding") ?? "" };
      }, 16),
    },
  };
}

/** Ask Manvi to refine one task. One request id per attempt: a retry of an uncertain reply replays, it does not call the model again. */
export async function requestRefine(repoPath: string, task: Task, selection: ModelSelection | null | undefined, requestId: string = newID()): Promise<RefineResult> {
  const result = refineResult(await request("enhancements.refine", {
    repo_path: repoPath, id: task.id, request_id: requestId, expected_revision: task.revision, ...selectionWire(selection),
  }));
  if (result.task_id !== task.id || !/^[A-Za-z0-9_-]{1,128}$/.test(result.enhancement_id)) return invalid("refinement");
  // What was stored is what is reviewed and accepted: a reply that names a proposal the store does not hold as ready for this task is not one.
  const stored = await getRefinement(result.enhancement_id);
  if (stored.task_id !== task.id || stored.state !== "ready" || stored.source_revision !== result.source_revision) return invalid("refinement");
  return { ...result, enhancement_revision: stored.revision };
}

export type DiffState = "add" | "present";
export interface RefineDiff {
  /** The proposal was made against an older revision of the task than the one on the board now. */
  stale: boolean;
  criteria: (RefineCite & { state: DiffState })[];
  duplicates: (RefineResult["duplicates"][number] & { state: DiffState })[];
  splits: RefineResult["splits"];
}
/**
 * The proposal as a diff against `task`: what it would add, and what the task already has.
 *
 * Criteria are compared trimmed on both sides, and shown as they would be
 * stored. A whitespace-only criterion is nothing, and one repeated within the
 * proposal is one: the store appends what it is given and checks neither.
 */
export function refineDiff(task: Task, result: RefineResult): RefineDiff {
  const have = new Set(task.acceptance_criteria.map((c) => c.trim()));
  const linked = new Set(task.links.filter((l) => l.kind === "duplicate_of").map((l) => l.item_id));
  const seen = new Set<string>();
  const criteria: RefineDiff["criteria"] = [];
  for (const c of result.criteria) {
    const text = c.text.trim();
    if (!text || seen.has(text)) continue;
    seen.add(text);
    criteria.push({ ...c, text, state: have.has(text) ? "present" : "add" });
  }
  return {
    stale: task.revision !== result.source_revision,
    criteria,
    duplicates: result.duplicates.map((d) => ({ ...d, state: linked.has(d.id) ? "present" : "add" })),
    splits: result.splits,
  };
}

/**
 * Which parts of a proposal the person can choose, and which start chosen.
 *
 * A part the task has locked is never offered and never ticked: the store
 * refuses it as `field_locked`. Criteria start chosen only when one is new;
 * splits create tasks and duplicates link one, so they wait for a choice.
 */
export function selectableParts(task: Task, result: RefineResult): { locked: RefinePart[]; initial: RefinePart[] } {
  const lockedFields: readonly string[] = task.locked_fields ?? [];
  const locked = REFINE_PARTS.filter((part) => lockedFields.includes(part));
  const fresh = refineDiff(task, result).criteria.some((c) => c.state === "add");
  return { locked, initial: fresh && !locked.includes("acceptance_criteria") ? ["acceptance_criteria"] : [] };
}

export interface ApplyConflict { cause: "proposal" | "task" | "unknown"; message: string }
/**
 * Why an apply was refused as a `revision_conflict`, from a fresh read of both
 * sides: the proposal (dismissed or accepted elsewhere, or revised) or the task
 * (edited since the review). The store's one code covers both, and naming the
 * wrong one sends the person to re-read a task that did not move. The proposal
 * is checked first: one settled elsewhere is the more specific cause. A side
 * that could not be re-read is not assumed unchanged.
 */
export function applyConflict(reviewed: RefineResult, task: Pick<Task, "revision"> | null, stored: Pick<StoredRefinement, "state" | "revision"> | null): ApplyConflict {
  if (stored && (stored.state !== "ready" || stored.revision !== reviewed.enhancement_revision)) {
    const why = stored.state === "accepted" ? "was already applied elsewhere"
      : stored.state === "dismissed" ? "was dismissed elsewhere"
      : stored.state === "ready" ? "was changed elsewhere" : `is now ${stored.state}`;
    return { cause: "proposal", message: `This suggestion ${why}, so nothing was applied here. Ask again for a fresh one.` };
  }
  if (task && task.revision !== reviewed.source_revision) {
    return { cause: "task", message: "The task changed after this suggestion was made, so nothing was applied. It has been re-read; ask again to refine the current version." };
  }
  return { cause: "unknown", message: "The suggestion or the task changed after it was made, so nothing was applied. Ask again to refine the current version." };
}

export interface ApplyReceipt { outcome: "applied"; revision: number; criteriaAdded: number; linksAdded: number; createdTaskIds: string[] }
/** Nothing was applied: a proposed split looks like open work, and the person must say it is separate. */
export interface ApplyHeld { outcome: "related_tasks_exist"; splits: { index: number; title: string; related: RelatedTask[] }[] }
const relatedRows = (value: unknown): RelatedTask[] => list(value, "apply receipt", (r) => {
  const row = obj(r, "related task");
  return { item_id: str(row.item_id, "related task"), title: str(row.title, "related task"), status: str(row.status, "related task") };
}, 25);
/**
 * Accept the selected parts of the stored refinement. Conditional on the
 * proposal's revision and on the task still being at `source_revision`; a task
 * that moved since is refused (`revision_conflict`) and nothing is written. The
 * new tasks' ids are made by the host, not here. A split that looks like open
 * work holds the whole accept (`related_tasks_exist`, nothing written) until
 * the person names those tasks in `reviewedRelated`.
 */
export async function applyRefine(repoPath: string, result: RefineResult, parts: readonly RefinePart[], reviewedRelated: readonly string[] = []): Promise<ApplyReceipt | ApplyHeld> {
  const raw = obj(await request("proposals.apply", {
    repo_path: repoPath, id: result.enhancement_id, expected_revision: result.enhancement_revision,
    expected_task_revision: result.source_revision, fields: parts, reviewed_related: reviewedRelated,
  }), "apply receipt");
  if (raw.outcome === "related_tasks_exist") {
    if (raw.ok !== false) return invalid("apply receipt");
    return { outcome: "related_tasks_exist", splits: list(raw.splits, "apply receipt", (s) => {
      const row = obj(s, "held split");
      return { index: int(row.index, "held split"), title: str(row.title, "held split"), related: relatedRows(row.related) };
    }, 16) };
  }
  if (raw.ok !== true || raw.outcome !== "applied") return invalid("apply receipt");
  return {
    outcome: "applied", revision: int(raw.revision, "apply receipt"), criteriaAdded: int(raw.criteria_added, "apply receipt"), linksAdded: int(raw.links_added, "apply receipt"),
    createdTaskIds: list(raw.created_item_ids, "apply receipt", (id) => str(id, "apply receipt"), 16),
  };
}
/** Reject the stored refinement, so it does not wait for the next review. The task is untouched. */
export async function dismissRefine(result: RefineResult): Promise<void> {
  await request("enhancements.dismiss", { id: result.enhancement_id, request_id: newID(), expected_revision: result.enhancement_revision });
}
