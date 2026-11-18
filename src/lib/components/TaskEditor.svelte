<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { bounded } from "../workbench/taskActions";
  import { DEFAULT_PRIORITY, PRIORITY_LABELS } from "../workbench/taskPriority";
  import { ChevronDown, ChevronUp, Clipboard, X } from "@lucide/svelte";
  import { copyText } from "../desktop/clipboard";
  import TaskAgentPanel from "./TaskAgentPanel.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskManviAssist from "./TaskManviAssist.svelte";
  import TaskVerification from "./TaskVerification.svelte";
  import TaskRefine from "./TaskRefine.svelte";
  import { repoStore } from "../stores/repoStore";
  import TaskRepositoryPicker from "./TaskRepositoryPicker.svelte";
  import TaskDuePicker from "./TaskDuePicker.svelte";
  import LabelInput from "./LabelInput.svelte";
  import TaskRelations from "./TaskRelations.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { askConfirm } from "../stores/modalStore";
  import { deleteTask, explainError, freshTaskFields, getTask, getTaskBrief, getWorkspace, newID, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type TaskStatus, type WorkspaceCard } from "../workbench/client";
  import { deleteAttempt, deleteConfirmCopy, isRetryableDelete } from "../workbench/taskDelete";
  import { addableOpenTabs, attachRepositories, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  import { linkSummary, repositoryRows, shouldOfferFilter, triggerChips } from "../workbench/taskRepositories";
  import { appendCriteria, canAskManvi, composeFromNotes, describeChecklistMove, describeRefusedCriteria, criteriaFromPaste, extractDraftSubtasks, formatDraftAgentCopy, MAX_SUBTASKS, SUBTASK_CAP, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  import { sanitizeLogs } from "../workbench/taskLogs";
  import { assistEngineName, DEFAULT_ASSIST_ENGINE } from "../workbench/taskEnhance";
  import { editorTabBadge, editorTabHint, editorTabs, resolveEditorTab, type TaskEditorTab } from "../workbench/taskEditorTabs";
  import { assistAttention, assistDisclosure, assistFoldStatus, editorSections, logsDisclosure, riseAssistOpen } from "../workbench/taskEditorLayout";
  import { handleTablistKeydown, focusTabAt, panelId, tabId, tabProps } from "../dom/tablist";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { LAYERS } from "../ui/layers";
  import { scrollEdgeFade } from "../ui/scrollEdgeFade";
  import { liquidSelection } from "../ui/transitions";

  // Same material as every other section strip in the app: the selected pane
  // carries one glass pill that crossfades between tabs on macOS.
  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  const KIND_OPTIONS = ["issue", "bug", "feature", "improvement", "maintenance", "research", "documentation"] as const;
  const SEVERITY_OPTIONS = [
    { value: "low", label: "Low" },
    { value: "medium", label: "Medium" },
    { value: "high", label: "High" },
    { value: "critical", label: "Critical" },
  ] as const;

  let { value, seed = null, repositories, workspaces, openTabs = [], primary = "", home = null, active = true, initialStatus = "backlog", onSaved, onClose }: {
    value: Task | null; seed?: Partial<TaskDraft> | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; active?: boolean; initialStatus?: TaskStatus;
    onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, seed, primary, home, initialStatus }));
  let current = $state(initial.value);
  let draft = $state<TaskDraft>((() => {
    const base = initial.value ? taskDraft(initial.value) : {
      title: "", description: "", kind: "feature", status: initial.initialStatus, priority: DEFAULT_PRIORITY, severity: null,
      owner: null, due_at: null, labels: [], acceptance_criteria: [], repository_ids: initial.primary ? [initial.primary] : [],
      primary_repository_id: initial.primary, home_workspace_id: initial.home, position: Date.now(), locked_fields: [] as EnhancementField[],
      logs: "", ...freshTaskFields(),
      ...initial.seed,
    };
    return { ...base, locked_fields: base.locked_fields ?? [], logs: base.logs ?? "" };
  })());
  let criteria = $state(untrack(() => draft.acceptance_criteria.join("\n")));
  let newSubtask = $state("");
  const criteriaList = $derived(criteria.split("\n").map((s) => s.trim()).filter(Boolean));
  /**
   * The same rows as `criteriaList`, untrimmed, for the inline editors: a row
   * redrawn from the trimmed list would eat the space a reader just typed
   * between two words. Both filter on the trimmed text, so indexes agree.
   */
  const criteriaRows = $derived(criteria.split("\n").filter((s) => s.trim()));
  /** Off by default: one list to edit, with the raw lines a toggle away. */
  let rawCriteria = $state(false);
  let labelChips = $state<string[]>(untrack(() => [...draft.labels]));
  let error = $state("");
  let note = $state("");
  let saving = $state(false);
  let pending = $state<Record<string, unknown> | null>(null);
  let pendingDelete = $state<{ id: string; request_id: string; expected_revision: number } | null>(null);
  let dirty = $state(false);
  let enhancementBusy = $state(false);
  /** Unsaved edits to a suggestion; also part of `enhancementBusy` (see `onEditing`). */
  let suggestionEditing = $state(false);
  /** Busy for a reason only waiting can end: a model run or an unconfirmed result. */
  const enhancementWorking = $derived(enhancementBusy && !suggestionEditing);
  let copying = $state(false);
  let extras = $state<Repository[]>([]);
  let adding = $state(false);
  let repoFilter = $state("");
  let attaching = $state(false);
  /**
   * Member repositories of the task's home workspace.
   *
   * `null` means no home workspace, or a membership this sheet could not
   * read — never "a member of nothing". The picker marks members only when
   * this is a list, so an unread membership is never drawn as an empty
   * workspace, and `homeError` says which of the two happened.
   */
  let homeMembers = $state<string[] | null>(null);
  let homeError = $state("");
  let homeToken = 0;
  let notes = $state("");
  let logNotice = $state("");
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let confirming = $state(false);
  let reloading = $state(false);
  let shortcutBlocked = $state("");
  let customKind = $state(false);
  let sheet: HTMLElement;
  /** The checklist, links and subtasks section, which holds a typed-but-unadded checklist item. */
  let relations: ReturnType<typeof TaskRelations> | undefined = $state();
  let tabStrip: HTMLDivElement | undefined = $state();
  /**
   * The pane on screen.
   *
   * A draft has one pane and no strip; saving grows the strip to two and
   * leaves the reader on Task. `resolveEditorTab` is what makes that safe —
   * it refuses a pane this sheet does not offer instead of rendering nothing,
   * and sends a merged-away pane to the one that absorbed it.
   */
  let requestedTab = $state<TaskEditorTab>("task");
  /**
   * Fields a just-accepted suggestion rewrote, so they can flash.
   *
   * The only thing that crosses back from the assist. Accepting itself lives
   * entirely inside the assist's review, beside the diff that shows what would
   * change — the sheet used to draw a second "Use this title" button under
   * each field, which left one decision with two owners.
   */
  let flash = $state<EnhancementField[]>([]);
  /** A suggestion is waiting on the Task pane; the tab dot is drawn from this. */
  let reviewable = $state(false);
  let workingCount = $state(0);
  /** Of those, the ones asking for the reader; the badge turns to say so. */
  let askingCount = $state(0);
  /** Display name of the engine the assist section would use; it owns the picker. */
  let assistName = $state(assistEngineName(DEFAULT_ASSIST_ENGINE));
  const tabs = $derived(editorTabs(Boolean(current)));
  const tab = $derived(resolveEditorTab(requestedTab, Boolean(current)));
  const saved = $derived(Boolean(current));
  /** Why Save is disabled, said beside it rather than left for the reader to guess. */
  const saveBlocker = $derived(!draft.repository_ids.length ? "Link a repository to save."
    : !draft.title.trim() && !notes.trim() ? "Add a title, or paste notes above."
    : !draft.kind.trim() ? "Choose a type under Details." : "");
  const sections = $derived(editorSections(saved));
  let assistState = $state<string | null>(null);
  let assistUncertain = $state(false);
  const hasAssistWork = $derived(assistAttention({
    notes,
    busy: enhancementBusy,
    reviewable,
    state: assistState,
    uncertain: assistUncertain,
  }));
  let assistOpened = $state(false);
  let prevAssistWork = false;
  $effect(() => {
    const work = hasAssistWork;
    untrack(() => {
      assistOpened = riseAssistOpen(prevAssistWork, work, assistOpened);
      prevAssistWork = work;
    });
  });
  const assistChrome = $derived(assistDisclosure({ saved, opened: assistOpened }));
  // Folded notes are still written by Save, so the fold says so.
  const assistStatus = $derived(!assistChrome.open
    ? notes.trim() ? "Notes not applied yet: Save adds them" : assistFoldStatus({ state: assistState, uncertain: assistUncertain })
    : "");
  const hasLogs = $derived(Boolean((draft.logs ?? "").trim()));
  let logsOpened = $state(false);
  const logsChrome = $derived(logsDisclosure({ saved, hasLogs, opened: logsOpened }));
  let disposed = false;
  const kindIsCustom = $derived(!KIND_OPTIONS.includes(draft.kind as typeof KIND_OPTIONS[number]));
  $effect(() => { if (kindIsCustom) customKind = true; });

  onMount(() => {
    const opener = document.activeElement;
    // A draft lands on its title too. The notes box used to take the cursor,
    // which scrolled the repository picker — the one control a save requires —
    // off the top of the sheet before the reader had seen it.
    sheet.querySelector<HTMLInputElement>('input[name="task-title"]')?.focus();
    return () => { if (opener instanceof HTMLElement && opener.isConnected) opener.focus(); };
  });
  onDestroy(() => { disposed = true; if (copiedTimer) clearTimeout(copiedTimer); if (flashTimer) clearTimeout(flashTimer); });
  const copyable = $derived(Boolean(current || draft.title.trim() || draft.description.trim() || notes.trim() || (draft.logs ?? "").trim()));
  const id = initial.value?.id ?? newID();
  const group = `task-sheet-${id}`;
  const pathOpts = { caseInsensitive: isCaseInsensitiveFs() };
  const known = $derived.by(() => {
    const map = new Map(repositories.map((repo) => [repo.id, repo]));
    for (const extra of extras) map.set(extra.id, extra);
    return [...map.values()];
  });
  const addable = $derived(addableOpenTabs(openMembershipCandidates(openTabs, known, draft.repository_ids, pathOpts)));
  const homeWorkspaceName = $derived(
    draft.home_workspace_id
      ? workspaces.find((space) => space.id === draft.home_workspace_id)?.name ?? "this workspace"
      : "",
  );
  const rows = $derived(repositoryRows(known, draft.repository_ids, draft.primary_repository_id, homeMembers, repoFilter));
  const summary = $derived(linkSummary(known, draft.repository_ids, draft.primary_repository_id, homeMembers));
  const trigger = $derived(triggerChips(known, draft.repository_ids, draft.primary_repository_id));
  const offerFilter = $derived(shouldOfferFilter(known.length));
  // Membership follows the draft's home workspace, not the board's scope: the
  // Home workspace control can move a task while the sheet is open.
  $effect(() => {
    const workspaceId = draft.home_workspace_id;
    const ticket = ++homeToken;
    homeMembers = null;
    homeError = "";
    if (!workspaceId) return;
    void getWorkspace(workspaceId)
      .then((full) => { if (!disposed && ticket === homeToken) homeMembers = full.repository_ids; })
      .catch((cause) => { if (!disposed && ticket === homeToken) homeError = explainError(cause); });
  });
  async function attachOutsiders() {
    const workspaceId = draft.home_workspace_id;
    if (!workspaceId || attaching || adding || saving || !summary.outsiders.length) return;
    attaching = true; error = "";
    try {
      const saved = await attachRepositories(workspaceId, summary.outsiders);
      if (disposed) return;
      homeMembers = saved.repository_ids;
      homeError = "";
      note = `Added to ${homeWorkspaceName}`;
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) attaching = false; }
  }
  function setPrimary(repositoryId: string) {
    if (!draft.repository_ids.includes(repositoryId)) return;
    draft.primary_repository_id = repositoryId;
    dirty = true;
  }
  async function addOpenPaths(paths: string[]) {
    if (adding || pending !== null || paths.length === 0) return;
    adding = true; error = "";
    try {
      for (const path of paths) {
        const repo = await registerRepository(path);
        if (disposed) return;
        if (!extras.some((item) => item.id === repo.id)) extras = [...extras, repo];
        membership(repo.id, true);
      }
    } catch (cause) { error = explainError(cause); }
    finally { adding = false; }
  }
  /**
   * Whether the task may be left, asking first when leaving would lose edits.
   *
   * Every board route that replaces this sheet comes through here. Edits to
   * a suggestion count as edits: they used to make this answer a silent
   * `false` (they hold `enhancementBusy`), so the board ignored the click and
   * nothing on screen said why. A model run still refuses — waiting is the
   * only way out — but it now says so.
   */
  export async function canLeave(): Promise<boolean> {
    if (saving || adding || reloading || confirming) return false;
    if (enhancementWorking) { shortcutBlocked = `Wait for ${assistName} to finish before leaving this task.`; return false; }
    if (pending || pendingDelete) { error = "Try the pending action again before closing this task."; return false; }
    // A criterion or checklist item typed but not added is work too: Save keeps
    // it, so leaving must not drop it without asking.
    const edited = dirty || Boolean(notes.trim()) || Boolean(newSubtask.trim()) || Boolean(relations?.hasPending());
    if (!edited && !suggestionEditing) return true;
    confirming = true;
    try {
      return await askConfirm(suggestionEditing && !edited
        ? {title:"Discard suggestion edits?",message:`Your edits to the ${assistName} suggestion have not been saved and will be lost.`,confirmLabel:"Discard edits",cancelLabel:"Keep editing",destructive:true}
        : {title:"Discard task edits?",message:suggestionEditing ? `Your unsaved changes, including your edits to the ${assistName} suggestion, will be lost.` : "Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing",destructive:true});
    }
    finally { confirming = false; }
  }
  async function close() {
    if (await canLeave()) onClose();
  }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  /**
   * What the notes would write, drawn live under the notes box.
   *
   * The save calls the same `composeFromNotes`, so the preview cannot promise
   * a title, a description or criteria the save does not then write.
   */
  const notesPreview = $derived(notes.trim()
    ? composeFromNotes({ title: draft.title, description: draft.description, acceptance_criteria: criteriaList }, notes)
    : null);
  /** Whether the description holds a checklist; the Extract button is drawn only then. */
  const checklistInText = $derived(extractDraftSubtasks({ description: draft.description, acceptance_criteria: criteriaList }));
  function applyExtractedNotes(): boolean {
    const next = composeFromNotes({ title: draft.title, description: draft.description, acceptance_criteria: criteriaList }, notes);
    if (!next.extracted) return true;
    if (new TextEncoder().encode(next.description).length > 65_536) {
      error = "Description is too long. Keep it below 64 KB.";
      return false;
    }
    draft.title = next.title;
    draft.description = next.description;
    if (next.addedCriteria.length) criteria = next.acceptance_criteria.join("\n");
    notes = next.notes;
    dirty = true;
    return true;
  }
  let flashTimer: ReturnType<typeof setTimeout> | undefined;
  /** Flash fields once; cleared afterwards so the next fill can flash them again. */
  function flashFields(fields: EnhancementField[]) {
    flash = fields;
    if (flashTimer) clearTimeout(flashTimer);
    flashTimer = setTimeout(() => { flash = []; }, 1600);
  }
  type MoveSnapshot = { title: string; description: string; criteria: string; notes: string };
  const currentSnapshot = $derived<MoveSnapshot>({ title: draft.title, description: draft.description, criteria, notes });
  /**
   * The last text move (Fill from notes, or Extract), so one click puts it back.
   *
   * `after` is what the move left. Undo is offered only while the four fields
   * still read exactly that: an edit made since, a save or a refresh would
   * otherwise be overwritten by a snapshot from before it.
   */
  let lastMove = $state<{ what: string; before: MoveSnapshot; after: MoveSnapshot } | null>(null);
  const canUndoMove = $derived(lastMove !== null
    && currentSnapshot.title === lastMove.after.title && currentSnapshot.description === lastMove.after.description
    && currentSnapshot.criteria === lastMove.after.criteria && currentSnapshot.notes === lastMove.after.notes);
  function rememberMove(what: string, before: MoveSnapshot) {
    lastMove = { what, before, after: { title: draft.title, description: draft.description, criteria, notes } };
  }
  /** Fill the fields from the notes now, so the reader edits the result before saving it. */
  function applyNotesNow() {
    const preview = notesPreview;
    if (!preview || enhancementBusy) return;
    const before = { ...currentSnapshot };
    if (!applyExtractedNotes()) return;
    rememberMove("the fill", before);
    const touched: EnhancementField[] = [];
    if (preview.titleFromNotes) touched.push("title");
    if (preview.addedText) touched.push("description");
    flashFields(touched);
    const parts = [preview.titleFromNotes ? "title" : "", preview.addedText ? "description" : "", preview.addedCriteria.length ? `${preview.addedCriteria.length} criteri${preview.addedCriteria.length === 1 ? "on" : "a"}` : ""].filter(Boolean);
    note = parts.length
      ? `Filled ${parts.join(", ")} from your notes. Review them, then save.`
      : "Nothing new in those notes: it was already in the task.";
  }
  function undoMove() {
    if (!lastMove || !canUndoMove || enhancementBusy) return;
    const { before, what } = lastMove;
    draft.title = before.title; draft.description = before.description; criteria = before.criteria; notes = before.notes;
    lastMove = null; dirty = true; note = `Undid ${what}.`;
  }
  /** Append criteria through the one owner, leaving existing rows as typed and saying what it refused. */
  function addCriteria(lines: readonly string[]): number {
    const result = appendCriteria(criteriaRows, lines);
    if (!result.added.length && !result.duplicates && !result.overflow) return 0;
    if (result.added.length) {
      criteria = [...criteriaRows, ...result.added].join("\n");
      dirty = true;
    }
    note = describeRefusedCriteria(result) || (result.added.length > 1 ? `Added ${result.added.length} criteria` : "");
    return result.added.length;
  }
  function addSubtaskItem() {
    if (!newSubtask.trim()) return;
    addCriteria([newSubtask]);
    newSubtask = "";
  }
  function onSubtaskKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      addSubtaskItem();
    }
  }
  /**
   * A pasted block becomes one criterion per item. The paste lands where the
   * cursor is, so text already typed in the box is kept; a block holding a
   * checklist ("Subtasks:" then bullets) adds its items, not its heading.
   */
  function onSubtaskPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text") ?? "";
    if (!/\r?\n/.test(text.trim())) return;
    e.preventDefault();
    const box = e.currentTarget as HTMLInputElement;
    const start = box.selectionStart ?? box.value.length;
    const end = box.selectionEnd ?? start;
    const value = `${box.value.slice(0, start)}${text}${box.value.slice(end)}`;
    addCriteria(criteriaFromPaste(value));
    newSubtask = "";
  }
  function removeSubtaskItem(index: number) {
    const list = [...criteriaRows];
    if (index < 0 || index >= list.length) return;
    list.splice(index, 1);
    criteria = list.join("\n");
    dirty = true;
  }
  function editCriterion(index: number, value: string) {
    // An emptied row stays on screen until it loses focus, so clearing it to
    // retype does not delete it under the cursor.
    if (!value.trim()) return;
    const list = [...criteriaRows];
    if (index < 0 || index >= list.length) return;
    list[index] = value.replace(/[\r\n]+/g, " ");
    criteria = list.join("\n");
    dirty = true;
  }
  async function focusCriterion(index: number) {
    await tick();
    const target = index < 0 || index >= criteriaRows.length
      ? sheet?.querySelector<HTMLInputElement>('[data-testid="task-subtask-add-input"]')
      : sheet?.querySelector<HTMLTextAreaElement>(`[data-testid="task-subtask-item-${index}"] .subtask-text`);
    target?.focus();
  }
  /**
   * Several lines pasted into a row become that row and new rows under it, in
   * the order pasted, through the same `appendCriteria` the add box uses. A
   * criterion is one line, so the paste is never folded into one long row.
   */
  function onCriterionPaste(e: ClipboardEvent, index: number) {
    const text = e.clipboardData?.getData("text") ?? "";
    if (!/\r?\n/.test(text.trim())) return;
    e.preventDefault();
    const box = e.currentTarget as HTMLTextAreaElement;
    const start = box.selectionStart ?? box.value.length;
    const end = box.selectionEnd ?? start;
    const others = criteriaRows.filter((_, i) => i !== index);
    const result = appendCriteria(others, criteriaFromPaste(`${box.value.slice(0, start)}${text}${box.value.slice(end)}`));
    const list = [...others];
    list.splice(index, 0, ...result.added);
    criteria = list.join("\n");
    dirty = true;
    note = describeRefusedCriteria(result) || (result.added.length > 1 ? `Pasted as ${result.added.length} criteria` : "");
  }
  /**
   * Fit a criterion's box to its text.
   *
   * Two things change the height a row needs, and each re-fits it. A new value
   * — typed, or set by the sheet in a move, a fill or an undo, which reuse rows
   * by index — reaches `update`, deferred until the DOM holds it; "Edit as
   * text" is one of its inputs too, so the list coming back re-fits. A new
   * width — the box first laid out, the sheet resized, the Task tab shown after
   * Agents — reaches the observer, which waits a frame: resizing the box inside
   * its own callback is the "ResizeObserver loop" error. The first fit runs at
   * once so a long row is not drawn one line tall for that frame. A box with no
   * layout is left at its one row rather than pinned to 0px.
   */
  function growToFit(node: HTMLTextAreaElement, _deps: unknown) {
    const fit = () => {
      node.style.height = "auto";
      const height = node.scrollHeight;
      node.style.height = height > 0 ? `${height}px` : "";
    };
    fit();
    let width = -1;
    let frame = 0;
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(([entry]) => {
      const next = Math.round(entry?.contentRect.width ?? 0);
      if (next === width) return;
      width = next;
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(fit);
    });
    observer?.observe(node);
    return {
      update: () => { queueMicrotask(fit); },
      destroy() { observer?.disconnect(); cancelAnimationFrame(frame); },
    };
  }
  function moveCriterion(index: number, delta: number) {
    const list = [...criteriaRows];
    const to = index + delta;
    if (to < 0 || to >= list.length) return;
    [list[index], list[to]] = [list[to]!, list[index]!];
    criteria = list.join("\n");
    dirty = true;
    void focusCriterion(to);
  }
  function onCriterionKeydown(e: KeyboardEvent, index: number) {
    const input = e.currentTarget as HTMLTextAreaElement;
    if (e.key === "Enter") {
      // Enter in a form field would submit the sheet; here it moves on. On a
      // row the reader cleared, it removes the row first and stays in place,
      // where the next row now is — computing the target before the removal
      // skipped a row.
      e.preventDefault();
      if (!input.value.trim()) { removeSubtaskItem(index); void focusCriterion(index); return; }
      void focusCriterion(index + 1 < criteriaRows.length ? index + 1 : -1);
    } else if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
      e.preventDefault();
      moveCriterion(index, e.key === "ArrowUp" ? -1 : 1);
    } else if (e.key === "Backspace" && !input.value) {
      e.preventDefault();
      removeSubtaskItem(index);
      void focusCriterion(index > 0 ? index - 1 : 0);
    }
  }
  /**
   * Move the description's checklist into the criteria. Notes are not read
   * here: what they hold is shown, criteria included, in the preview under
   * the notes box, and filled by its button or the save. Reading them here too
   * gave two buttons that each moved the notes' checklist, counted differently.
   */
  function extractSubtasksAction() {
    const extracted = extractDraftSubtasks({ description: draft.description, acceptance_criteria: criteriaRows });
    note = describeChecklistMove(extracted);
    if (!extracted.extracted) return;
    const before = { ...currentSnapshot };
    draft.description = extracted.description;
    criteria = extracted.subtasks.join("\n");
    dirty = true;
    rememberMove("the extract", before);
  }
  /** Typing into an "add" box or a search is not an edit to the task until it is added. */
  function markDirty(event: Event) {
    if (event.target instanceof Element && event.target.closest("[data-ephemeral]")) return;
    dirty = true;
  }
  async function save(fromManvi = false): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete || (enhancementBusy && !fromManvi)) return null;
    // A criterion or checklist item typed but not yet added is saved with the
    // rest, not dropped.
    if (!pending) {
      // A row cleared and still focused has not lost focus yet, so its removal
      // has not run: Cmd+S would otherwise save the text the reader deleted.
      const active = document.activeElement;
      if (active instanceof HTMLTextAreaElement && active.classList.contains("subtask-text") && sheet?.contains(active) && !active.value.trim()) active.blur();
    }
    if (!pending && newSubtask.trim()) addSubtaskItem();
    if (!pending) {
      const refused = relations?.flushPending() ?? "";
      if (refused) { error = refused; return null; }
    }
    if (!pending && !applyExtractedNotes()) return null;
    if (!pending && (!draft.title.trim() || !draft.kind.trim())) { error = "Add a title and task type before saving."; return null; }
    if (!pending) {
      // The store bounds each criterion in bytes; name the row rather than
      // letting the write fail with a message about the whole task.
      const long = criteriaList.findIndex((item) => new TextEncoder().encode(item).length > SUBTASK_CAP);
      if (long >= 0) { error = `Criterion ${long + 1} is longer than 4 KB. Shorten or split it before saving.`; return null; }
    }
    if (!pending && new TextEncoder().encode(draft.description).length > 65_536) { error = "Description is too long. Keep it below 64 KB."; return null; }
    if (!pending) {
      const logs = sanitizeLogs(draft.logs);
      draft.logs = logs.text;
      logNotice = logs.notice;
    }
    saving = true; error = ""; note = ""; shortcutBlocked = "";
    try {
      if (!pending) {
        draft.acceptance_criteria = criteria.split("\n").map((s) => s.trim()).filter(Boolean);
        draft.labels = [...labelChips];
        pending = taskWrite(id, current?.revision ?? 0, draft);
      }
      const saved = await bounded(putTask(pending));
      if (saved.id !== id || saved.revision !== Number(pending.expected_revision) + 1) throw new WorkbenchError("protocol_error", "Task update confirmation does not match the request.");
      if (disposed) return null;
      current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [], logs: saved.logs ?? "" }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels]; dirty = false; pending = null;
      note = "Saved"; lastMove = null; onSaved(saved);
      return saved;
    } catch (cause) {
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !["transport_error", "worker_error", "store_error", "protocol_error"].includes(cause.code)) pending = null;
      return null;
    } finally { saving = false; }
  }
  async function prepareForManvi(): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete !== null) {
      error = "Wait for the current task action to finish.";
      throw new Error(error);
    }
    if (!applyExtractedNotes()) throw new Error(error || "Couldn't use these notes.");
    const blocked = canAskManvi(draft, notes);
    if (blocked) { error = blocked; throw new Error(blocked); }
    if (!current || dirty || pending) {
      const saved = await save(true);
      if (disposed) return null;
      if (!saved) throw new Error(error || `Couldn't save a draft for ${assistName}.`);
      return saved;
    }
    return current;
  }
  function setLogs(value: string) {
    const result = sanitizeLogs(value);
    draft.logs = result.text;
    logNotice = result.notice;
    dirty = true;
  }
  function onLogsPaste(event: ClipboardEvent) {
    const incoming = event.clipboardData?.getData("text") ?? "";
    if (!incoming) return;
    event.preventDefault();
    const area = event.currentTarget as HTMLTextAreaElement;
    const start = area.selectionStart ?? area.value.length;
    const end = area.selectionEnd ?? start;
    setLogs(`${area.value.slice(0, start)}${incoming}${area.value.slice(end)}`);
  }
  async function copyLogs() {
    if (copying || saving || !hasLogs) return;
    copying = true; error = ""; note = "";
    try {
      const packet = sanitizeLogs(draft.logs).text;
      if (packet && await copyText(packet)) note = "Copied raw logs";
      else error = packet ? "Clipboard unavailable" : "Add logs before copying them.";
    } finally { copying = false; }
  }
  function markCopied(message: string) {
    note = message;
    copied = true;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => { copied = false; }, 2000);
  }
  async function copyForAgent() {
    if (copying || saving || pending !== null || enhancementBusy || !copyable) return;
    copying = true; error = ""; note = "";
    try {
      if (current) {
        const revision = current.revision;
        const saved = await bounded(getTaskBrief(id, revision));
        if (disposed) return;
        if (current.revision !== revision) throw new Error("The saved task changed while loading its brief. Copy the latest revision again.");
        const packet = wrapSavedBriefForAgent(saved.markdown);
        if (packet && await copyText(packet)) markCopied(`Copied revision ${revision}${dirty ? " (unsaved edits omitted)" : ""}`);
        else error = "Clipboard unavailable";
      } else {
        const extracted = composeFromNotes({ title: draft.title, description: draft.description, acceptance_criteria: criteriaList }, notes);
        const packet = formatDraftAgentCopy({
          title: extracted.title,
          description: extracted.description,
          kind: draft.kind,
          status: draft.status,
          priority: draft.priority,
          owner: draft.owner,
          labels: [...labelChips],
          // Typed but not yet added: Save keeps it, so the copy carries it too.
          acceptance_criteria: appendCriteria(extracted.acceptance_criteria, [newSubtask]).list,
          repositoryNames: draft.repository_ids.map((repo) => known.find((item) => item.id === repo)?.name ?? repo),
          logs: draft.logs,
        });
        if (packet && await copyText(packet)) markCopied("Copied unsaved draft for an agent");
        else error = packet ? "Clipboard unavailable" : "Add a title or description to copy";
      }
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { copying = false; }
  }
  async function reload() {
    if (!current || saving || adding || enhancementBusy || reloading || confirming || pendingDelete) return;
    reloading = true;
    confirming = true;
    try {
      if (!await askConfirm({title: "Refresh from saved task?", message: "Replace your unsaved edits with the latest saved revision?", confirmLabel: "Refresh", cancelLabel: "Keep editing"}) || disposed) return;
      const latest = await bounded(getTask(id));
      if (disposed) return;
      if (latest.id !== id) throw new WorkbenchError("protocol_error", "Loaded task does not match this editor.");
      lastMove = null; current = latest; draft = { ...taskDraft(latest), locked_fields: latest.locked_fields ?? [], logs: latest.logs ?? "" }; criteria = latest.acceptance_criteria.join("\n"); labelChips = [...latest.labels]; notes = ""; logNotice = ""; pending = null; dirty = false; error = "";
    } catch (cause) { if (!disposed) error = explainError(cause); }
    finally { if (!disposed) { confirming = false; reloading = false; } }
  }
  async function remove() {
    if (!current || saving || adding || enhancementBusy) return;
    if (!pendingDelete) {
      const copy = deleteConfirmCopy([{ id: current.id, revision: current.revision, title: current.title }]);
      if (!await askConfirm({ title: copy.title, message: copy.message, confirmLabel: copy.confirmLabel, cancelLabel: "Keep task", destructive: true })) return;
      if (disposed) return;
      pendingDelete = deleteAttempt({ id: current.id, revision: current.revision, title: current.title }, newID());
    }
    if (!pendingDelete) return;
    saving = true; error = "";
    try {
      await bounded(deleteTask(pendingDelete.id, pendingDelete.expected_revision, pendingDelete.request_id));
      if (disposed) return;
      pendingDelete = null;
      onSaved(current);
      onClose();
    } catch (cause) {
      if (disposed) return;
      error = explainError(cause);
      if (cause instanceof WorkbenchError && !isRetryableDelete(cause)) pendingDelete = null;
    } finally { if (!disposed) saving = false; }
  }
  function applied(saved: Task) {
    lastMove = null;
    current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [], logs: saved.logs ?? "" }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels];
    dirty = false; pending = null; note = `Saved revision ${saved.revision}`; onSaved(saved);
  }
  function onTabKeydown(event: KeyboardEvent) {
    const index = tabs.findIndex((entry) => entry.id === tab);
    const move = handleTablistKeydown(event.key, index, tabs.length);
    if (!move) return;
    event.preventDefault();
    const next = tabs[move.index];
    if (!next) return;
    requestedTab = next.id;
    focusTabAt(tabStrip, move.index);
  }

  function onKindSelect(value: string) {
    if (value === "__custom__") { customKind = true; return; }
    customKind = false;
    draft.kind = value;
    dirty = true;
  }
</script>

<!--
  The repository popover is portaled to the body to escape `.sheet-body`'s
  scroller, so it is NOT inside `sheet` — and a guard that only asked
  `sheet.contains` would leave Cmd+S and Cmd+Shift+C dead for a reader whose
  focus is in the picker. `owns()` is the sheet's real boundary: its own
  subtree, plus the overlays it opened.
-->
<svelte:window onkeydown={(e) => {
    const owns = (node: Node) =>
      Boolean(sheet?.contains(node)) ||
      (node instanceof Element && Boolean(node.closest("[data-task-repo-popup]")));
    if (!active || !(e.target instanceof Node) || !owns(e.target)) return;
    if (e.key === "Escape") {
      e.preventDefault();
      void close();
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "s") {
      e.preventDefault();
      if (enhancementBusy) { shortcutBlocked = `Wait for ${assistName} to finish before saving.`; return; }
      void save();
    }
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "c") {
      e.preventDefault();
      void copyForAgent();
    }
  }} />

<aside bind:this={sheet} class="task-editor gp-glass bg-surface" aria-label={current ? "Task details" : "New task"} data-task-revision={current?.revision ?? ""}>
  <header class="gp-glass shadow-float" style="z-index: {LAYERS.STICKY}">
    <div>
      <h2>{current ? "Task details" : "New task"}</h2>
      <small>{dirty ? "Unsaved" : current ? `Revision ${current.revision}` : "Describe the work, then save."}</small>
    </div>
    <div class="header-actions">
      <button type="button" class="gp-btn" onclick={() => void copyForAgent()} disabled={!copyable || copying || saving || enhancementBusy || pending !== null} aria-label="Copy task for an AI agent" title={copyable ? "Copy a packet an AI agent can paste" : "Add a title, description, notes, or logs first"}>
        <Clipboard size={12} /> {copying ? "Copying…" : copied ? "Copied" : "Copy for agent"}
      </button>
      <button type="button" class="gp-icon-btn" onclick={close} disabled={enhancementWorking || saving || adding || reloading || confirming || pending !== null || pendingDelete !== null} aria-label="Close task details">✕</button>
    </div>
  </header>
  {#if tabs.length > 1}
    <div
      bind:this={tabStrip}
      class="sheet-tabs gp-segmented"
      class:gp-liquid-tabs={macos}
      role="tablist"
      aria-label="Task sections"
      tabindex="-1"
      onkeydown={onTabKeydown}
    >
      {#each tabs as entry, index (entry.id)}
        {@const props = tabProps(group, entry.id, tab === entry.id)}
        {@const badge = editorTabBadge(entry.id, { working: workingCount })}
        <button
          type="button"
          class="gp-seg-btn text-meta!"
          role={props.role}
          id={props.id}
          aria-selected={props["aria-selected"]}
          aria-controls={props["aria-controls"]}
          tabindex={props.tabindex}
          data-active={tab === entry.id ? "true" : "false"}
          data-sheet-tab={entry.id}
          title={editorTabHint(entry.id, saved)}
          onclick={() => { requestedTab = entry.id; focusTabAt(tabStrip, index); }}
        >
          {#if macos && tab === entry.id}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: `task-sheet-pane-${id}` }}
              out:sendSelection={{ key: `task-sheet-pane-${id}` }}
            ></span>
          {/if}
          <span>{entry.label}</span>
          {#if badge}{@const asking = entry.id === "agent" ? askingCount : 0}{@const said = asking ? `${badge} working now, ${asking} ${asking === 1 ? "needs" : "need"} you` : `${badge} working now`}<span class="tab-badge tabular-nums" data-asking={asking > 0 ? "true" : null} title={said} aria-label={said} data-testid="agents-working-badge">{badge}</span>{/if}
          <!-- A suggestion waiting to be reviewed lives on Task. The dot is
               how the reader knows that while they are on Agents. -->
          {#if entry.id === "task" && reviewable}<span class="tab-dot" aria-label="Suggestion ready"></span>{/if}
        </button>
      {/each}
    </div>
    <p class="tab-hint">{editorTabHint(tab, saved)}</p>
  {/if}
  <div class="sheet-body" use:scrollEdgeFade>
  <form id="task-editor-form-{id}" novalidate
    onsubmit={(e) => { e.preventDefault(); void save(); }}
    oninput={markDirty}
    onchange={markDirty}
  >
    <fieldset disabled={saving || reloading || pending !== null || pendingDelete !== null}>
      <div class="pane" hidden={Boolean(current) && tab !== "task"} id={panelId(group, "task")} role={current ? "tabpanel" : undefined} aria-labelledby={current ? tabId(group, "task") : undefined}>
        <!--
          One column, numbered sections, in an order `editorSections` owns.
          A draft still dictates first: notes above the fields they fill. A
          saved task leads with the record (title, logs, schedule, owner) and
          folds the model last — the compose chrome labelled "Quick add" used
          to sit above the title on a revision the reader had already written,
          which is the layout this refuses.

          The assist stays mounted when folded (`hidden`, not `{#if}`), so a
          running suggestion can still cancel and history can still load. The
          model's suggestions are not a seventh section; they live inside assist.
        -->
        <div class="steps">
          {#if current}<TaskVerification taskId={current.id} revision={current.revision} repositoryPath={$repoStore.currentPath ?? ""} />{/if}
          {#each sections as section (section.id)}
            <section class="step" data-editor-section={section.id} data-assist-open={section.id === "assist" ? String(assistChrome.open) : undefined}>
              <div class="step-head">
                <span class="step-n" aria-hidden="true">{section.n}</span>
                <h3>{section.heading}</h3>
                {#if section.id === "assist" && assistStatus}
                  <span class="assist-status" data-testid="task-assist-status">{assistStatus}</span>
                {/if}
                {#if section.id === "assist" && assistChrome.collapsible}
                  <button
                    type="button"
                    class="gp-btn shrink-0" data-size="sm"
                    data-testid="task-assist-toggle"
                    aria-expanded={assistChrome.open}
                    aria-controls="task-assist-body-{id}"
                    onclick={() => { assistOpened = !assistChrome.open; }}
                  >{assistChrome.open ? "Hide" : "Show"}</button>
                {/if}
                {#if section.id === "logs" && logsChrome.collapsible}
                  <button
                    type="button"
                    class="gp-btn shrink-0" data-size="sm"
                    data-testid="task-logs-toggle"
                    aria-expanded={logsChrome.open}
                    aria-controls="task-logs-body-{id}"
                    onclick={() => { logsOpened = !logsChrome.open; }}
                  >{logsChrome.open ? "Hide" : "Show"}</button>
                {/if}
                <span class="step-rule" aria-hidden="true"></span>
              </div>
              {#if section.id === "repositories"}
                <!-- `enhancementBusy` disables the fields a running suggestion may
                     rewrite. It wraps the *fields* and never the assist, which owns
                     the cancel button for that same run. -->
                <fieldset disabled={enhancementBusy}>
                  <TaskRepositoryPicker
                    {rows}
                    {summary}
                    {offerFilter}
                    chips={trigger.chips}
                    overflow={trigger.overflow}
                    bind:filter={repoFilter}
                    knownCount={known.length}
                    name={id}
                    {addable}
                    {adding}
                    {attaching}
                    workspace={draft.home_workspace_id ? { name: homeWorkspaceName, error: homeError } : null}
                    disabled={saving || reloading || pending !== null || pendingDelete !== null || enhancementBusy}
                    onToggle={membership}
                    onPrimary={setPrimary}
                    onAddPaths={(paths) => void addOpenPaths(paths)}
                    onAttachOutsiders={() => void attachOutsiders()}
                  />
                </fieldset>
              {:else if section.id === "assist"}
                <!-- Dictating, choosing a model, and reviewing what came back are
                     one surface. On a draft it sits above the fields; on a saved
                     task it follows the title and is folded. The assist owns the
                     lifecycle (polling, history, accept, undo) and sits outside the
                     enhancement-busy fieldset so a running suggestion cannot
                     disable its own cancel. -->
                <div id="task-assist-body-{id}" data-testid="task-assist-body" hidden={!assistChrome.open}>
                  <TaskManviAssist
                    task={current}
                    compose={!current}
                    {notes}
                    {dirty}
                    {active}
                    onNotes={(value) => { notes = value; dirty = true; }}
                    onFillNotes={() => { if (!notesPreview) return false; applyNotesNow(); return true; }}
                    bind:title={draft.title}
                    bind:description={draft.description}
                    bind:lockedFields={draft.locked_fields!}
                    repositoryIds={draft.repository_ids}
                    repositoryNames={draft.repository_ids.map((repo) => known.find((item) => item.id === repo)?.name ?? "").filter(Boolean)}
                    prepareTask={prepareForManvi}
                    onApplied={applied}
                    onBusy={(busy) => { enhancementBusy = busy; }}
                    onEditing={(editing) => { suggestionEditing = editing; }}
                    onFlash={(fields) => { flash = fields; }}
                    onReview={(ready) => { reviewable = ready; }}
                    onEngine={(name) => { assistName = name; }}
                    onStatus={({ state, uncertain }) => { assistState = state; assistUncertain = uncertain; }}
                    disabled={saving || reloading || pending !== null || pendingDelete !== null}
                  >
                  {#snippet notesExtra()}
                  {#if notesPreview?.extracted}
                    <!-- Drawn from the same `composeFromNotes` the save calls, so
                         nothing here is a guess about what Save will write. -->
                    <div class="notes-preview" data-testid="task-notes-preview" aria-live="polite">
                      <p class="preview-head">{current ? "Saving adds your notes to this task" : "Your notes will become"}</p>
                      <dl>
                        {#if notesPreview.titleFromNotes}
                          <div><dt class="gp-label">Title</dt><dd class="preview-title" data-testid="task-notes-preview-title">{notesPreview.title}</dd></div>
                        {/if}
                        {#if notesPreview.addedText}
                          {@const lineCount = notesPreview.addedText.split("\n").filter((line) => line.trim()).length}
                          <div><dt class="gp-label">Description</dt><dd>{draft.description.trim() ? "Adds" : "Gets"} {lineCount} line{lineCount === 1 ? "" : "s"}<span class="preview-excerpt">{notesPreview.addedText.slice(0, 140)}{notesPreview.addedText.length > 140 ? "…" : ""}</span></dd></div>
                        {/if}
                        {#if notesPreview.addedCriteria.length}
                          <div><dt class="gp-label">Criteria</dt><dd data-testid="task-notes-preview-criteria">
                            +{notesPreview.addedCriteria.length}
                            {#each notesPreview.addedCriteria.slice(0, 3) as item (item)}<span class="gp-chip" data-kind="label">{item}</span>{/each}
                            {#if notesPreview.addedCriteria.length > 3}<span class="meta">and {notesPreview.addedCriteria.length - 3} more</span>{/if}
                          </dd></div>
                        {/if}
                        {#if !notesPreview.titleFromNotes && !notesPreview.addedText && !notesPreview.addedCriteria.length}
                          <div><dt class="gp-label">Nothing new</dt><dd>Everything in these notes is already in the task.</dd></div>
                        {/if}
                      </dl>
                      <button type="button" class="gp-btn" data-size="sm" data-testid="task-notes-apply" disabled={enhancementBusy} onclick={applyNotesNow}>Fill the fields now</button>
                    </div>
                  {/if}
                  {/snippet}
                  </TaskManviAssist>
                  {#if current}
                    <TaskRefine task={current} repositoryPath={$repoStore.currentPath ?? ""} disabled={dirty || saving || reloading || pending !== null || pendingDelete !== null || enhancementBusy} onApplied={applied} />
                  {/if}
                </div>
              {:else if section.id === "title"}
                <fieldset disabled={enhancementBusy}>
                  <label class:flash={flash.includes("title")}>Title
                    <input class="gp-field" name="task-title" bind:value={draft.title} disabled={enhancementBusy} required maxlength="300" placeholder={current ? "What needs doing" : `A short name, or paste notes above and let them fill it`} />
                  </label>
                  <label class:flash={flash.includes("description")}>Description
                    <textarea class="gp-field-multi" bind:value={draft.description} disabled={enhancementBusy} rows="6" maxlength="65536" placeholder="Context, reproduction steps, links. A “- [ ] item” checklist here can move into Acceptance criteria."></textarea>
                  </label>
                </fieldset>
              {:else if section.id === "criteria"}
                <fieldset disabled={enhancementBusy} class="criteria" data-testid="task-criteria-section">
                  <div class="criteria-tools">
                    <p class="meta criteria-summary">{criteriaList.length
                      ? `${criteriaList.length} criteri${criteriaList.length === 1 ? "on" : "a"} — what has to be true for this to be done`
                      : "What has to be true for this task to be done."}</p>
                    {#if checklistInText.extracted}
                      {@const found = checklistInText.count + checklistInText.duplicates}
                      <button
                        type="button"
                        class="gp-btn shrink-0" data-size="sm"
                        data-testid="task-extract-subtasks"
                        disabled={enhancementBusy}
                        onclick={extractSubtasksAction}
                        title="Move the checklist in the description into this list"
                      >Extract {found} from text</button>
                    {/if}
                    <button
                      type="button"
                      class="gp-btn shrink-0" data-size="sm" data-variant="ghost"
                      data-testid="task-criteria-raw-toggle"
                      aria-pressed={rawCriteria}
                      onclick={() => { rawCriteria = !rawCriteria; }}
                    >{rawCriteria ? "Edit as list" : "Edit as text"}</button>
                  </div>
                  <div class="criteria-list-view" hidden={rawCriteria}>
                    {#if criteriaRows.length > 0}
                      <ol class="subtasks-list" data-testid="task-subtasks-list" aria-label="Acceptance criteria">
                        {#each criteriaRows as item, index}
                          <li class="subtask-item" data-testid="task-subtask-item-{index}">
                            <span class="subtask-n tabular-nums" aria-hidden="true">{index + 1}</span>
                            <!-- A text box one line tall that grows with its text, so a
                                 long criterion is read whole rather than scrolled
                                 sideways. Enter never adds a line: a criterion is one. -->
                            <textarea
                              class="subtask-text"
                              rows="1"
                              value={item}
                              maxlength="4096"
                              disabled={enhancementBusy}
                              aria-label="Criterion {index + 1}"
                              title="Edit in place. Alt+↑/↓ reorders; Backspace on an empty row removes it."
                              use:growToFit={[item, rawCriteria]}
                              oninput={(event) => editCriterion(index, event.currentTarget.value)}
                              onblur={(event) => { if (!event.currentTarget.value.trim()) removeSubtaskItem(index); }}
                              onkeydown={(event) => onCriterionKeydown(event, index)}
                              onpaste={(event) => onCriterionPaste(event, index)}
                            ></textarea>
                            <!-- The reorder Alt+↑/↓ does, for a reader who uses the
                                 pointer. mousedown is held so a click does not first
                                 blur a cleared row, whose removal would shift the
                                 index under the button. -->
                            <span class="subtask-moves">
                              <button
                                type="button"
                                class="subtask-move"
                                data-testid="task-subtask-up-{index}"
                                disabled={enhancementBusy || index === 0}
                                onmousedown={(event) => event.preventDefault()}
                                onclick={() => moveCriterion(index, -1)}
                                aria-label="Move criterion {index + 1} up"
                                title="Move up (Alt+↑)"
                              ><ChevronUp size={12} /></button>
                              <button
                                type="button"
                                class="subtask-move"
                                data-testid="task-subtask-down-{index}"
                                disabled={enhancementBusy || index === criteriaRows.length - 1}
                                onmousedown={(event) => event.preventDefault()}
                                onclick={() => moveCriterion(index, 1)}
                                aria-label="Move criterion {index + 1} down"
                                title="Move down (Alt+↓)"
                              ><ChevronDown size={12} /></button>
                            </span>
                            <button
                              type="button"
                              class="subtask-remove"
                              data-testid="task-subtask-remove-{index}"
                              disabled={enhancementBusy}
                              onmousedown={(event) => event.preventDefault()}
                              onclick={() => removeSubtaskItem(index)}
                              aria-label="Remove criterion: {item}"
                              title="Remove"
                            >
                              <X size={12} />
                            </button>
                          </li>
                        {/each}
                      </ol>
                    {/if}
                    <div class="subtask-add-row" data-testid="task-subtask-add-row">
                      <input
                        class="gp-field subtask-add-input"
                        data-size="sm"
                        data-testid="task-subtask-add-input"
                        data-ephemeral
                        bind:value={newSubtask}
                        disabled={enhancementBusy || criteriaList.length >= MAX_SUBTASKS}
                        maxlength="4096"
                        aria-label="New acceptance criterion"
                        placeholder={criteriaList.length >= MAX_SUBTASKS ? `The list is full (${MAX_SUBTASKS})` : "Add a criterion — paste several lines to add each"}
                        onkeydown={onSubtaskKeydown}
                        onpaste={onSubtaskPaste}
                      />
                      <button
                        type="button"
                        class="gp-btn shrink-0" data-size="sm"
                        data-testid="task-subtask-add-button"
                        disabled={enhancementBusy || !newSubtask.trim()}
                        onclick={addSubtaskItem}
                      >
                        Add
                      </button>
                    </div>
                  </div>
                  <label class="criteria-raw-label" hidden={!rawCriteria}>
                    <span class="sr-only">Acceptance criteria</span>
                    <textarea
                      id="task-criteria-{id}"
                      class="gp-field-multi criteria-textarea"
                      data-testid="task-criteria"
                      bind:value={criteria}
                      rows="5"
                      maxlength="65536"
                      placeholder="One verifiable criterion per line"
                    ></textarea>
                  </label>
                </fieldset>
              {:else if section.id === "logs"}
                <div id="task-logs-body-{id}" data-testid="task-logs-body" hidden={!logsChrome.open}>
                  <div class="logs-head">
                    <p class="meta">Paste a stack trace or terminal dump. It is saved with the task so you can copy it or send it to an agent.</p>
                    <button type="button" class="gp-btn" onclick={() => void copyLogs()} disabled={!hasLogs || copying || saving} aria-label="Copy raw logs">Copy logs</button>
                  </div>
                  <fieldset disabled={enhancementBusy}>
                    <label>Raw logs<textarea class="gp-field-multi logs-field" data-testid="task-logs" value={draft.logs ?? ""} rows="8" spellcheck="false" wrap="off" placeholder="Paste rustc, pytest, or terminal output. ANSI codes are stripped; the text is kept." oninput={(event) => setLogs(event.currentTarget.value)} onpaste={onLogsPaste}></textarea></label>
                  </fieldset>
                  {#if logNotice}<p class="meta" role="status">{logNotice}</p>{/if}
                </div>
              {:else if section.id === "schedule"}
                <fieldset disabled={enhancementBusy}>
                  <div class="pair">
                    <label>Status<select class="gp-select" bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label>
                    <label>Priority<select class="gp-select" bind:value={draft.priority}>{#each PRIORITY_LABELS as name, level}<option value={level}>{name}</option>{/each}</select></label>
                  </div>
                  <div class="pair">
                    <label>Type
                      {#if customKind || kindIsCustom}
                        <input class="gp-field" bind:value={draft.kind} required maxlength="64" placeholder="Custom type" />
                        <button type="button" class="gp-btn kind-preset" disabled={enhancementBusy} onclick={() => { customKind = false; draft.kind = "feature"; dirty = true; }}>Use preset</button>
                      {:else}
                        <select class="gp-select" value={draft.kind} onchange={(e) => onKindSelect(e.currentTarget.value)}>
                          {#each KIND_OPTIONS as kind}<option value={kind}>{kind}</option>{/each}
                          <option value="__custom__">Custom…</option>
                        </select>
                      {/if}
                    </label>
                    <label>Severity<select class="gp-select" bind:value={draft.severity}><option value={null}>None</option>{#each SEVERITY_OPTIONS as severity}<option value={severity.value}>{severity.label}</option>{/each}</select></label>
                  </div>
                  <!-- The plan's optional home group. It was set once, from the
                       scope a task was created in, and nothing could change it.
                       An archived workspace is offered only while it is the
                       current choice, so saving never silently clears it. -->
                  <label>Home workspace<select class="gp-select" bind:value={draft.home_workspace_id}><option value={null}>None</option>{#each workspaces.filter((space) => !space.archived || space.id === draft.home_workspace_id) as space (space.id)}<option value={space.id}>{space.name}{space.archived ? " (archived)" : ""}</option>{/each}</select></label>
                  <!-- Owner is one field, so it sits beside Due rather than taking
                       a numbered section of its own. Written on one line: the
                       label's first child has to be the name. -->
                  <div class="pair">
                    <label>Owner<input class="gp-field" value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" placeholder="Unassigned" /></label>
                    <div class="due-field">
                      <span class="field-label">Due</span>
                      <!-- `disabled` is passed rather than inherited: the popover is
                           portaled to the body, so a disabled fieldset around this
                           component would not reach the controls inside it. -->
                      <TaskDuePicker
                        value={draft.due_at}
                        disabled={saving || reloading || pending !== null || pendingDelete !== null || enhancementBusy}
                        onChange={(next) => { draft.due_at = next; dirty = true; }}
                      />
                    </div>
                  </div>
                  <label>Labels
                    <LabelInput bind:value={labelChips} disabled={enhancementBusy} />
                  </label>
                  {#if current}
                    <div class="notifications-row">
                      <p class="notifications-label">Notifications (profile-wide; mute this task)</p>
                      <NativeNotificationSettings scope={{ kind: "global" }} taskID={current.id} />
                    </div>
                  {/if}
                </fieldset>
              {:else if section.id === "relations"}
                <!-- The done-able checklist, links to other tasks, and real child
                     subtasks. They used to hang off the bottom of "Status and
                     scheduling", where nothing said they were there. -->
                <fieldset disabled={enhancementBusy}>
                  <TaskRelations
                    bind:checklist={draft.checklist}
                    bind:links={draft.links}
                    taskId={current?.id ?? null}
                    task={current}
                    bind:this={relations}
                    acceptanceCriteria={current?.acceptance_criteria ?? []}
                    criteriaUnsaved={criteriaList.join("\n") !== (current?.acceptance_criteria ?? []).join("\n")}
                    repositories={known}
                    {openTabs}
                    repositoryPath={$repoStore.currentPath ?? ""}
                    disabled={enhancementBusy}
                    onchange={() => { dirty = true; }}
                  />
                </fieldset>
              {/if}
            </section>
          {/each}
        </div>
      </div>
    </fieldset>
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if shortcutBlocked}<p role="status" class="meta">{shortcutBlocked}</p>{/if}
    {#if pending}<p role="status">Couldn't confirm the save. Try the same write again before editing further.</p>{/if}
    {#if pendingDelete}<p role="status">Couldn't confirm the deletion. Try the same delete again before editing further.</p>{/if}
  </form>
  {#if current}
    <div class="pane" hidden={tab !== "agent"} id={panelId(group, "agent")} role="tabpanel" aria-labelledby={tabId(group, "agent")}>
      <TaskAgentPanel
        task={current}
        {repositories}
        {openTabs}
        {active}
        {dirty}
        disabled={saving || reloading || confirming || pending !== null || pendingDelete !== null || enhancementBusy}
        onCount={(count) => { workingCount = count; }}
        onAttention={(count) => { askingCount = count; }}
      />
    </div>
  {/if}
  </div>
  <footer>
    <button class="gp-btn" data-variant="primary" disabled={saving || adding || enhancementBusy || pendingDelete !== null || !draft.repository_ids.length || (!pending && ((!draft.title.trim() && !notes.trim()) || !draft.kind.trim()))} form="task-editor-form-{id}" type="submit">{saving && !pendingDelete ? "Saving…" : pending ? "Try save again" : "Save task"}</button>
    {#if current}
      <button type="button" class="gp-btn" onclick={reload} disabled={saving || enhancementBusy || pendingDelete !== null}>Refresh saved</button>
      <button type="button" class="gp-btn" data-variant="danger" onclick={remove} disabled={saving || adding || enhancementBusy || pending !== null}>{pendingDelete ? "Try delete again" : "Delete"}</button>
    {/if}
    {#if note}<p role="status" class="footer-note">{note}</p>{/if}
    {#if canUndoMove && lastMove}<button type="button" class="gp-btn" data-size="sm" data-variant="ghost" data-testid="task-notes-undo" disabled={enhancementBusy} onclick={undoMove} title="Put the fields back as they were before {lastMove.what}">Undo {lastMove.what.replace(/^the /, "")}</button>{/if}
    <!-- The reason Save is off is its own line: a note left by an earlier action
         ("Added 3 criteria") used to take its place and hide it. -->
    {#if saveBlocker && !pending}<p class="footer-note" data-testid="task-save-blocker">{saveBlocker}</p>{/if}
  </footer>
</aside>

<style>
  /* `[hidden]` is Tailwind preflight's zero-specificity rule, so any
     `display` this file sets on `.pane` would silently beat it and leave
     every pane on screen at once. The explicit rule below is the guard. */
  .pane[hidden]{display:none}
  .sheet-tabs{flex-shrink:0;margin:0 18px 8px;width:calc(100% - 36px)}
  .tab-hint{flex-shrink:0;margin:0 18px 10px;font-size:var(--text-meta);color:rgb(var(--c-text-muted))}
  .tab-badge{margin-left:5px;padding:0 4px;border-radius:var(--gp-r-full);font-size:var(--text-micro);line-height:14px;background:rgb(var(--c-surface-hover) / 0.8);color:rgb(var(--c-text-muted))}
  .tab-badge[data-asking]{background:rgb(var(--c-warn) / 0.2);color:rgb(var(--c-warn));font-weight:600}
  .tab-dot{margin-left:4px;width:5px;height:5px;border-radius:var(--gp-r-full);background:rgb(var(--c-accent));display:inline-block}
  .flash :is(input,textarea){animation:gp-task-flash var(--gp-dur-emphasis) ease-out 2}
  @keyframes gp-task-flash{from{border-color:rgb(var(--c-accent));box-shadow:0 0 0 3px rgb(var(--c-accent) / 0.18)}to{border-color:rgb(var(--c-border));box-shadow:none}}
  @media (prefers-reduced-motion: reduce){.flash :is(input,textarea){animation:none}}
  /* `--gp-task-sheet-w` (app.css) is the one owner of this width: the strip
     of open-task tabs above the sheet reads the same token, and these used to
     be two literals that could drift apart. */
  .task-editor{width:var(--gp-task-sheet-w);flex-shrink:0;min-width:0;min-height:0;border:1px solid rgb(var(--c-border) / 0.65);border-radius:var(--gp-r-lg);margin:0 8px 8px 0;overflow:hidden;padding:0;color:rgb(var(--c-text));display:flex;flex-direction:column}
  /* The columns respond to the sheet, not the window: this is a dock whose
     width the reader's window only partly decides. `container-type` also
     makes this a containing block for fixed descendants, which is why the
     repository popover is portaled to the body. */
  .sheet-body{flex:1;min-height:0;overflow:auto;scroll-padding-block:2rem;padding:0 18px 18px;container-type:inline-size}
  /* One column at every width. The two-column grid this replaces only applied
     past 520px, so which layout a reader got depended on how far they had
     dragged the dock's splitter. */
  .steps{display:flex;flex-direction:column;gap:20px}
  .step{min-width:0}
  .step-head{display:flex;align-items:center;gap:7px;margin:0 0 8px}
  .step-head h3{margin:0;font-size:var(--text-meta);font-weight:500;color:rgb(var(--c-text-muted))}
  .step-n{flex-shrink:0;width:16px;height:16px;border-radius:var(--gp-r-full);display:inline-flex;align-items:center;justify-content:center;font-size:var(--text-micro);font-weight:600;line-height:1;color:rgb(var(--c-accent));background:rgb(var(--c-accent) / 0.14);border:1px solid rgb(var(--c-accent) / 0.35)}
  .step-rule{flex:1;height:1px;background:linear-gradient(to right,rgb(var(--c-border) / 0.55),transparent)}
  .assist-status{font-size:var(--text-micro);font-weight:500;color:rgb(var(--c-text-muted))}
  
  /* Same guard as `.pane[hidden]`: a later `display` on this body would
     silently beat Tailwind's zero-specificity `[hidden]` and leave the
     folded assist on screen. */
  [data-testid="task-assist-body"][hidden],[data-testid="task-logs-body"][hidden]{display:none}
  .logs-head{display:flex;align-items:flex-start;justify-content:space-between;gap:10px;margin:0 0 8px}
  .logs-head p{margin:0;flex:1}
  .logs-field{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;font-size:var(--text-meta);white-space:pre;overflow:auto}
  /* The last field in a section carries the section's own bottom gap, not a
     second one of its own. */
  .step fieldset > :last-child{margin-bottom:0}
  .due-field{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;min-width:0;flex:1}
  .field-label{color:rgb(var(--c-text-muted));font-size:var(--text-meta)}
  header,footer,.pair,.header-actions{display:flex;gap:10px;align-items:center}header{padding:16px 18px;justify-content:space-between;flex-shrink:0;padding-bottom:10px;background:rgb(var(--c-surface) / 0.82)}h2{font-size:var(--text-base);font-weight:600;margin:0}small,.meta,.notifications-label{color:rgb(var(--c-text-muted));font-size:var(--text-meta)}form{font-size:var(--text-xs);min-width:0}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;flex:1}.pair{align-items:flex-start}input,textarea,select{width:100%;min-width:0}textarea{resize:vertical}
  /* Only a control that carries no field primitive is drawn here. This rule used
     to restyle every input, so its scoped selector outranked gp-field and
     gp-select and squared the editor's one-line pills into 8px rectangles.
     A criterion row's input is bare on purpose: the row is the field. */
  :where(input,textarea,select):not(.gp-field):not(.gp-field-multi):not(.gp-select):not(.subtask-text){padding:8px;border:1px solid var(--gp-edge);border-radius:var(--gp-r-md);background:rgb(var(--c-bg) / 0.6);color:inherit}button:disabled{opacity:.5}footer{flex-shrink:0;flex-wrap:wrap;padding:10px 18px 16px}.footer-note{margin:0;flex:1;min-width:8rem;color:rgb(var(--c-text-muted))}
  .error{color:rgb(var(--c-danger))}p{font-size:var(--text-xs);margin:10px 0}.notifications-row{margin:12px 0 16px}.kind-preset{margin-top:6px;align-self:flex-start}
  .criteria-tools{display:flex;align-items:center;gap:6px;margin:0 0 8px}
  .criteria-summary{flex:1;min-width:0;margin:0}
  .criteria-list-view{display:flex;flex-direction:column;gap:6px}
  .subtasks-list{list-style:none;margin:0;padding:0;display:flex;flex-direction:column;gap:3px}
  .subtask-item{display:flex;align-items:flex-start;gap:6px;padding:2px 4px 2px 6px;border-radius:var(--gp-r-sm);background:rgb(var(--c-bg) / 0.4);border:1px solid var(--gp-edge-soft);font-size:var(--text-meta)}
  .subtask-item:focus-within{border-color:rgb(var(--c-accent) / 0.65)}
  .subtask-n{flex-shrink:0;min-width:1.2em;padding-top:4px;text-align:right;font-size:var(--text-micro);color:rgb(var(--c-text-muted))}
  /* The row is the field: no second border inside the bordered row. */
  .subtask-item .subtask-text{flex:1;min-width:0;padding:3px 2px;border:0;border-radius:var(--gp-r-xs);background:transparent;color:inherit;font:inherit;font-size:var(--text-meta);line-height:1.4;resize:none;overflow:hidden;overflow-wrap:anywhere}
  .subtask-item .subtask-text:focus{outline:none}
  .subtask-moves{display:inline-flex;flex-shrink:0;opacity:.6}
  .subtask-item:hover .subtask-moves,.subtask-item:focus-within .subtask-moves{opacity:1}
  .subtask-move{background:transparent;border:0;cursor:pointer;padding:2px;display:inline-flex;align-items:center;color:rgb(var(--c-text-muted));border-radius:var(--gp-r-xs)}
  .subtask-move:hover:not(:disabled){color:rgb(var(--c-text))}
  .subtask-move:disabled{opacity:.3;cursor:default}
  .subtask-remove{background:transparent;border:0;cursor:pointer;padding:2px;display:inline-flex;align-items:center;justify-content:center;color:rgb(var(--c-text-muted));border-radius:var(--gp-r-xs);opacity:.6}
  .subtask-item:hover .subtask-remove,.subtask-remove:focus-visible{opacity:1}
  .subtask-remove:hover{color:rgb(var(--c-text))}
  .subtask-add-row{display:flex;gap:6px;align-items:center}
  .subtask-add-input{flex:1;font-size:var(--text-meta)}
  .criteria-raw-label{margin-bottom:0}
  .criteria-list-view[hidden],.criteria-raw-label[hidden]{display:none}

  .notes-preview{margin:10px 0 4px;padding:9px 11px;border-radius:var(--gp-r-md);border:1px dashed rgb(var(--c-accent) / 0.35);background:rgb(var(--c-accent) / 0.06);display:flex;flex-direction:column;gap:6px}
  .preview-head{margin:0;font-size:var(--text-meta);font-weight:600;color:rgb(var(--c-text))}
  .notes-preview dl{margin:0;display:flex;flex-direction:column;gap:4px}
  .notes-preview dl > div{display:grid;grid-template-columns:5.5rem 1fr;gap:8px;align-items:baseline}
  .notes-preview dt{margin:0}
  .notes-preview dd{margin:0;min-width:0;overflow-wrap:anywhere;font-size:var(--text-meta);display:flex;flex-wrap:wrap;gap:4px;align-items:center}
  .preview-title{font-weight:600}
  .preview-excerpt{flex-basis:100%;color:rgb(var(--c-text-muted));white-space:pre-line;overflow-wrap:anywhere;display:-webkit-box;-webkit-line-clamp:2;line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
  .notes-preview > button{align-self:flex-start}</style>
