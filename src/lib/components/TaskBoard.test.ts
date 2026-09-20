import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

describe("TaskBoard materials", () => {
  it("lets the shared material system paint the board instead of covering it with opaque local fills", () => {
    for (const selector of ["workbench", "search", "column-title", "card", "ghost"]) {
      const rule = source.match(new RegExp(`\\.${selector}\\{([^}]*)\\}`))?.[1];
      expect(rule, `missing .${selector} rule`).toBeDefined();
      expect(rule, `.${selector} covers the shared material`).not.toMatch(/(?:^|;)background(?:-color)?:/);
    }
    expect(source).toContain('class="workbench bg-background"');
    expect(source).toContain('class="card bg-surface"');
  });

  it("uses a shared floating material for the drag preview without filtering every card", () => {
    expect(source).toContain('class="ghost gp-glass bg-surface shadow-float"');
    expect(source).not.toContain("backdrop-filter:");
    expect(source).not.toContain("backdrop-blur-");
  });
});

describe("TaskBoard", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskBoard.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("moves cards with pointer capture so Tauri file-drop never sees an HTML5 drag", () => {
    expect(source).toContain("setPointerCapture");
    expect(source).toContain("onpointerdown");
    expect(source).toContain("onpointerup");
    expect(source).toContain("skipClick");
    expect(source).toContain("onCardClick");
    expect(source).toContain("data-task-column");
    expect(source).toContain('draggable="false"');
    expect(source).not.toContain("ondragstart");
    expect(source).not.toContain("ondrop");
    expect(source).not.toContain("dataTransfer");
  });

  it("drops onto the column under the pointer, not only the column title", () => {
    expect(source).toContain("statusAtPoint");
    expect(source).toContain("elementFromPoint");
    expect(source).toContain("drop-target");
    expect(source).toContain("task-drag-ghost");
  });

  it("measures insert slots from the same attribute the card buttons carry", () => {
    const query = source.match(/querySelectorAll\("(\[[^\]]+\])"\)/);
    expect(query?.[1]).toBe("[data-task-card]");
    const card = source.slice(source.indexOf('data-testid="task-card"'), source.indexOf("onpointerdown"));
    expect(card).toContain("data-task-card");
    expect(card).toContain("data-card-id={card.id}");
    expect(source).toContain("insertIndexFromY");
  });

  it("moves a focused card with arrow keys", () => {
    expect(source).toContain("ArrowLeft");
    expect(source).toContain("ArrowRight");
    expect(source).toContain("neighborStatus");
    expect(source).toContain("ArrowLeft");
    expect(source).toContain("ContextMenu");
    expect(source).toContain('aria-keyshortcuts="ArrowLeft ArrowRight Delete ContextMenu"');
  });

  it("keeps the board chrome short and skips generic marketing labels", () => {
    expect(source).not.toContain("GLOBAL BOARD");
    expect(source).not.toContain("WORKSPACE BOARD");
    expect(source).not.toContain("REPOSITORY BOARD");
    expect(source).not.toContain("Drop a Git repository");
    expect(source).not.toContain("Drop a task here");
    expect(source).not.toContain("Activity inbox");
    expect(source).not.toContain("Add repositories to start creating linked tasks");
    expect(source).toContain(">New task<");
    expect(source).toContain('aria-label="Inbox"');
  });

  it("does not wipe columns before fetch and keeps previous cards while busy", () => {
    expect(source).not.toMatch(/columns\s*=\s*\{\s*\}/);
    expect(source).toContain("loading = true");
    expect(source).toContain("generation !== revision");
  });

  it("uses GitPulse pill chrome, a compact automation chip, and explains a disabled New", () => {
    expect(source).toContain("gp-btn");
    expect(source).toContain("gp-btn-primary");
    expect(source).toContain("compact");
    expect(source).toContain('data-testid="task-create-hint"');
    expect(source).toContain("unread");
    expect(source).toContain("listAttention");
    expect(source).not.toContain('class="primary"');
  });

  it("answers New task from one decision, so no two surfaces can disagree about it", () => {
    // The header button, the empty state, the column +, quick add and the
    // sheet's seed each used to decide separately. The empty state offered
    // New task where the header refused it, opening a sheet that could never
    // be saved; an empty workspace refused it everywhere with nothing on
    // screen saying why.
    expect(source).toContain("return taskCreation(target, {");
    expect(source).not.toContain("canCreate");
    expect(source).toContain("disabled={!creation.allowed}");
    expect(source).toContain("action={creation.allowed ?");
    expect(source).toContain("primary={creation.seed.primaryRepositoryId}");
    expect(source).toContain("home={creation.seed.homeWorkspaceId}");
    expect(source).toContain("...creation.seed");
    expect(source).toContain("quickAddRefusal(creation)");
    // A refusal that only disables is a dead end; every gate names its reason.
    expect(source).toContain("creation.blocked ?? creation.caveat");
    expect(source).toContain('if (!creation.allowed) { error = creation.blocked ??');
  });

  it("retries the workspace membership read that Retry used to skip", () => {
    // The effect is keyed on `scope`; refresh() does not change it, so a
    // failed membership read could never be repeated from the error banner.
    expect(source).toContain("let membershipToken = $state(0)");
    expect(source).toMatch(/\$effect\(\(\) => \{\s*membershipToken;/);
    expect(source).toMatch(/async function refresh\(\)[\s\S]*?membershipToken\+\+/);
  });

  it("renders scannable cards and collapses empty columns while exposing empty drop targets", () => {
    expect(source).toContain("cardFace");
    expect(source).toContain("visibleBoardStatuses");
    expect(source).toContain("insertionPosition");
    expect(source).toContain("shouldCommitMove");
    expect(source).not.toContain("PRIORITY_LABELS[card.priority]");
    expect(source).not.toContain("repoNames(");
  });

  it("reads layout, density, columns and card chips from the persisted preference", () => {
    // The board renders the preference; it never keeps a second copy that
    // could drift from what the View menu and the header just wrote.
    expect(source).toContain("$interfaceStore.taskLayout");
    expect(source).toContain("$interfaceStore.taskDensity");
    expect(source).toContain("$interfaceStore.taskHiddenColumns");
    expect(source).toContain("$interfaceStore.taskCardFields");
    expect(source).toContain("interfaceStore.setTaskLayout");
    expect(source).not.toMatch(/let layout = \$state/);
    expect(source).not.toMatch(/let showArchived = \$state/);
    expect(source).toContain("TaskViewMenu");
    expect(source).toContain("is-compact");
    for (const field of ["repo", "type", "owner", "due", "labels"]) {
      expect(source, `card chip ${field} is not switchable`).toContain(`cardFields.has("${field}")`);
    }
  });

  it("says what a hidden column is keeping off screen, and offers the undo", () => {
    // Hiding is cosmetic. Hiding the work in a column without saying so would
    // make a filtered board indistinguishable from an empty one.
    expect(source).toContain("hiddenColumnReport");
    expect(source).toContain('data-testid="task-hidden-columns"');
    expect(source).toContain("showAllTaskColumns");
  });

  it("adds a task from one line, and can hand it to the editor or to the model", () => {
    expect(source).toContain("TaskQuickAdd");
    expect(source).toContain("createFromQuickAdd");
    expect(source).toContain("quickAddDraft");
    expect(source).toContain("expandQuickAdd");
    // The quick-add path saves through the same write the editor uses.
    expect(source).toContain("putTask(taskWrite(");
    // The drafting mode is a preference, not a per-keystroke choice, and it is
    // read from the one place board preferences live.
    expect(source).toContain("$interfaceStore.taskQuickAddAssist");
    expect(source).toContain("interfaceStore.setTaskQuickAddAssist");
    expect(source).toContain("startRequest={enhanceStart}");
  });

  it("keeps the drafting request tied to the sheet that asked for it", () => {
    /*
     * `startRequest` is a counter each Quick Enhance instance compares against
     * its own zero, so two things have to hold or it aims at the wrong task.
     *
     * The sheet is keyed: it loads its task once, on mount, so swapping the id
     * under a live instance would leave the reader reading the previous task.
     * And opening the sheet to *read* resets the counter: left standing from a
     * drafting run, it would auto-start on whatever is opened next and spend a
     * model request nobody asked for.
     */
    expect(source).toMatch(/\{#key enhanceId\}\s*\n\s*<QuickEnhanceSheet/);
    const start = source.indexOf("async function openQuickEnhance(");
    expect(start, "openQuickEnhance is missing").toBeGreaterThan(0);
    const end = source.indexOf("\n  function refuseAtCeiling(", start);
    expect(end, "openQuickEnhance has no end anchor").toBeGreaterThan(start);
    expect(source.slice(start, end)).toContain("enhanceStart = 0");
    // And only asking raises it.
    expect([...source.matchAll(/enhanceStart\s*\+=/g)]).toHaveLength(1);
  });

  it("asks before discarding editor edits, and asks before it writes", () => {
    /*
     * The drafting mode ends by opening Quick Enhance over whatever sheet is
     * open, so it has to ask the discard question first. Reversed, a reader
     * who answers "Keep editing" has already had a task created that they
     * cannot review — the write happened, the review surface did not open.
     *
     * Sliced by index, not matched with a lazy regex across the file: a
     * `[\s\S]*?` here would happily find a `putTask` in some later function
     * and report an ordering this function does not have.
     */
    const start = source.indexOf("async function createFromQuickAdd(");
    expect(start).toBeGreaterThan(0);
    const end = source.indexOf("\n  /** Hand a typed quick-add line", start);
    expect(end, "createFromQuickAdd has no end anchor").toBeGreaterThan(start);
    const body = source.slice(start, end);
    const confirm = body.indexOf("confirmDiscard(");
    const write = body.indexOf("putTask(taskWrite(");
    expect(confirm, "the drafting mode must confirm").toBeGreaterThan(0);
    expect(write).toBeGreaterThan(0);
    expect(confirm).toBeLessThan(write);
    // And only the drafting mode asks: a plain Add never had a sheet to lose.
    expect(body).toContain('mode === "assist" && !(await confirmDiscard(');
  });

  it("gives both quick-add modes the same refusal", () => {
    // Drafting cannot open a repository picker any more than typing can, so a
    // second refusal string here would be two answers to one question.
    const start = source.indexOf("async function createFromQuickAdd(");
    const end = source.indexOf("\n  /** Hand a typed quick-add line", start);
    const body = source.slice(start, end);
    // Enumerated rather than counted: what matters is that no *hand-written*
    // message appears here, in either mode. `quickAddRefusal` owns the
    // no-repository answer and `explainError` owns the write failure; the
    // empty string is the reset before the write.
    const assigned = [...body.matchAll(/error = ([^;]+);/g)].map((match) => match[1].trim());
    expect(assigned).toEqual(['quickAddRefusal(creation)', '""', "explainError(cause)"]);
  });

  it("offers an agent handoff from the card menu and the selection bar", () => {
    expect(source).toContain("TaskHandoffSheet");
    expect(source).toContain("handoffFromTarget");
    expect(source).toContain("canHandoff: repositories.length > 0");
    expect(source).toContain("Send to agent");
  });

  it("gives the context menu the vocabulary it needs for owner and label rows", () => {
    expect(source).toContain("vocabulary: { owners: facetOptions.owners, labels: facetOptions.labels }");
    expect(source).toContain('case "due"');
    expect(source).toContain('case "owner"');
    expect(source).toContain('case "label"');
    expect(source).toContain('case "agent"');
  });

  it("adds already-open repositories from a menu and attaches them to the current workspace", () => {
    expect(source).toContain("openMembershipCandidates");
    expect(source).toContain("menuTabs");
    expect(source).toContain("Add all open");
    expect(source).toContain("Choose folder…");
    expect(source).toContain("attachToScope");
    // The read-modify-write lives in openMembership, shared with the task
    // sheet, rather than as a second copy that could drop a concurrent change.
    expect(source).toContain("attachRepositories(scope.id, ids)");
    expect(source).not.toContain("putWorkspace");
    expect(source).toContain("openTabs={openTabRefs}");
    expect(source).toContain('aria-haspopup="menu"');
    expect(source).toContain("data-add-repo");
    expect(source).toContain("emptyAddLabel");
    expect(source).toContain("pickerSelectionIds");
    expect(source).toContain('aria-controls="task-add-repo-menu"');
    expect(source).toContain("onAddMenuKey");
    expect(source).toContain('class="add-path"');
  });

  it("portals the add-repository menu out of the 188px navigator and clamps it to the viewport", () => {
    // The pre-fix menu was `position:absolute; right:0` under the heading,
    // 260px wide, inside `.navigator { width:188px; overflow:auto }`. Opening
    // it grew the sidebar's scrollWidth and cropped every row that did not
    // fit the leftover strip. That is a header-dropdown geometry, not a
    // left-rail one. The sheet's repository picker already escaped its
    // scroller this way; this menu now does the same.
    expect(source).toMatch(/use:portal=\{"body"\}\s*\n\s*use:popover=\{addMenuDismissal\}/);
    expect(source).toContain("element: addRepoTriggerEl");
    expect(source).toContain('inside: "[data-add-repo], [data-add-repo-popup]"');
    expect(source).toContain("scroll: true");
    expect(source).toContain("resize: true");
    expect(source).toContain("inset: 8");
    expect(source).toContain('class="add-menu gp-menu gp-pop fixed"');
    expect(source).toContain("data-add-repo-popup");
    expect(source).toContain("closeAddMenu({ restoreFocus: true })");
    expect(source).toContain("restoreFocusTo");
    // Replaced, not accumulated: no CSS placement beside the owner, and no
    // nowrap ellipsis that crops a path the panel was supposed to show.
    expect(source).not.toContain("clampMenuPosition");
    expect(source).not.toContain("position:absolute;right:0;top:calc(100% + 4px)");
    expect(source).not.toContain("width:min(260px,70vw)");
    expect(source).not.toContain(".navigator .add-menu button");
    const addMenuRule = source.slice(source.indexOf("<style>")).match(/\.add-menu\{([^}]*)\}/)?.[1];
    expect(source).toContain(".navigator{width:188px");
    expect(addMenuRule, "missing .add-menu rule").toBeDefined();
    expect(addMenuRule).toContain("calc(100vw - 16px)");
    expect(addMenuRule).toContain("calc(100vh - 16px)");
    expect(addMenuRule).toContain("overflow:hidden auto");
    expect(addMenuRule).not.toContain("position:absolute");
    const nameRule = source.slice(source.indexOf("<style>")).match(/\.add-name,\.add-path\{([^}]*)\}/)?.[1];
    expect(nameRule, "missing .add-name/.add-path rule").toBeDefined();
    expect(nameRule).toContain("overflow-wrap:anywhere");
    expect(nameRule).toContain("min-width:0");
    expect(nameRule).not.toContain("text-overflow:ellipsis");
    expect(nameRule).not.toContain("white-space:nowrap");
  });

  it("reaches registered repositories a workspace has not joined, and names the side effect", () => {
    // The menu used to list only open tabs and a folder picker, so a
    // registered repository that happened to be closed could not join a
    // workspace from this board at all.
    expect(source).toContain("const attachable = $derived.by(");
    expect(source).toContain("addRegistered(attachable.map((repo) => repo.id))");
    expect(source).toContain("Add all registered");
    expect(source).toContain("menuTabs.length === 0 && attachable.length === 0");
    // Adding here also writes workspace membership; the control says so.
    expect(source).toContain('`Add repository to ${title}`');
    expect(source).toContain("title={addRepoLabel}");
    expect(source).toContain("`Added ${what} to ${title}`");
  });

  it("says which workspaces are empty before the reader picks one", () => {
    expect(source).toContain("workspaceMembershipLabel(group.repository_count)");
    expect(source).toContain('group.repository_count === 0 ? " · Empty" : ""');
  });

  it("uses in-app confirms, a task context menu, liquid glass, and Quick Enhance", () => {
    expect(source).toContain("canLeave");
    expect(source).not.toContain("window.confirm");
    expect(source).toContain("oncontextmenu");
    expect(source).toContain("TaskContextMenu");
    expect(source).toContain("QuickEnhanceSheet");
    expect(source).toContain("gp-glass");
    expect(source).toContain("gp-btn-danger");
    expect(source).toContain("TaskActionDialog");
    expect(source).toContain("removeSelected");
    expect(source).toContain("isContextMenuKey");
    expect(source).toContain("aria-label=\"Task layout\"");
    expect(source).toContain("gp-liquid-tabs");
    expect(source).toContain("duplicateTitle");
    expect(source).toContain("copyCardsForAgent");
    expect(source).toContain("Copy for agent");
    expect(source).toContain("copyAgent");
    expect(source).toContain("id=\"task-search\"");
    expect(source).toContain("Unassigned");
    expect(source).toContain('e.key.toLowerCase() === "a"');
    expect(source).toContain("Open Quick Enhance?");
    expect(source).toContain("openQuickEnhance");
    expect(source).toContain("Start a duplicate");
  });

  it("routes key e and toolbar Quick Enhance through discard + session clear", () => {
    expect(source).toContain('e.key === "e"');
    expect(source).toContain("void openQuickEnhance(card.id)");
    expect(source).toContain("void openQuickEnhance(id)");
    expect(source).toContain("session = null");
    expect(source).toContain("enhanceId = id");
  });

  it("does not add a second backdrop-filter on the board scrim path", () => {
    expect(source).not.toMatch(/\bbackdrop-blur-/);
  });

  it("puts workspace names in the same label span All and repositories already use", () => {
    // The liquid selection pill is position:absolute; inset:0. In-flow text
    // paints under it and disappears — the screenshot failure was an empty
    // dark pill with only the ⋯ edit control. All and repositories wrap their
    // labels in a span so `.gp-liquid-tabs .gp-seg-btn > span` can lift them.
    expect(source).toContain("{@render scopeSelection(scope.kind === \"global\")}<span>All</span>");
    expect(source).toMatch(
      /scope\.kind === "workspace" && scope\.id === group\.id\)\}<span>\{group\.icon\} \{group\.name\}/,
    );
    expect(source).toContain("{@render scopeSelection(scope.kind === \"repository\" && scope.id === repo.id)}<span>{repo.name}</span>");
  });

  it("keeps open tasks in a tab strip with a close control beside each tab", () => {
    expect(source).toContain('role="tablist"');
    expect(source).toContain('aria-label="Open tasks"');
    expect(source).toContain("handleTablistKeydown");
    expect(source).toContain("tabindex={isActive ? 0 : -1}");
    expect(source).toContain("aria-controls={TASK_EDITOR_PANE_ID}");
    expect(source).toContain('role="tabpanel"');
    expect(source).toContain('data-testid="task-tab-close"');
    expect(source).toContain("data-open-task");
    expect(source).toContain("in progress");
    expect(source).toContain(".editor-dock");
    const strip = source.slice(source.indexOf('aria-label="Open tasks"'), source.indexOf("ScrollCue target={tabStrip}"));
    expect(strip).toContain('role="tab"');
    expect(strip).toContain('data-testid="task-tab-close"');
    expect(strip.indexOf("</div>")).toBeLessThan(strip.indexOf('data-testid="task-tab-close"'));
    expect(source).toContain("var(--mac-fill-surface-hover, rgb(var(--c-surface-hover)))");
  });
});

/**
 * The board had a panel called Archive, a Restore inside it, and no Archive
 * verb anywhere — not on a card, not in the context menu, not on the
 * selection bar. "Archive this task" had no answer in the product; the only
 * route was `Move to… › Done`, and nothing said the two were the same thing.
 */
describe("the board can archive a task", () => {
  it("offers the verb from the card menu and from the selection bar", () => {
    expect(source, "the card menu's archive row must reach the board").toContain('case "archive":');
    expect(source).toContain("void archiveCards(cards)");
    expect(source).toContain('data-testid="task-archive-selected"');
    expect(source).toContain("void archiveCards(selectedCards)");
  });

  it("takes what archiving is from the seam rather than deciding here", () => {
    expect(source).toContain("archiveAction()");
    expect(source).toContain("archivable(cards)");
    expect(source).toContain("archiveState(selectedCards)");
    // The one owner. A status literal here is a second decision about what
    // archived means, in a place the upstream switch would not reach.
    expect(source).not.toMatch(/\bstatus\b\s*[!=]==?\s*ARCHIVE_STATUS/);
  });

  it("runs it through the board's one inline write path, like Move to", () => {
    // Not a private `putTask`: archiving gets the same revision checks,
    // uncertainty handling and receipt identity every other bulk edit has.
    const archive = source.slice(source.indexOf("async function archiveCards"), source.indexOf("/** Defaults a quick-added task"));
    expect(archive).toContain("applyTaskAction(wanted, archiveAction())");
    expect(archive).not.toContain("putTask");
    expect(archive).not.toContain("new TaskBatch");
    // `patchCards` and Archive are two callers of one function, not two paths.
    expect(source).toContain("await applyTaskAction(cards, {kind:\"update\", changes:patch});");
  });

  it("writes only the tasks archiving would change, and says what it skipped", () => {
    const archive = source.slice(source.indexOf("async function archiveCards"), source.indexOf("/** Defaults a quick-added task"));
    expect(archive).toContain("const wanted = archivable(cards);");
    expect(archive, "the batch must get the filtered selection").toContain("applyTaskAction(wanted,");
    expect(archive, "the batch must not get the raw selection").not.toContain("applyTaskAction(cards,");
    // A skipped task is not a failed one, and must not be folded into the
    // count `applyUpdate` announces — that count has to match the receipt.
    expect(archive).toContain("already archived");
    expect(archive).toContain("const already = cards.length - wanted.length;");
  });

  it("refuses the write that would store the status already there", () => {
    const bar = source.slice(source.indexOf('data-testid="task-archive-selected"'), source.indexOf("Trash2 size={12} /> Delete"));
    expect(bar).toContain('selectionArchived === "all"');
    expect(bar).toContain("disabled={busy ||");
    // The disabled control still says why, rather than sitting there dead.
    expect(bar).toContain("Already in ${STATUS_LABELS[ARCHIVE_STATUS]}");
  });

  /**
   * Two unqualified uses of "archived" on one screen. The navigator's
   * checkbox hides *workspaces*; the header's Archive holds completed
   * *tasks*. A reader looking for a way to archive a task finds a checkbox
   * called "Show archived" that does nothing they wanted, which is one of
   * the reasons the page read as one feature with a broken control.
   */
  it("does not call two different things archived on the same screen", () => {
    expect(source).toContain("Show archived workspaces");
    expect(source, "a bare 'Show archived' is the ambiguous label")
      .not.toMatch(/>Show archived<\/label>/);
  });

  it("names the column the header's Archive toggle actually holds", () => {
    // "Archive — completed tasks in this scope" named a category; a reader
    // hunting for the verb needed the mechanism instead.
    expect(source).toContain("aria-controls=\"task-archive-dock\"");
    expect(source).toContain("Archive — tasks in this scope that reached ${STATUS_LABELS[ARCHIVE_STATUS]}");
    expect(source).not.toContain("Archive — completed tasks in this scope");
  });

  it("dims and disables the board's quick-add row while the task editor sheet is open", () => {
    expect(source).toContain('data-testid="task-quick-add-row"');
    expect(source).toContain("class:dimmed={taskTabs.tabs.length > 0}");
    expect(source).toContain("disabled={busy || taskTabs.tabs.length > 0}");
    expect(source).toMatch(/\.quick-add-row\.dimmed\{opacity:\.45;pointer-events:none;/);
  });
});

