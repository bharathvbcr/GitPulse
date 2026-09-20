/**
 * Which panes the task sheet offers, and when.
 *
 * The sheet used to be one scroll: an AI block, then type and status, then a
 * disclosure holding six scheduling fields, then a repository checklist, then
 * notifications, then a second disclosure containing a single sentence, then
 * the whole agent-run panel. Everything a task could ever need was always on
 * screen, so nothing on it read as important.
 *
 * Splitting it into four panes fixed that and introduced a worse problem: the
 * AI pane's *output* was drawn on the Task pane, beside the fields each
 * suggestion would replace. A reader pressed a button on one pane and the
 * result appeared on another. Writing a task, scheduling it and asking the
 * model to improve the wording are one sitting, so they are now one pane, laid
 * out in two columns rather than stacked behind tabs.
 *
 * What is left is the one split that earns a tab: **Agent hands the saved
 * revision to something that will act on it.** That is a different decision
 * from writing the task, with its own risk and its own run history.
 *
 * One rule survives the merge: **a draft is never tabbed.** A reader creating a
 * task has one thing to say and should not have to find the pane to say it in —
 * and the pane a draft would show is empty anyway, because a task with no id
 * has no runs. `editorTabs` returns a single pane for a draft, which the sheet
 * renders with no tab strip at all.
 */

export type TaskEditorTab = "task" | "agent";

export interface TaskEditorTabDescriptor {
  id: TaskEditorTab;
  label: string;
  /** Short line under the tab strip, saying what the pane is for. */
  hint: string;
}

export const TASK_TAB_HINT_DRAFT = "What the work is, how it is scheduled, and the model's help with it.";
export const TASK_TAB_HINT_SAVED = "What the work is and how it is scheduled.";
export const AGENT_TAB_HINT = "Hand this saved revision to a coding agent.";

const ALL_SAVED: readonly TaskEditorTabDescriptor[] = Object.freeze([
  { id: "task", label: "Task", hint: TASK_TAB_HINT_SAVED },
  { id: "agent", label: "Agent", hint: AGENT_TAB_HINT },
]);

const DRAFT_TAB: readonly TaskEditorTabDescriptor[] = Object.freeze([
  { id: "task", label: "Task", hint: TASK_TAB_HINT_DRAFT },
]);

/**
 * Panes that were folded into Task, and still answer to their old names.
 *
 * `resolveEditorTab` already falls back to Task for anything it does not
 * recognize, so this map changes no outcome today. It exists because the two
 * cases are not the same fact: "Organize is part of Task now" is a rename the
 * sheet should honour deliberately, while "this is not a pane" is a repair of a
 * value that should never have been stored. Collapsing them would leave the
 * fallback carrying a meaning nothing states, and the first pane added back
 * would silently inherit every stale id.
 *
 * Null-prototype so a stored `"constructor"` resolves to a pane rather than to
 * a function — the same guard `taskQuickAdd`'s marker table uses.
 */
const MERGED_INTO: Record<string, TaskEditorTab> = Object.assign(Object.create(null), {
  organize: "task",
  ai: "task",
});

/**
 * Panes for this sheet.
 *
 * `saved` is the only input that matters: an unsaved draft gets the Task pane
 * alone. Nothing else here is conditional, because a pane that appears and
 * disappears as fields are filled in is worse than one that is simply empty.
 */
export function editorTabs(saved: boolean): TaskEditorTabDescriptor[] {
  return saved ? [...ALL_SAVED] : [...DRAFT_TAB];
}

export function isTaskEditorTab(value: unknown): value is TaskEditorTab {
  return ALL_SAVED.some((tab) => tab.id === value);
}

/**
 * The pane to show, given what the sheet last had open.
 *
 * Saving a draft grows the strip from one pane to two; the reader stays on
 * Task rather than being moved. A pane that is no longer offered (Agent on a
 * sheet that was reloaded into a draft) falls back to Task instead of
 * rendering nothing, and so does a pane this build has merged away.
 */
export function resolveEditorTab(current: unknown, saved: boolean): TaskEditorTab {
  const offered = new Set(editorTabs(saved).map((tab) => tab.id));
  if (isTaskEditorTab(current) && offered.has(current)) return current;
  if (typeof current === "string") {
    const merged = MERGED_INTO[current];
    if (merged && offered.has(merged)) return merged;
  }
  return "task";
}

export function editorTabHint(tab: TaskEditorTab, saved: boolean = true): string {
  if (tab === "task") return saved ? TASK_TAB_HINT_SAVED : TASK_TAB_HINT_DRAFT;
  if (tab === "agent") return AGENT_TAB_HINT;
  return "";
}

/**
 * A count beside a tab label, or 0 for none.
 *
 * Only counts things that already exist, never work the reader might do. Runs
 * on Agent is the whole list: suggestions used to badge the AI tab, and a
 * badge on the pane the reader is already looking at is noise, not
 * information — the assist's own heading says how many it has.
 */
export function editorTabBadge(tab: TaskEditorTab, counts: { runs?: number }): number {
  const value = tab === "agent" ? counts.runs : 0;
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.min(Math.floor(value), 99) : 0;
}
