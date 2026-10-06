/**
 * Filing a saved task as a GitHub issue.
 *
 * The issue is created by the existing guarded owner
 * (`repoStore.reportIssue` → `cmd_github_create_issue`), which validates the
 * payload, asks the MANVI gate about the exact `gh issue create` argv and pins
 * `--repo` to the checkout's discovered remote. This module only decides what
 * the issue says and how the result is read back, so every rule here mirrors
 * a refusal in `validate_issue_payload` — a draft this builds must never be
 * one the backend rejects.
 *
 * What crosses to GitHub is the task's title, description and acceptance
 * criteria. Never the saved brief (it names local repositories and carries
 * the agent preamble), never the checkout path, and never the task's labels:
 * `gh issue create --label X` fails outright when the repository does not
 * define `X`, and board labels such as `p1` or `inbox` almost never exist
 * there.
 */

import { outcomeUnknown } from "../async/deferral";
import { clipUtf8 } from "../terminal/agentPromptText";
import { explainError, putTask, taskDraft, taskWrite, type Task, type TaskCard } from "./client";
import { issueLinkLabel, linkedIssueNumber } from "./issueTask";
import { bounded, MAX_TASK_LABELS } from "./taskActions";

/** `validate_issue_payload`'s title limit, counted in code points. */
export const MAX_TASK_ISSUE_TITLE = 256;
/** Kept under the backend's 64 KiB body limit with room for the clip note. */
export const MAX_TASK_ISSUE_BODY_BYTES = 60 * 1024;
const CLIP_NOTE = "\n\n[GitPulse clipped this task to stay below GitHub's body limit.]";
const MARKER = "<!-- gitpulse:task-issue:v1 -->";

/** The backend refuses these in a title; they reorder how text renders. */
const BIDI_OVERRIDES = new Set([0x202a, 0x202b, 0x202c, 0x202d, 0x202e, 0x2066, 0x2067, 0x2068, 0x2069]);

export interface TaskIssueDraft {
  title: string;
  body: string;
  /** True when the body had to be cut to fit; the body itself says so too. */
  clipped: boolean;
}

/** Rust's `char::is_control`: the C0 and C1 blocks plus DEL. */
function isControl(code: number): boolean {
  return code < 0x20 || (code >= 0x7f && code <= 0x9f);
}

function cleanTitle(value: unknown): string {
  const text = typeof value === "string" ? value : "";
  const chars = Array.from(text, (char) => {
    const code = char.codePointAt(0) ?? 0;
    if (BIDI_OVERRIDES.has(code)) return "";
    return isControl(code) ? " " : char;
  });
  const collapsed = chars.join("").replace(/\s+/g, " ").trim();
  const points = Array.from(collapsed || "Untitled task");
  if (points.length <= MAX_TASK_ISSUE_TITLE) return points.join("");
  return `${points.slice(0, MAX_TASK_ISSUE_TITLE - 1).join("").trimEnd()}…`;
}

/** Newlines and tabs shape Markdown; every other control character is refused by the backend. */
function cleanBody(value: unknown): string {
  const text = typeof value === "string" ? value : "";
  return Array.from(text, (char) => {
    if (char === "\n" || char === "\t") return char;
    if (char === "\r") return "";
    return isControl(char.codePointAt(0) ?? 0) ? " " : char;
  }).join("");
}

/** One checklist line: a criterion is a single item, so its own newlines fold. */
function cleanLine(value: unknown): string {
  return cleanBody(value).replace(/\s+/g, " ").trim();
}

export function taskIssueDraft(task: Pick<Task, "title" | "description" | "acceptance_criteria">): TaskIssueDraft {
  const description = cleanBody(task.description).trim();
  const criteria = (Array.isArray(task.acceptance_criteria) ? task.acceptance_criteria : [])
    .map(cleanLine)
    .filter(Boolean);
  const sections = [MARKER, description || "_No description._"];
  if (criteria.length) {
    sections.push("", "## Acceptance criteria", "", ...criteria.map((line) => `- [ ] ${line}`));
  }
  sections.push("", "---", "_Filed from a GitPulse task._");
  const clipped = clipUtf8(sections.join("\n"), MAX_TASK_ISSUE_BODY_BYTES, CLIP_NOTE);
  return { title: cleanTitle(task.title), body: clipped.text, clipped: clipped.clipped };
}

/**
 * The issue number in what `gh issue create` printed, or null.
 *
 * gh prints the new issue's URL as its last line. Only that line is read, and
 * only an `/issues/<n>` path on an http(s) URL counts: a number found anywhere
 * else would link the task to an issue nobody created.
 */
export function issueNumberFromOutput(output: unknown): number | null {
  if (typeof output !== "string") return null;
  const last = output.trim().split(/\r?\n/).pop()?.trim() ?? "";
  const match = /^https?:\/\/[^\s/]+\/[^\s/]+\/[^\s/]+\/issues\/(\d{1,9})\/?$/.exec(last);
  const number = match ? Number(match[1]) : NaN;
  return Number.isSafeInteger(number) && number > 0 ? number : null;
}

/** The URL gh printed, when it is one; otherwise null so nothing offers to open it. */
export function issueUrlFromOutput(output: unknown): string | null {
  if (issueNumberFromOutput(output) === null || typeof output !== "string") return null;
  return output.trim().split(/\r?\n/).pop()?.trim() ?? null;
}

/**
 * The labels a task should carry once linked to issue `issueNumber`.
 *
 * Refuses rather than silently dropping the link when the task is already at
 * the label cap: `nextLabels` returns the list unchanged in that case, and the
 * write would then succeed without linking anything.
 */
export function linkedLabels(
  labels: readonly string[],
  issueNumber: number,
): { ok: true; labels: string[] } | { ok: false; reason: string } {
  const label = issueLinkLabel(issueNumber);
  if (labels.includes(label)) return { ok: true, labels: [...labels] };
  if (labels.length >= MAX_TASK_LABELS) {
    return { ok: false, reason: `the task already has ${MAX_TASK_LABELS} labels, the most it can hold` };
  }
  return { ok: true, labels: [...labels, label] };
}

/**
 * Write the link label onto the task the issue was filed from.
 *
 * Written against the revision the draft was read at, so a task edited while
 * the confirmation was open is refused by the store rather than overwritten.
 * Success is read off the saved record, not inferred from the write
 * resolving: the issue already exists either way, and the caller must be
 * able to say truthfully whether the task now points at it.
 */
export async function linkTaskToIssue(
  task: Task,
  issueNumber: number,
  io: { put: (input: Record<string, unknown>) => Promise<Task> } = { put: putTask },
): Promise<{ ok: true; task: Task } | { ok: false; reason: string }> {
  const next = linkedLabels(task.labels, issueNumber);
  if (!next.ok) return next;
  try {
    const saved = await bounded(io.put(taskWrite(task.id, task.revision, { ...taskDraft(task), labels: next.labels })));
    if (saved.id !== task.id || !saved.labels.includes(issueLinkLabel(issueNumber))) {
      return { ok: false, reason: "the saved task does not carry the link label" };
    }
    return { ok: true, task: saved };
  } catch (cause) {
    return { ok: false, reason: explainError(cause) };
  }
}

function checkoutPhrase(checkout: string, derived: boolean): string {
  return derived
    ? `${checkout} (inferred from the repository's git directory; not open in GitPulse)`
    : checkout;
}

const CARRIES =
  "The issue carries the task's title, description and acceptance criteria — not its labels, owner or local paths.";

/** The confirmation text: what will be published, and from which checkout's remote. */
export function taskIssueConfirmation(input: {
  draft: TaskIssueDraft;
  repositoryName: string;
  checkout: string;
  derived: boolean;
}): string {
  const clipped = input.draft.clipped ? "\n\nThe description is too long for an issue and will be clipped." : "";
  return (
    `${input.draft.title}\n\n` +
    `Create this issue on the GitHub remote of ${input.repositoryName}, read from ${checkoutPhrase(input.checkout, input.derived)}? ` +
    CARRIES +
    clipped
  );
}

// ---------------------------------------------------------------------------
// Several tasks at once
// ---------------------------------------------------------------------------

/**
 * Most tasks one run will file. Each creation is a `gh` process with a 90 s
 * deadline, so ten is already a long worst case to sit through, and a
 * selection bigger than this is more likely a slip than a plan.
 */
export const MAX_TASK_ISSUE_BATCH = 10;

/** Where a task's issue is filed: its repository, and the checkout whose remote names it. */
export interface TaskIssueTarget {
  repositoryName: string;
  checkout: string;
  derived: boolean;
}

/** A task read, drafted and ready to file. */
export interface PreparedTaskIssue {
  task: Task;
  target: TaskIssueTarget;
  draft: TaskIssueDraft;
}

/** A task the run will not file, and why — said in the confirmation and the summary. */
export interface SkippedTaskIssue {
  title: string;
  reason: string;
}

/**
 * Read and draft every task in the selection, setting aside those that
 * cannot or should not be filed.
 *
 * A task already linked to an issue is skipped, checked on the card and again
 * on the fresh read, so re-running a run that stopped part-way files only
 * what it has not filed yet. `resolve` answers where a task would be filed or
 * why it cannot be. Nothing here writes or publishes.
 */
export async function prepareTaskIssues(
  cards: readonly TaskCard[],
  io: {
    resolve: (card: TaskCard) => TaskIssueTarget | string;
    read: (id: string) => Promise<Task>;
    /**
     * The issue an earlier run created for this task but could not link.
     * Such a task carries no link label yet, so without this a re-run would
     * file it a second time.
     */
    unlinked?: (id: string) => number | null;
  },
): Promise<{ ready: PreparedTaskIssue[]; skipped: SkippedTaskIssue[] }> {
  const ready: PreparedTaskIssue[] = [];
  const skipped: SkippedTaskIssue[] = [];
  const seen = new Set<string>();
  for (const card of cards) {
    if (seen.has(card.id)) continue;
    seen.add(card.id);
    const onCard = linkedIssueNumber(card);
    if (onCard !== null) { skipped.push({ title: card.title, reason: `already linked to #${onCard}` }); continue; }
    const pending = io.unlinked?.(card.id) ?? null;
    if (pending !== null) { skipped.push({ title: card.title, reason: `issue #${pending} already exists for it; link it instead` }); continue; }
    const target = io.resolve(card);
    if (typeof target === "string") { skipped.push({ title: card.title, reason: target }); continue; }
    let task: Task;
    try {
      task = await bounded(io.read(card.id));
    } catch (cause) {
      skipped.push({ title: card.title, reason: `could not be read: ${explainError(cause)}` });
      continue;
    }
    const linked = linkedIssueNumber(task);
    if (linked !== null) { skipped.push({ title: task.title, reason: `already linked to #${linked}` }); continue; }
    ready.push({ task, target, draft: taskIssueDraft(task) });
  }
  return { ready, skipped };
}

/**
 * One confirmation for the whole run: every issue it will publish, grouped by
 * the repository whose remote receives it, and every task it will not file.
 * A run of exactly one task reads the way the single confirmation always did.
 */
export function taskIssuesConfirmation(ready: readonly PreparedTaskIssue[], skipped: readonly SkippedTaskIssue[]): string {
  if (ready.length === 1 && skipped.length === 0) {
    const [only] = ready;
    return taskIssueConfirmation({ draft: only.draft, ...only.target });
  }
  const groups = new Map<string, { target: TaskIssueTarget; titles: string[] }>();
  for (const entry of ready) {
    const key = `${entry.target.repositoryName}\u0000${entry.target.checkout}`;
    const group = groups.get(key) ?? { target: entry.target, titles: [] };
    group.titles.push(entry.draft.title);
    groups.set(key, group);
  }
  const lines = [ready.length === 1 ? "Create 1 issue on GitHub?" : `Create ${ready.length} issues on GitHub?`];
  for (const { target, titles } of groups.values()) {
    lines.push("", `${target.repositoryName} — remote read from ${checkoutPhrase(target.checkout, target.derived)}:`);
    for (const title of titles) lines.push(`• ${title}`);
  }
  if (skipped.length) {
    lines.push("", `Not filed (${skipped.length}):`);
    for (const entry of skipped) lines.push(`• ${entry.title} — ${entry.reason}`);
  }
  const clipped = ready.filter((entry) => entry.draft.clipped).length;
  lines.push(
    "",
    CARRIES,
    "Issues are created one at a time, and the run stops at the first one that does not go through cleanly.",
  );
  if (clipped) lines.push(`${clipped === 1 ? "One description is" : `${clipped} descriptions are`} too long for an issue and will be clipped.`);
  return lines.join("\n");
}

/**
 * What became of one task.
 *
 * `unknown` is a creation whose `gh` process hit its deadline: the issue may
 * exist. It is never retried by this module and never reported as "not
 * created", because either would invite a duplicate.
 */
export type TaskIssueResult =
  | { state: "linked"; title: string; number: number; url: string | null; task: Task }
  | { state: "unlinked"; title: string; taskId: string; number: number | null; url: string | null; reason: string }
  | { state: "refused"; title: string; reason: string }
  | { state: "unknown"; title: string; issueTitle: string; reason: string }
  | { state: "not_attempted"; title: string };

export interface TaskIssueRunIO {
  /**
   * The guarded creation. Deliberately not wrapped in a frontend timeout here:
   * one shorter than gh's own deadline would fire first, carry no timeout
   * marker, and call an issue that may exist "refused".
   */
  create: (entry: PreparedTaskIssue) => Promise<{ ok: boolean; error?: string; output?: string }>;
  link?: typeof linkTaskToIssue;
  /** Checked before each creation; a creation already running finishes first. */
  stopped?: () => boolean;
  progress?: (index: number, total: number, title: string) => void;
}

/**
 * File the prepared tasks in order, linking each as it is created.
 *
 * Stops at the first task that does not end linked — refused, unknown, or
 * created but not linked — and reports the rest as not attempted. Continuing
 * past an anomaly would multiply it (an unauthenticated `gh` refuses every
 * task the same way) or bury it; stopping keeps every outcome attributable,
 * and because linked tasks are skipped, re-running the same selection resumes
 * from where this run stopped.
 */
export async function runTaskIssues(entries: readonly PreparedTaskIssue[], io: TaskIssueRunIO): Promise<TaskIssueResult[]> {
  const link = io.link ?? linkTaskToIssue;
  const results: TaskIssueResult[] = [];
  let halted = false;
  for (const [index, entry] of entries.entries()) {
    const title = entry.task.title;
    if (halted || io.stopped?.()) { halted = true; results.push({ state: "not_attempted", title }); continue; }
    io.progress?.(index + 1, entries.length, title);
    let outcome: { ok: boolean; error?: string; output?: string };
    try {
      outcome = await io.create(entry);
    } catch (cause) {
      outcome = { ok: false, error: explainError(cause) };
    }
    if (!outcome.ok) {
      const reason = outcome.error?.trim() || "GitHub did not say why.";
      results.push(outcomeUnknown(reason)
        ? { state: "unknown", title, issueTitle: entry.draft.title, reason }
        : { state: "refused", title, reason });
      halted = true;
      continue;
    }
    const number = issueNumberFromOutput(outcome.output);
    const url = issueUrlFromOutput(outcome.output);
    if (number === null) {
      results.push({ state: "unlinked", title, taskId: entry.task.id, number: null, url, reason: "GitHub's reply did not name the new issue's number" });
      halted = true;
      continue;
    }
    const linked = await link(entry.task, number);
    if (!linked.ok) {
      results.push({ state: "unlinked", title, taskId: entry.task.id, number, url, reason: linked.reason });
      halted = true;
      continue;
    }
    results.push({ state: "linked", title, number, url, task: linked.task });
  }
  return results;
}

/** How a run is reported: a success line when everything landed, otherwise the error to show. */
export function summarizeTaskIssues(
  results: readonly TaskIssueResult[],
  skipped: readonly SkippedTaskIssue[],
  stoppedByReader: boolean,
): { ok: boolean; message: string } {
  const parts: string[] = [];
  const linked = results.filter((r): r is Extract<TaskIssueResult, { state: "linked" }> => r.state === "linked");
  if (linked.length === 1) parts.push(`Created issue #${linked[0].number} and linked the task to it.`);
  else if (linked.length > 1) parts.push(`Created and linked ${linked.length} issues (${linked.map((r) => `#${r.number}`).join(", ")}).`);
  for (const r of results) {
    if (r.state === "unlinked") {
      parts.push(r.number === null
        ? `An issue was created for “${r.title}”, but ${r.reason}, so the task is not linked — check GitHub before filing it again.`
        : `Created issue #${r.number} for “${r.title}”, but the task is not linked: ${r.reason}.`);
    } else if (r.state === "refused") {
      parts.push(`Not created — “${r.title}”: ${r.reason}`);
    } else if (r.state === "unknown") {
      parts.push(`Could not confirm whether “${r.title}” was filed (${r.reason}). Check GitHub for an issue titled “${r.issueTitle}” before filing it again.`);
    }
  }
  const pending = results.filter((r) => r.state === "not_attempted").length;
  if (pending) {
    parts.push(stoppedByReader
      ? `Stopped as asked; ${pending === 1 ? "1 task was" : `${pending} tasks were`} not filed.`
      : `Stopped there; ${pending === 1 ? "1 more task was" : `${pending} more tasks were`} not filed.`);
  }
  if (skipped.length) {
    parts.push(`Skipped ${skipped.length === 1 ? "1 task" : `${skipped.length} tasks`}: ${skipped.map((s) => `“${s.title}” (${s.reason})`).join("; ")}.`);
  }
  const ok = results.length > 0 && linked.length === results.length;
  return { ok, message: parts.join(" ") || "No task was filed." };
}
