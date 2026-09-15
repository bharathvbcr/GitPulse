<script lang="ts">
  import { onDestroy, onMount, untrack } from "svelte";
  import { bounded } from "../workbench/taskActions";
  import { Clipboard } from "@lucide/svelte";
  import { copyText } from "../desktop/clipboard";
  import TaskAgentPanel from "./TaskAgentPanel.svelte";
  import NativeNotificationSettings from "./NativeNotificationSettings.svelte";
  import TaskManviAssist from "./TaskManviAssist.svelte";
  import TaskRepositoryPicker from "./TaskRepositoryPicker.svelte";
  import TaskDuePicker from "./TaskDuePicker.svelte";
  import LabelInput from "./LabelInput.svelte";
  import { isCaseInsensitiveFs } from "../repos/paths";
  import { askConfirm } from "../stores/modalStore";
  import { deleteTask, explainError, getTask, getTaskBrief, getWorkspace, newID, putTask, registerRepository, STATUSES, STATUS_LABELS, taskDraft, taskWrite, WorkbenchError, type EnhancementField, type Repository, type Task, type TaskDraft, type TaskStatus, type WorkspaceCard } from "../workbench/client";
  import { deleteAttempt, deleteConfirmCopy, isRetryableDelete } from "../workbench/taskDelete";
  import { addableOpenTabs, attachRepositories, openMembershipCandidates, type OpenTabRef } from "../workbench/openMembership";
  import { linkSummary, repositoryRows, shouldOfferFilter, triggerChips } from "../workbench/taskRepositories";
  import { applyNotesToDraft, canAskManvi, consumeNotes, formatDraftAgentCopy, wrapSavedBriefForAgent } from "../workbench/taskCompose";
  import { assistEngineName, DEFAULT_ASSIST_ENGINE } from "../workbench/taskEnhance";
  import { editorTabBadge, editorTabHint, editorTabs, resolveEditorTab, type TaskEditorTab } from "../workbench/taskEditorTabs";
  import { handleTablistKeydown, focusTabAt, panelId, tabId, tabProps } from "../dom/tablist";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
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

  let { value, seed = null, repositories, workspaces, openTabs = [], primary = "", home = null, active = true, initialStatus = "inbox", onSaved, onClose }: {
    value: Task | null; seed?: Partial<TaskDraft> | null; repositories: Repository[]; workspaces: WorkspaceCard[]; openTabs?: OpenTabRef[];
    primary?: string; home?: string | null; active?: boolean; initialStatus?: TaskStatus;
    onSaved: (task: Task) => void; onClose: () => void;
  } = $props();
  // A mounted editor owns one snapshot. Background board updates never replace it.
  const initial = untrack(() => ({ value, seed, primary, home, initialStatus }));
  let current = $state(initial.value);
  let draft = $state<TaskDraft>((() => {
    const base = initial.value ? taskDraft(initial.value) : {
      title: "", description: "", kind: "feature", status: initial.initialStatus, priority: 2, severity: null,
      owner: null, due_at: null, labels: [], acceptance_criteria: [], repository_ids: initial.primary ? [initial.primary] : [],
      primary_repository_id: initial.primary, home_workspace_id: initial.home, position: Date.now(), locked_fields: [] as EnhancementField[],
      ...initial.seed,
    };
    return { ...base, locked_fields: base.locked_fields ?? [] };
  })());
  let criteria = $state(untrack(() => draft.acceptance_criteria.join("\n")));
  let labelChips = $state<string[]>(untrack(() => [...draft.labels]));
  let error = $state("");
  let note = $state("");
  let saving = $state(false);
  let pending = $state<Record<string, unknown> | null>(null);
  let pendingDelete = $state<{ id: string; request_id: string; expected_revision: number } | null>(null);
  let dirty = $state(false);
  let enhancementBusy = $state(false);
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
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  let confirming = $state(false);
  let reloading = $state(false);
  let shortcutBlocked = $state("");
  let customKind = $state(false);
  let sheet: HTMLElement;
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
  let runCount = $state(0);
  /** Display name of the engine the assist section would use; it owns the picker. */
  let assistName = $state(assistEngineName(DEFAULT_ASSIST_ENGINE));
  const tabs = $derived(editorTabs(Boolean(current)));
  const tab = $derived(resolveEditorTab(requestedTab, Boolean(current)));
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
  onDestroy(() => { disposed = true; if (copiedTimer) clearTimeout(copiedTimer); });
  const copyable = $derived(Boolean(current || draft.title.trim() || draft.description.trim() || notes.trim()));
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
  export async function canLeave(): Promise<boolean> {
    if (saving || adding || reloading || confirming || enhancementBusy) return false;
    if (pending || pendingDelete) { error = "Retry the pending action before closing this task."; return false; }
    if (!dirty && !notes.trim()) return true;
    confirming = true;
    try { return await askConfirm({title:"Discard task edits?",message:"Your unsaved changes will be lost.",confirmLabel:"Discard edits",cancelLabel:"Keep editing",destructive:true}); }
    finally { confirming = false; }
  }
  async function close() {
    if (enhancementBusy) { shortcutBlocked = `Wait for ${assistName} to finish before closing.`; return; }
    if (await canLeave()) onClose();
  }
  function membership(id: string, checked: boolean) {
    draft.repository_ids = checked ? [...new Set([...draft.repository_ids, id])] : draft.repository_ids.filter((r) => r !== id);
    if (!draft.repository_ids.includes(draft.primary_repository_id)) draft.primary_repository_id = draft.repository_ids[0] ?? "";
    dirty = true;
  }
  function applyExtractedNotes(): boolean {
    const next = consumeNotes(draft, notes);
    if (!next.extracted) return true;
    if (new TextEncoder().encode(next.description).length > 65_536) {
      error = "Description is too long. Keep it below 64 KB.";
      return false;
    }
    draft.title = next.title;
    draft.description = next.description;
    notes = next.notes;
    dirty = true;
    return true;
  }
  async function save(fromManvi = false): Promise<Task | null> {
    if (saving || adding || reloading || confirming || pendingDelete || (enhancementBusy && !fromManvi)) return null;
    if (!pending && !applyExtractedNotes()) return null;
    if (!pending && (!draft.title.trim() || !draft.kind.trim())) { error = "Add a title and task type before saving."; return null; }
    if (!pending && new TextEncoder().encode(draft.description).length > 65_536) { error = "Description is too long. Keep it below 64 KB."; return null; }
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
      current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [] }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels]; dirty = false; pending = null;
      note = "Saved"; onSaved(saved);
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
    if (!applyExtractedNotes()) throw new Error(error || "Could not use these notes.");
    const blocked = canAskManvi(draft, notes);
    if (blocked) { error = blocked; throw new Error(blocked); }
    if (!current || dirty || pending) {
      const saved = await save(true);
      if (disposed) return null;
      if (!saved) throw new Error(error || `Could not save a draft for ${assistName}.`);
      return saved;
    }
    return current;
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
        const extracted = applyNotesToDraft(draft, notes);
        const packet = formatDraftAgentCopy({
          title: extracted.title,
          description: extracted.description,
          kind: draft.kind,
          status: draft.status,
          priority: draft.priority,
          owner: draft.owner,
          labels: [...labelChips],
          acceptance_criteria: criteria.split("\n").map((item) => item.trim()).filter(Boolean),
          repositoryNames: draft.repository_ids.map((repo) => known.find((item) => item.id === repo)?.name ?? repo),
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
      if (!await askConfirm({title: "Reload saved task?", message: "Replace your unsaved edits with the latest saved revision?", confirmLabel: "Reload", cancelLabel: "Keep editing"}) || disposed) return;
      const latest = await bounded(getTask(id));
      if (disposed) return;
      if (latest.id !== id) throw new WorkbenchError("protocol_error", "Loaded task does not match this editor.");
      current = latest; draft = { ...taskDraft(latest), locked_fields: latest.locked_fields ?? [] }; criteria = latest.acceptance_criteria.join("\n"); labelChips = [...latest.labels]; notes = ""; pending = null; dirty = false; error = "";
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
    current = saved; draft = { ...taskDraft(saved), locked_fields: saved.locked_fields ?? [] }; criteria = saved.acceptance_criteria.join("\n"); labelChips = [...saved.labels];
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
      if (enhancementBusy) { shortcutBlocked = `Wait for ${assistName} to finish before closing.`; return; }
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
  <header class="gp-glass shadow-float">
    <div>
      <h2>{current ? "Task details" : "New task"}</h2>
      <small>{dirty ? "Unsaved" : current ? `Revision ${current.revision}` : "Describe the work, then save."}</small>
    </div>
    <div class="header-actions">
      <button type="button" class="gp-btn" onclick={() => void copyForAgent()} disabled={!copyable || copying || saving || enhancementBusy || pending !== null} aria-label="Copy task for an AI agent" title={copyable ? "Copy a packet an AI agent can paste" : "Add a title, description, or notes first"}>
        <Clipboard size={12} /> {copying ? "Copying…" : copied ? "Copied" : "Copy for agent"}
      </button>
      <button type="button" class="gp-icon-btn" onclick={close} disabled={enhancementBusy || saving || adding || reloading || confirming || pending !== null || pendingDelete !== null} aria-label="Close task details">✕</button>
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
        {@const badge = editorTabBadge(entry.id, { runs: runCount })}
        <button
          type="button"
          class="gp-seg-btn text-[11px]! py-1!"
          role={props.role}
          id={props.id}
          aria-selected={props["aria-selected"]}
          aria-controls={props["aria-controls"]}
          tabindex={props.tabindex}
          data-active={tab === entry.id ? "true" : "false"}
          data-sheet-tab={entry.id}
          title={editorTabHint(entry.id)}
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
          {#if badge}<span class="tab-badge tabular-nums">{badge}</span>{/if}
          <!-- A suggestion waiting to be reviewed lives on Task. The dot is
               how the reader knows that while they are on Agent. -->
          {#if entry.id === "task" && reviewable}<span class="tab-dot" aria-label="Suggestion ready"></span>{/if}
        </button>
      {/each}
    </div>
    <p class="tab-hint">{editorTabHint(tab)}</p>
  {/if}
  <div class="sheet-body">
  <form id="task-editor-form-{id}" novalidate
    onsubmit={(e) => { e.preventDefault(); void save(); }}
    oninput={() => { dirty = true; }}
    onchange={() => { dirty = true; }}
  >
    <fieldset disabled={saving || reloading || pending !== null || pendingDelete !== null}>
      <div class="pane" hidden={Boolean(current) && tab !== "task"} id={panelId(group, "task")} role={current ? "tabpanel" : undefined} aria-labelledby={current ? tabId(group, "task") : undefined}>
        <!--
          One column, five numbered sections, in the order a task actually gets
          written: what it touches, what you dictated, what it says, how it is
          scheduled, who owns it.

          This replaces a two-column grid. The columns were a container query
          rather than a media query, which was the right call for a dock whose
          width the window only partly decides — but the dock's floor is 380px
          and the columns only fit past 520px, so the layout a reader got
          depended on how far they had dragged a splitter. Numbering survives
          that: the sheet reads as five things at every width.

          Nothing is unnumbered and nothing is behind a disclosure. The model's
          suggestions are not a sixth section either — they belong to the field
          they were dictated into, so they live inside section 2.
        -->
        <div class="steps">
          <section class="step">
            <div class="step-head"><span class="step-n" aria-hidden="true">1</span><h3>Repositories</h3><span class="step-rule" aria-hidden="true"></span></div>
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
          </section>

          <section class="step">
            <div class="step-head"><span class="step-n" aria-hidden="true">2</span><h3>Quick add</h3><span class="step-rule" aria-hidden="true"></span></div>
            <!-- Dictating the task, choosing which model reads it, and
                 reviewing what came back are one surface, above the fields the
                 result lands in. The assist owns the whole lifecycle (polling,
                 history, accept, undo) and sits outside the enhancement-busy
                 fieldset so a running suggestion cannot disable its own
                 cancel. -->
            <TaskManviAssist
              task={current}
              {notes}
              {dirty}
              {active}
              onNotes={(value) => { notes = value; dirty = true; }}
              bind:title={draft.title}
              bind:description={draft.description}
              bind:lockedFields={draft.locked_fields!}
              repositoryIds={draft.repository_ids}
              repositoryNames={draft.repository_ids.map((repo) => known.find((item) => item.id === repo)?.name ?? "").filter(Boolean)}
              prepareTask={prepareForManvi}
              onApplied={applied}
              onBusy={(busy) => { enhancementBusy = busy; }}
              onFlash={(fields) => { flash = fields; }}
              onReview={(ready) => { reviewable = ready; }}
              onEngine={(name) => { assistName = name; }}
              disabled={saving || reloading || pending !== null || pendingDelete !== null}
            />
          </section>

          <section class="step">
            <div class="step-head"><span class="step-n" aria-hidden="true">3</span><h3>Title and description</h3><span class="step-rule" aria-hidden="true"></span></div>
            <fieldset disabled={enhancementBusy}>
              <label class:flash={flash.includes("title")}>Title
                <input class="gp-field" name="task-title" bind:value={draft.title} disabled={enhancementBusy} required maxlength="300" placeholder="Or let {assistName} draft this from your notes" />
              </label>
              <label class:flash={flash.includes("description")}>Description
                <textarea class="gp-field" bind:value={draft.description} disabled={enhancementBusy} rows="6" maxlength="65536" placeholder="Or let {assistName} draft this from your notes"></textarea>
              </label>
              <label>Acceptance criteria<textarea class="gp-field" bind:value={criteria} rows="4" maxlength="65536" placeholder="One verifiable criterion per line"></textarea></label>
            </fieldset>
          </section>

          <section class="step">
            <div class="step-head"><span class="step-n" aria-hidden="true">4</span><h3>Status and scheduling</h3><span class="step-rule" aria-hidden="true"></span></div>
            <fieldset disabled={enhancementBusy}>
              <div class="pair">
                <label>Status<select class="gp-select" bind:value={draft.status}>{#each STATUSES as status}<option value={status}>{STATUS_LABELS[status]}</option>{/each}</select></label>
                <label>Priority<select class="gp-select" bind:value={draft.priority}><option value={0}>Urgent</option><option value={1}>High</option><option value={2}>Normal</option><option value={3}>Low</option></select></label>
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
          </section>

          <section class="step">
            <div class="step-head"><span class="step-n" aria-hidden="true">5</span><h3>Owner</h3><span class="step-rule" aria-hidden="true"></span></div>
            <fieldset disabled={enhancementBusy}>
              <!-- The section heading already says "Owner", so the label is
                   there for a screen reader rather than repeated on screen.
                   Written on one line deliberately: the label's first child has
                   to be the name, not a whitespace text node. -->
              <label class="owner-field"><span class="sr-only">Owner</span><input class="gp-field" value={draft.owner ?? ""} oninput={(e) => { draft.owner = e.currentTarget.value || null; }} maxlength="300" placeholder="Unassigned" /></label>
            </fieldset>
          </section>
        </div>
      </div>
    </fieldset>
    {#if error}<p role="alert" class="error">{error}</p>{/if}
    {#if shortcutBlocked}<p role="status" class="meta">{shortcutBlocked}</p>{/if}
    {#if pending}<p role="status">Save result uncertain. Retry the same write before editing further.</p>{/if}
    {#if pendingDelete}<p role="status">Delete result uncertain. Retry the same delete before editing further.</p>{/if}
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
        onCount={(count) => { runCount = count; }}
      />
    </div>
  {/if}
  </div>
  <footer>
    <button class="gp-btn-primary" disabled={saving || adding || enhancementBusy || pendingDelete !== null || !draft.repository_ids.length || (!pending && ((!draft.title.trim() && !notes.trim()) || !draft.kind.trim()))} form="task-editor-form-{id}" type="submit">{saving && !pendingDelete ? "Saving…" : pending ? "Retry save" : "Save task"}</button>
    {#if current}
      <button type="button" class="gp-btn" onclick={reload} disabled={saving || enhancementBusy || pendingDelete !== null}>Reload saved</button>
      <button type="button" class="gp-btn-danger" onclick={remove} disabled={saving || adding || enhancementBusy || pending !== null}>{pendingDelete ? "Retry delete" : "Delete"}</button>
    {/if}
    {#if note}<p role="status" class="footer-note">{note}</p>{/if}
  </footer>
</aside>

<style>
  /* `[hidden]` is Tailwind preflight's zero-specificity rule, so any
     `display` this file sets on `.pane` would silently beat it and leave
     every pane on screen at once. The explicit rule below is the guard. */
  .pane[hidden]{display:none}
  .sheet-tabs{flex-shrink:0;margin:0 18px 8px;width:calc(100% - 36px)}
  .tab-hint{flex-shrink:0;margin:0 18px 10px;font-size:11px;color:rgb(var(--c-text-muted))}
  .tab-badge{margin-left:5px;padding:0 4px;border-radius:999px;font-size:9px;line-height:14px;background:rgb(var(--c-surface-hover) / 0.8);color:rgb(var(--c-text-muted))}
  .tab-dot{margin-left:4px;width:5px;height:5px;border-radius:999px;background:rgb(var(--c-accent));display:inline-block}
  .flash :is(input,textarea){animation:gp-task-flash 1.1s ease-out}
  @keyframes gp-task-flash{from{border-color:rgb(var(--c-accent));box-shadow:0 0 0 3px rgb(var(--c-accent) / 0.18)}to{border-color:rgb(var(--c-border));box-shadow:none}}
  @media (prefers-reduced-motion: reduce){.flash :is(input,textarea){animation:none}}
  /* `--gp-task-sheet-w` (app.css) is the one owner of this width: the strip
     of open-task tabs above the sheet reads the same token, and these used to
     be two literals that could drift apart. */
  .task-editor{width:var(--gp-task-sheet-w);flex-shrink:0;min-width:0;min-height:0;border-left:1px solid rgb(var(--c-border) / 0.65);overflow:hidden;padding:0;color:rgb(var(--c-text));display:flex;flex-direction:column}
  /* The columns respond to the sheet, not the window: this is a dock whose
     width the reader's window only partly decides. `container-type` also
     makes this a containing block for fixed descendants, which is why the
     repository popover is portaled to the body. */
  .sheet-body{flex:1;min-height:0;overflow:auto;padding:0 18px 18px;container-type:inline-size}
  /* One column at every width. The two-column grid this replaces only applied
     past 520px, so which layout a reader got depended on how far they had
     dragged the dock's splitter. */
  .steps{display:flex;flex-direction:column;gap:20px}
  .step{min-width:0}
  .step-head{display:flex;align-items:center;gap:7px;margin:0 0 8px}
  .step-head h3{margin:0;font-size:11px;font-weight:600;letter-spacing:.04em;text-transform:uppercase;color:rgb(var(--c-text-muted))}
  .step-n{flex-shrink:0;width:16px;height:16px;border-radius:999px;display:inline-flex;align-items:center;justify-content:center;font-size:9px;font-weight:700;line-height:1;color:rgb(var(--c-accent));background:rgb(var(--c-accent) / 0.14);border:1px solid rgb(var(--c-accent) / 0.34)}
  .step-rule{flex:1;height:1px;background:linear-gradient(to right,rgb(var(--c-border) / 0.55),transparent)}
  /* The last field in a section carries the section's own bottom gap, not a
     second one of its own. */
  .step fieldset > :last-child{margin-bottom:0}
  .due-field{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;min-width:0}
  .field-label{color:rgb(var(--c-text-muted));font-size:11px}
  .owner-field{margin-bottom:0;gap:0}
  header,footer,.pair,.header-actions{display:flex;gap:10px;align-items:center}header{padding:16px 18px;justify-content:space-between;flex-shrink:0;z-index:1;padding-bottom:10px;background:rgb(var(--c-surface) / 0.82)}h2{font-size:16px;font-weight:650;margin:0}small,.meta,.notifications-label{color:rgb(var(--c-text-muted));font-size:11px}form{font-size:12px;min-width:0}fieldset{border:0;padding:0;min-width:0}label{display:flex;flex-direction:column;gap:6px;margin-bottom:13px;flex:1}.pair{align-items:flex-start}input,textarea,select{width:100%;padding:8px;border:1px solid rgb(var(--c-border));border-radius:7px;background:rgb(var(--c-bg) / 0.6);color:inherit;min-width:0}textarea{resize:vertical}button:disabled{opacity:.5}footer{flex-shrink:0;flex-wrap:wrap;padding:10px 18px 16px;border-top:1px solid rgb(var(--c-border) / 0.45)}.footer-note{margin:0;flex:1;min-width:8rem;color:rgb(var(--c-text-muted))}
  .error{color:#dc6565}p{font-size:12px;margin:10px 0}.notifications-row{margin:12px 0 16px}.kind-preset{margin-top:6px;align-self:flex-start}
</style>
