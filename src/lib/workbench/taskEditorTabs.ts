/**
 * Which panes the task sheet offers, and when.
 *
 * The sheet used to be one scroll: an AI block, then type and status, then a
 * disclosure holding six scheduling fields, then a repository checklist, then
 * notifications, then a second disclosure containing a single sentence, then
 * the whole agent-run panel. Everything a task could ever need was always on
 * screen, so nothing on it read as important.
 *
 * Splitting it needs one rule to stay honest: **a draft is never tabbed.**
 * A reader creating a task has one thing to say and should not have to find
 * the pane to say it in — and the panes a draft would show are all empty
 * anyway, because a task with no id has no runs, no suggestion history and
 * nothing to notify about. `editorTabs` returns a single pane for a draft,
 * which the sheet renders with no tab strip at all.
 */

export type TaskEditorTab = "task" | "organize" | "agent" | "ai";

export interface TaskEditorTabDescriptor {
  id: TaskEditorTab;
  label: string;
  /** Short line under the tab strip, saying what the pane is for. */
  hint: string;
}

const ALL: readonly TaskEditorTabDescriptor[] = [
  { id: "task", label: "Task", hint: "What the work is, and which repositories it touches." },
  { id: "organize", label: "Organize", hint: "Priority, schedule, owner, labels and notifications." },
  { id: "agent", label: "Agent", hint: "Hand this saved revision to a coding agent." },
  { id: "ai", label: "AI", hint: "Draft or improve the title and description." },
];

/**
 * Panes for this sheet.
 *
 * `saved` is the only input that matters: an unsaved draft gets the Task pane
 * alone. Nothing else here is conditional, because a pane that appears and
 * disappears as fields are filled in is worse than one that is simply empty.
 */
export function editorTabs(saved: boolean): TaskEditorTabDescriptor[] {
  return saved ? [...ALL] : [ALL[0]];
}

export function isTaskEditorTab(value: unknown): value is TaskEditorTab {
  return ALL.some((tab) => tab.id === value);
}

/**
 * The pane to show, given what the sheet last had open.
 *
 * Saving a draft grows the strip from one pane to four; the reader stays on
 * Task rather than being moved. A pane that is no longer offered (Agent on a
 * sheet that was reloaded into a draft) falls back to Task instead of
 * rendering nothing.
 */
export function resolveEditorTab(current: unknown, saved: boolean): TaskEditorTab {
  const tabs = editorTabs(saved);
  return isTaskEditorTab(current) && tabs.some((tab) => tab.id === current) ? current : "task";
}

export function editorTabHint(tab: TaskEditorTab): string {
  return ALL.find((entry) => entry.id === tab)?.hint ?? "";
}

/**
 * A count beside a tab label, or 0 for none.
 *
 * Only counts things that already exist, never work the reader might do:
 * runs on Agent, suggestions on AI. Organize deliberately has no badge —
 * "4 fields set" is noise, not information.
 */
export function editorTabBadge(
  tab: TaskEditorTab,
  counts: { runs?: number; suggestions?: number },
): number {
  const value = tab === "agent" ? counts.runs : tab === "ai" ? counts.suggestions : 0;
  return typeof value === "number" && Number.isFinite(value) && value > 0 ? Math.min(Math.floor(value), 99) : 0;
}
