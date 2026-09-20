import { ENHANCEMENT_STATE_LABELS } from "./taskEnhance";

/**
 * What the Task pane puts on screen, and in what order.
 *
 * A draft is a compose surface: notes and the model sit above the fields they
 * fill, because that is the sitting that *creates* a task. A saved task is a
 * record the reader came to edit. Putting the same compose chrome second on
 * that sheet — labelled "Quick add", expanded, with an accepted suggestion
 * filling the viewport — is the layout this module exists to refuse.
 *
 * Visual order is owned here rather than by the Svelte file. The sheet iterates
 * what this returns; tests that care about "title before assist on a saved
 * task" assert this table, not source order of two tags.
 */

export const EDITOR_SECTION_IDS = [
  "repositories",
  "assist",
  "title",
  "logs",
  "schedule",
  "owner",
] as const;

export type EditorSectionId = (typeof EDITOR_SECTION_IDS)[number];

export interface EditorSection {
  id: EditorSectionId;
  /** 1-based, matching the number drawn in the step heading. */
  n: number;
  heading: string;
}

export interface AssistDisclosure {
  /** Saved tasks can fold the assist; a draft cannot. */
  collapsible: boolean;
  /** Whether the assist body is shown. Always true for a draft. */
  open: boolean;
}

/** Compose heading on a new task. Not "Quick add": that name is the board's one-line creator. */
export const ASSIST_HEADING_DRAFT = "Notes";

/** Secondary heading on a saved task: improve wording, do not compose a second card. */
export const ASSIST_HEADING_SAVED = "Improve with model";

const HEADINGS: Readonly<Record<EditorSectionId, string>> = Object.freeze({
  repositories: "Repositories",
  assist: ASSIST_HEADING_DRAFT,
  title: "Title and description",
  logs: "Raw logs",
  schedule: "Status and scheduling",
  owner: "Owner",
});

/**
 * Draft order: dictate, then read what landed.
 *
 * Saved order: the task itself first. Assist follows the title as a folded
 * heading so a live suggestion is still one scroll away, but an accepted
 * review is no longer the first thing a reader of a revision-7 card sees.
 */
const DRAFT_ORDER: readonly EditorSectionId[] = Object.freeze([
  "repositories",
  "assist",
  "title",
  "logs",
  "schedule",
  "owner",
]);

const SAVED_ORDER: readonly EditorSectionId[] = Object.freeze([
  "repositories",
  "title",
  "assist",
  "logs",
  "schedule",
  "owner",
]);

function headingFor(id: EditorSectionId, saved: boolean): string {
  if (id === "assist") return saved ? ASSIST_HEADING_SAVED : ASSIST_HEADING_DRAFT;
  return HEADINGS[id];
}

/**
 * Numbered sections for this sheet.
 *
 * Only `true` is saved. `"yes"`, `1`, and `{}` fall through to the draft
 * order: a new task that mis-reports its state should still show notes, not
 * hide them behind a fold the reader has not earned.
 */
export function editorSections(saved: unknown): EditorSection[] {
  const isSaved = saved === true;
  const order = isSaved ? SAVED_ORDER : DRAFT_ORDER;
  return order.map((id, index) => ({
    id,
    n: index + 1,
    heading: headingFor(id, isSaved),
  }));
}

/**
 * Proposal states that are work the reader should see without hunting for Show.
 *
 * `accepted` is history, not work: that is the revision-7 screenshot. `ready`
 * is listed here so a caller that only has the store state, and not the sheet's
 * `reviewable` flag, still unfolds. Hostile strings are ignored by membership,
 * not by coercing them into this set.
 */
const ASSIST_WORK_STATES: ReadonlySet<string> = new Set([
  "ready",
  "failed",
  "interrupted",
  "pending",
  "running",
  "cancel_requested",
]);

/**
 * Whether the assist has work the reader should see without asking.
 *
 * Notes that are only whitespace are not work: they are the empty textarea.
 * `busy` and `reviewable` are the sheet's own flags (generation in flight, a
 * suggestion waiting to be accepted). `state` is the proposal's store state;
 * `uncertain` is a lost reply or `interrupted` outcome. Hostile non-booleans
 * are ignored rather than treated as "something is happening".
 */
export function assistAttention(input: {
  notes?: unknown;
  busy?: unknown;
  reviewable?: unknown;
  state?: unknown;
  uncertain?: unknown;
}): boolean {
  const notes = typeof input.notes === "string" ? input.notes.trim() : "";
  if (notes.length > 0) return true;
  if (input.busy === true || input.reviewable === true || input.uncertain === true) return true;
  return typeof input.state === "string" && ASSIST_WORK_STATES.has(input.state);
}

/**
 * One line on a folded saved-task assist: remaining work, or accepted history.
 *
 * Copy comes from `ENHANCEMENT_STATE_LABELS` so this heading cannot disagree
 * with the review that opens underneath it. Empty string means mute — just Show.
 */
export function assistFoldStatus(input: { state?: unknown; uncertain?: unknown }): string {
  if (input.uncertain === true && input.state !== "ready" && input.state !== "accepted") {
    return ENHANCEMENT_STATE_LABELS.interrupted;
  }
  const state = typeof input.state === "string" ? input.state : "";
  if (state === "failed") return ENHANCEMENT_STATE_LABELS.failed;
  if (state === "interrupted") return ENHANCEMENT_STATE_LABELS.interrupted;
  if (state === "ready") return ENHANCEMENT_STATE_LABELS.ready;
  if (state === "running" || state === "cancel_requested") return ENHANCEMENT_STATE_LABELS.running;
  if (state === "pending") return ENHANCEMENT_STATE_LABELS.pending;
  if (state === "accepted") return ENHANCEMENT_STATE_LABELS.accepted;
  return "";
}

/**
 * Empty logs on a saved task are not primary chrome.
 *
 * A draft always shows the paste surface: that is how a dump gets onto a new
 * task. A saved task with no logs folds it behind Show; a saved task that
 * already has logs keeps them open and is not collapsible.
 */
export function logsDisclosure(input: {
  saved?: unknown;
  hasLogs?: unknown;
  opened?: unknown;
}): AssistDisclosure {
  if (input.saved !== true) return { collapsible: false, open: true };
  if (input.hasLogs === true) return { collapsible: false, open: true };
  return { collapsible: true, open: input.opened === true };
}

/**
 * Fold state for the assist section.
 *
 * A draft is never collapsible and never closed: hiding the notes box on a
 * task that does not exist yet is how a reader loses the one field they came
 * to fill. A saved task starts closed (`opened` false) and stays closed until
 * the sheet latches `opened` true — either because the reader asked, or
 * because `riseAssistOpen` saw attention arrive.
 */
export function assistDisclosure(input: { saved?: unknown; opened?: unknown }): AssistDisclosure {
  if (input.saved !== true) return { collapsible: false, open: true };
  return { collapsible: true, open: input.opened === true };
}

/**
 * Latch: attention arriving opens the assist; attention leaving does not close it.
 *
 * Closing after accept would snatch the undo control away. Re-opening on every
 * reactive tick while attention is true would fight a reader who hid a long
 * review to look at the logs. Only the false→true edge forces open.
 */
export function riseAssistOpen(prevAttention: unknown, attention: unknown, opened: unknown): boolean {
  const was = prevAttention === true;
  const now = attention === true;
  if (now && !was) return true;
  return opened === true;
}
