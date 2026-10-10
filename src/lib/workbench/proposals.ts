/**
 * Manvi's proposals, as the board's reviewer sees them.
 *
 * Two Manvi ops only *propose*: `work.scan.gaps` (tasks the repository seems to
 * need) and `work.enhancements.refine` (acceptance criteria, splits and
 * duplicate links for one task). Neither writes the board. This module is the
 * client side of the host methods that start, read and stop them, and of the
 * two that write what a person accepted:
 *
 * - `proposals.file` files a proposal through the board's task-intake path, so
 *   the related-task protection holds and filing twice is one card;
 * - `proposals.apply` accepts the selected parts of the refinement Manvi stored,
 *   at the proposal and task revisions that were reviewed, and is refused if
 *   the task moved. It is the one apply path: dc-store's own accept, which
 *   appends the criteria, merges the duplicate links and creates the splits.
 *
 * Nothing here calls a write on its own: every function that writes is called
 * from a person's click.
 */
import { getRefinement, newID, request, WorkbenchError, type RefinePart, type Task } from "./client";
import { selectionWire, type ModelSelection } from "./taskModel";

export type ProposalMethod =
  | "scans.start" | "scans.get" | "scans.cancel"
  | "enhancements.refine"
  | "proposals.file" | "proposals.apply";

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
function bool(value: unknown, what: string): boolean { return typeof value === "boolean" ? value : invalid(what); }
function list<T>(value: unknown, what: string, decode: (item: unknown) => T, max = 1000): T[] {
  if (!Array.isArray(value) || value.length > max) return invalid(what);
  return value.map(decode);
}

export const SCAN_STATES = ["running", "done", "deadline", "cancelled", "failed"] as const;
export type ScanState = (typeof SCAN_STATES)[number];
export const PROPOSAL_STATUSES = ["new", "known", "attached"] as const;
export type ProposalStatus = (typeof PROPOSAL_STATUSES)[number];

export interface ScanCoverage { category: string; status: string; detail: string; hidden: number }
export interface ScanSignal { source: string; result: string }
export interface ScanProposal {
  fingerprint: string; category: string; status: ProposalStatus;
  attached_item_id: string | null; attached_item_title: string | null;
  title: string; description: string; kind: string; priority: number; labels: string[]; signals: ScanSignal[];
}
export interface ScanView {
  scan_id: string; state: ScanState; repo_root: string; since: string; max_proposals: number;
  joined: boolean; started_at: number; finished_at: number | null; error: string;
  coverage: ScanCoverage[]; complete: boolean; board_complete: boolean;
  new_count: number; attached_count: number; over_cap: number; proposals: ScanProposal[];
}

export function scanProposal(value: unknown): ScanProposal {
  const raw = obj(value, "proposal");
  const status = PROPOSAL_STATUSES.find((s) => s === raw.status) ?? invalid("proposal status");
  const fingerprint = str(raw.fingerprint, "proposal");
  if (!/^[0-9a-f]{16}$/.test(fingerprint)) return invalid("proposal fingerprint");
  return {
    fingerprint, status, category: str(raw.category, "proposal"),
    attached_item_id: optStr(raw.attached_item_id, "proposal") ?? null,
    attached_item_title: optStr(raw.attached_item_title, "proposal") ?? null,
    title: str(raw.title, "proposal"), description: str(raw.description, "proposal"), kind: str(raw.kind, "proposal"),
    priority: int(raw.priority, "proposal"),
    labels: list(raw.labels, "proposal", (l) => str(l, "proposal"), 64),
    signals: raw.signals === undefined ? [] : list(raw.signals, "proposal", (s) => {
      const signal = obj(s, "proposal signal");
      return { source: str(signal.source, "proposal signal"), result: str(signal.result, "proposal signal") };
    }, 16),
  };
}

export function scanView(value: unknown): ScanView {
  const raw = obj(value, "scan");
  const state = SCAN_STATES.find((s) => s === raw.state) ?? invalid("scan state");
  const scan_id = str(raw.scan_id, "scan");
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(scan_id)) return invalid("scan id");
  return {
    scan_id, state, repo_root: str(raw.repo_root, "scan"), since: optStr(raw.since, "scan") ?? "",
    max_proposals: int(raw.max_proposals, "scan"), joined: raw.joined === true,
    started_at: int(raw.started_at, "scan"),
    finished_at: raw.finished_at === undefined ? null : int(raw.finished_at, "scan"),
    error: optStr(raw.error, "scan") ?? "",
    coverage: list(raw.coverage, "scan coverage", (c) => {
      const row = obj(c, "scan coverage");
      return { category: str(row.category, "scan coverage"), status: str(row.status, "scan coverage"),
        detail: optStr(row.detail, "scan coverage") ?? "", hidden: row.hidden === undefined ? 0 : int(row.hidden, "scan coverage") };
    }, 32),
    complete: bool(raw.complete, "scan"), board_complete: bool(raw.board_complete, "scan"),
    new_count: int(raw.new_count, "scan"), attached_count: int(raw.attached_count, "scan"), over_cap: int(raw.over_cap, "scan"),
    proposals: list(raw.proposals, "scan proposals", scanProposal, 200),
  };
}

// ---------------------------------------------------------------------------
// Scan: start, read, stop
// ---------------------------------------------------------------------------

/** Start (or join) a gap scan of the open repository. */
export async function startScan(repoPath: string, options: { since?: string; maxProposals?: number } = {}): Promise<ScanView> {
  const input: Record<string, unknown> = { repo_path: repoPath };
  if (options.since?.trim()) input.since = options.since.trim();
  if (options.maxProposals !== undefined) input.max_proposals = options.maxProposals;
  return scanView(await request("scans.start", input));
}
export async function getScan(scanId: string): Promise<ScanView> { return scanView(await request("scans.get", { scan_id: scanId })); }
export async function cancelScan(scanId: string): Promise<ScanView> { return scanView(await request("scans.cancel", { scan_id: scanId })); }

export function scanFinished(view: ScanView): boolean { return view.state !== "running"; }

/**
 * What the reviewer must be told about how much of the repository was seen.
 * Every line carries both numbers (what is shown, and what exists or was
 * hidden); an empty result is only "nothing found" when `complete` is true.
 */
export function coverageNotes(view: ScanView): string[] {
  const notes: string[] = [];
  const shown = view.proposals.filter((p) => p.status !== "attached").length;
  if (view.over_cap > 0) notes.push(`Showing ${shown} of ${shown + view.over_cap} new proposals (cap ${view.max_proposals}); ${view.over_cap} more were found and are not listed.`);
  if (view.state !== "done") notes.push(view.state === "running" ? "The scan is still running; this list is not final." : `The scan ended early (${view.state}${view.error ? `: ${view.error}` : ""}); the list is incomplete.`);
  if (!view.board_complete) notes.push("The board was not read in full, so a proposal may duplicate a task already on it.");
  for (const c of view.coverage) {
    if (c.status === "ok") continue;
    const hidden = c.hidden > 0 ? `; ${c.hidden} hidden` : "";
    notes.push(`${categoryLabel(c.category)}: ${c.status}${c.detail ? ` (${c.detail})` : ""}${hidden}.`);
  }
  return notes;
}
/** True when finding nothing, or few things, must not be read as the repository being clean. */
export function scanIsPartial(view: ScanView): boolean { return !view.complete || view.over_cap > 0 || view.state !== "done"; }

const CATEGORY_LABELS: Record<string, string> = {
  dead_symbols: "Unused code", changed_untested: "Changed without tests", verification_gaps: "Open verification gaps", doc_drift: "Stale doc paths",
};
export function categoryLabel(category: string): string { return CATEGORY_LABELS[category] ?? category; }

// ---------------------------------------------------------------------------
// Filing a scan proposal
// ---------------------------------------------------------------------------

export interface RelatedTask { item_id: string; title: string; status: string }
export type FileOutcome =
  | { outcome: "filed" | "already_filed"; item_id: string; title: string }
  | { outcome: "related_tasks_exist"; related: RelatedTask[] };
export interface FileDraft { title: string; description: string; kind: string; priority: number; labels: string[] }

type Origin = { kind: "scan"; fingerprint: string };

async function file(repoPath: string, origin: Origin, draft: FileDraft, reviewedRelated: readonly string[]): Promise<FileOutcome> {
  const raw = obj(await request("proposals.file", {
    repo_path: repoPath, origin, title: draft.title, description: draft.description, kind: draft.kind,
    priority: draft.priority, labels: draft.labels, reviewed_related: reviewedRelated,
  }), "filing receipt");
  if (raw.outcome === "related_tasks_exist") {
    if (raw.ok !== false) return invalid("filing receipt");
    return { outcome: "related_tasks_exist", related: list(raw.related, "filing receipt", (r) => {
      const row = obj(r, "related task");
      return { item_id: str(row.item_id, "related task"), title: str(row.title, "related task"), status: str(row.status, "related task") };
    }, 25) };
  }
  if (raw.ok !== true || (raw.outcome !== "filed" && raw.outcome !== "already_filed")) return invalid("filing receipt");
  return { outcome: raw.outcome, item_id: str(raw.item_id, "filing receipt"), title: str(raw.title, "filing receipt") };
}

/** File a scan proposal, as edited. Filing it again returns `already_filed`. */
export function fileScanProposal(repoPath: string, proposal: ScanProposal, draft: FileDraft, reviewedRelated: readonly string[] = []): Promise<FileOutcome> {
  return file(repoPath, { kind: "scan", fingerprint: proposal.fingerprint }, draft, reviewedRelated);
}
export function draftOf(proposal: ScanProposal): FileDraft {
  return { title: proposal.title, description: proposal.description, kind: proposal.kind, priority: proposal.priority, labels: [...proposal.labels] };
}

// ---------------------------------------------------------------------------
// Dismissal. A per-viewer convenience: a dismissed proposal stays out of this
// person's list on later scans. It never touches the board.
// ---------------------------------------------------------------------------

const DISMISSED_KEY = "gitpulse.scan-dismissed:";
export const MAX_DISMISSED = 500;
export function dismissedFingerprints(repoPath: string): Set<string> {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(DISMISSED_KEY + repoPath) ?? "[]");
    return new Set(Array.isArray(parsed) ? parsed.filter((f): f is string => typeof f === "string") : []);
  } catch { return new Set(); }
}
export function dismissFingerprint(repoPath: string, fingerprint: string): Set<string> {
  const kept = [...dismissedFingerprints(repoPath).add(fingerprint)].slice(-MAX_DISMISSED);
  try { localStorage.setItem(DISMISSED_KEY + repoPath, JSON.stringify(kept)); } catch { /* the dismissal lasts this session only */ }
  return new Set(kept);
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
/** The proposal as a diff against `task`: what it would add, and what the task already has. */
export function refineDiff(task: Task, result: RefineResult): RefineDiff {
  const have = new Set(task.acceptance_criteria.map((c) => c.trim()));
  const linked = new Set(task.links.filter((l) => l.kind === "duplicate_of").map((l) => l.item_id));
  return {
    stale: task.revision !== result.source_revision,
    criteria: result.criteria.map((c) => ({ ...c, state: have.has(c.text.trim()) ? "present" : "add" })),
    duplicates: result.duplicates.map((d) => ({ ...d, state: linked.has(d.id) ? "present" : "add" })),
    splits: result.splits,
  };
}

export interface ApplyReceipt { revision: number; criteriaAdded: number; linksAdded: number; createdTaskIds: string[] }
/**
 * Accept the selected parts of the stored refinement. Conditional on the
 * proposal's revision and on the task still being at `source_revision`; a task
 * that moved since is refused (`revision_conflict`) and nothing is written. The
 * new tasks' ids are made by the host, not here.
 */
export async function applyRefine(repoPath: string, result: RefineResult, parts: readonly RefinePart[]): Promise<ApplyReceipt> {
  const raw = obj(await request("proposals.apply", {
    repo_path: repoPath, id: result.enhancement_id, expected_revision: result.enhancement_revision,
    expected_task_revision: result.source_revision, fields: parts,
  }), "apply receipt");
  if (raw.ok !== true || raw.outcome !== "applied") return invalid("apply receipt");
  return {
    revision: int(raw.revision, "apply receipt"), criteriaAdded: int(raw.criteria_added, "apply receipt"), linksAdded: int(raw.links_added, "apply receipt"),
    createdTaskIds: list(raw.created_item_ids, "apply receipt", (id) => str(id, "apply receipt"), 16),
  };
}
/** Reject the stored refinement, so it does not wait for the next review. The task is untouched. */
export async function dismissRefine(result: RefineResult): Promise<void> {
  await request("enhancements.dismiss", { id: result.enhancement_id, request_id: newID(), expected_revision: result.enhancement_revision });
}
