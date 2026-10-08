import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskArchive.svelte", import.meta.url), "utf8");
const board = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

describe("TaskArchive", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskArchive.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });

  it("lets the shared material system paint the dock, like the Inbox beside it", () => {
    const rule = source.match(/\.archive\{([^}]*)\}/)?.[1];
    expect(rule, "missing .archive rule").toBeDefined();
    expect(rule, ".archive covers the shared material").not.toMatch(/(?:^|;)background(?:-color)?:/);
    expect(source).toContain('class="archive gp-glass bg-surface"');
  });
});

/**
 * The defect the panel was reported for. With thirty rows loaded on a 1024x768
 * window the panel's visible box ended at y=533 and the Restore/Delete bar
 * rendered at y=640 — inside the panel's own scroll container, 107px below its
 * fold, at scrollTop 0. Ticking a checkbox changed nothing a reader could see.
 *
 * The cause was two nested scrollers with independent caps: the panel capped
 * at 46vh, the row list capped at 280px inside it. The list won and the
 * actions were pushed out. These are source contracts rather than a rendered
 * measurement because jsdom computes no layout; the measured proof is the
 * tasks harness, which drives the real board in a real browser.
 */
describe("a selection's actions are never off screen", () => {
  const styles = source.slice(source.lastIndexOf("<style>"));
  // Escapes the whole selector, not just its leading dot: `.archive > *`
  // carries three regex metacharacters, and a helper that silently matched
  // nothing would report every rule below as absent — or, worse, as present.
  const rule = (selector: string) => {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = styles.match(new RegExp(`(?:^|[;}\\s])${escaped}\\{([^}]*)\\}`, "m"));
    expect(match, `no CSS rule for \`${selector}\``).not.toBeNull();
    return match?.[1] ?? "";
  };

  /**
   * The markup of one element, from `open` to its own closing `</div>`.
   *
   * Counts nesting rather than slicing to the next landmark, because the
   * question here is whether a control sits *inside* the scroller, and a
   * slice that overshot the closing tag answers "yes" either way.
   */
  const balanced = (open: string) => {
    const start = source.indexOf(open);
    expect(start, `no element matching \`${open}\``).toBeGreaterThan(-1);
    let depth = 0;
    for (let at = start; at < source.length; at += 1) {
      if (source.startsWith("<div", at)) depth += 1;
      else if (source.startsWith("</div>", at)) {
        depth -= 1;
        if (depth === 0) return source.slice(start, at + 6);
      }
    }
    throw new Error(`\`${open}\` is never closed`);
  };

  it("keeps exactly one scroller between the panel and its rows", () => {
    // `.entries` is it. The panel keeps `overflow:auto` only as the fallback
    // for a window too short for its own chrome, where the sticky action bar
    // below is what holds the promise.
    expect(rule(".entries")).toContain("overflow:auto");
    expect(rule(".entries")).toContain("min-height:0");
    // No second cap racing the panel's.
    expect(rule(".entries")).not.toMatch(/max-height/);
    expect(rule(".archive")).toContain("max-height:");
  });

  it("makes the row list the only part of the panel that shrinks", () => {
    expect(rule(".archive")).toContain("display:flex");
    expect(rule(".archive")).toContain("flex-direction:column");
    expect(rule(".archive > *")).toContain("flex:0 0 auto");
    // Shrink, but never grow: three archived tasks draw a short panel.
    expect(rule(".entries")).toContain("flex:0 1 auto");
  });

  it("pins the action bar for the case the list cannot shrink any further", () => {
    expect(rule(".actions")).toContain("position:sticky");
    expect(rule(".actions")).toContain("bottom:0");
  });

  // Sticky over scrolling rows needs a ground, and the ground has to be the
  // one `html.macos` can remap. A local `rgb(var(--c-surface))` is invisible
  // to that remap and paints an opaque bar across a glass panel — the same
  // shape `scripts/mac-material-contract.test.ts` guards the whole components
  // tree against, asserted here beside the rules that need it.
  it("takes the pinned bar's ground from the shared material", () => {
    expect(rule(".actions"), ".actions must not paint its own token fill")
      .not.toMatch(/(?:^|;)background(?:-color)?:/);
    expect(source).toContain('class="actions bg-surface"');
  });

  it("spends no fixed row on a control that belongs in the list", () => {
    // "Load more" was a fixed row competing with the list for the panel's
    // height, and it extends the rows — it belongs after the last one.
    //
    // The region is the *balanced* `.entries` element, not everything up to
    // the next landmark: a slice that ran past the closing tag would contain
    // "Load more" whether or not it was still inside the scroller, which is
    // exactly the move this asserts against.
    const entries = balanced('<div class="entries">');
    expect(entries, "Load more must extend the rows from inside the scroller").toContain("Load more");
  });

  /**
   * The select-all head is the deliberate exception, and the comment in the
   * component is the reason. Sticky inside the scroller saves the list 31px
   * and costs legibility: `bg-surface` is a 50%-alpha material on macOS, and
   * rows sliding under a half-transparent bar put a task title and the
   * select-all label in the same pixels — observed, not predicted. Outside,
   * `.entries` clips its own overflow and no row can reach it.
   */
  it("keeps the select-all head out of the scroller rather than floating it", () => {
    const entries = balanced('<div class="entries">');
    expect(entries, "a row can slide under a sticky head; it cannot reach a sibling")
      .not.toMatch(/<div class="list-head[ "]/);
    expect(rule(".list-head")).not.toContain("position:sticky");
    // And it must not have acquired its own ground, which would only be
    // needed by something rows can pass beneath.
    expect(rule(".list-head")).not.toMatch(/(?:^|;)background(?:-color)?:/);
    expect(source).not.toContain('class="list-head bg-surface"');
  });
});

describe("the dock says how a task gets into it", () => {
  // It used to say so only in the empty state — the one moment a reader has
  // no archived work to ask about. A panel called Archive that offers Restore
  // and never names the thing that archives a task is the report this fixed.
  it("carries the rule as header chrome, not as an empty state", () => {
    expect(source).toContain('data-testid="task-archive-rule"');
    const rule = source.indexOf('data-testid="task-archive-rule"');
    const empty = source.indexOf("{#if summary.pending}");
    expect(rule).toBeGreaterThan(-1);
    expect(rule, "the rule must be above the list, not inside a branch").toBeLessThan(empty);
  });

  it("teaches the same rule whether or not it is empty, from one constant", () => {
    expect(source).toContain("ARCHIVE_RULE");
    // Two renderings, one owner: the header line and the empty state.
    expect(source.match(/ARCHIVE_RULE/g)?.length).toBeGreaterThanOrEqual(3);
    // Still no second spelling of the status here.
    expect(source).not.toMatch(/["']done["']/);
  });
});

describe("the dock owns no write path", () => {
  // Restore and delete are the board's existing batch, run through the
  // board's existing confirm dialog. A private `putTask` here would be a
  // second owner of "move a task" with its own revision handling, and the
  // interrupted-write recovery the dialog provides would not cover it.
  it("never calls the store's mutations itself", () => {
    for (const forbidden of ["putTask", "deleteTask", "taskWrite", "new TaskBatch"]) {
      expect(source, `TaskArchive must not call ${forbidden}`).not.toContain(forbidden);
    }
  });

  it("hands every action to the board", () => {
    expect(source).toContain("onaction([...chosen], restoreAction())");
    expect(source).toContain('onaction([...chosen], { kind: "delete" })');
    // Bringing a deleted task back is the board's bounded restore pass, not
    // a store call made from here.
    expect(source).toContain("await onrestoredeleted([...chosen])");
    expect(source).not.toContain("restoreDeletedTask");
    expect(board).toContain("onrestoredeleted={restoreDeleted}");
  });

  it("stops at the same selection cap the batch enforces, rather than inventing one", () => {
    // Two writers, two caps, both imported: the batch's for archive changes
    // and the delete pass's for bringing deleted tasks back.
    expect(source).toContain('view === "deleted" ? MAX_DELETE_BATCH : MAX_TASK_SELECTION');
    expect(source).toContain("chosen.length > cap");
    expect(source).not.toMatch(/const\s+\w*[Cc]ap\w*\s*=\s*\d+/);
  });
});

describe("the dock reads only what the store actually has", () => {
  // The archive is the `archived` flag, ordered by the store's own
  // completion time; the deleted list is the deleted rows, most recently
  // deleted first. Never a status, and never a client-side sort of one page.
  it("asks the store for each list by its own filter and order", () => {
    expect(source).toContain('{ archived: true, order: "completed" }');
    expect(source).toContain('{ deleted: true, order: "updated" }');
    expect(source).toContain("listTasks(scope, null, query, cursor, 30,");
    expect(source).not.toMatch(/["']done["']/);
    expect(source).not.toMatch(/\.sort\(/);
  });

  // The stamp states the completion the list is ordered by, and for a task
  // archived without being finished says so rather than inventing one.
  it("takes the row stamp from the seam", () => {
    expect(source).toContain("archiveStamp(card, relative)");
    expect(source).not.toMatch(/Completed \{/);
  });
});

describe("the dock never presents a page as the whole archive", () => {
  it("renders the summary that carries both numbers", () => {
    // Less any task the board is about to delete in its undo window: those
    // have left the dock too, and the total must not still count them.
    expect(source).toContain("archiveSummary(result ? rows.length : null, Math.max(0, (result?.total ?? 0) - hiddenHere), view)");
    expect(source).toContain("{summary.text}");
    expect(source).toContain("summary.partial");
    expect(source).toContain('data-testid="task-archive-summary"');
  });

  // The dock defers its query while the window is in the background, so an
  // empty list is routinely a query that has not run. The empty state is
  // gated on a successful read rather than on `rows.length`, or a paused
  // dock claims the scope has no completed work.
  it("holds its empty state back until a read has actually succeeded", () => {
    expect(source).toContain("{#if summary.pending}");
    const empty = source.indexOf("{#if summary.pending}");
    const rowsEmpty = source.indexOf("{:else if rows.length === 0}");
    expect(empty, "the pending branch must precede the empty branch").toBeGreaterThan(-1);
    expect(rowsEmpty).toBeGreaterThan(empty);
    expect(source).toContain("Paused while this window is in the background.");
    expect(source).not.toContain("{#if rows.length === 0}");
  });

  it("offers more only while the server says there is more", () => {
    expect(source).toContain("{#if result?.next_cursor}");
    expect(source).toContain("Load more");
  });

  // "Select all" over a paged list would claim rows the dock has not loaded,
  // and the action would then silently apply to fewer tasks than the label
  // promised. The count is in the label instead.
  it("scopes the bulk checkbox to the rows it has, and says so", () => {
    expect(source).toContain("Select the {rows.length} loaded");
    expect(source).toContain("selected = on ? new Set(rows.map((card) => card.id)) : new Set()");
  });
});

describe("the archive is separate from the Done column", () => {
  // An archived task leaves the board whatever its status, and a Done one
  // that is not archived stays on it. Every column read says so.
  it("reads every board column without archived tasks", () => {
    expect(board).toContain("const BOARD: TaskListFilter = { archived: false };");
    // The filter is the contract; the page size is the board's to choose.
    expect(board).toMatch(/listTasks\(target, status, query, undefined, \w+, BOARD\)/);
    expect(board).toMatch(/listTasks\(scope, status, search, cursor, \w+, BOARD\)/);
    expect(board.match(/listTasks\(/g)?.length, "a column read without the filter would draw archived cards").toBe(3);
  });

  it("no longer couples the dock to the Done column's visibility", () => {
    const mount = board.slice(board.indexOf("<TaskArchive"), board.indexOf("/>", board.indexOf("<TaskArchive")));
    expect(mount).not.toContain("hiddenColumns");
    expect(mount).not.toContain("ontogglecolumn");
    expect(board).not.toContain("offersArchive");
    expect(source).not.toContain("boardPresence");
  });

  it("is reachable from the board header", () => {
    expect(board).toContain('aria-label="Archive"');
    expect(board).toContain("aria-pressed={showArchive}");
  });

  // The badge is the server's total for the scope, not the loaded page size:
  // a board with 412 archived tasks must not advertise the 30 it loaded.
  // The board's search does not narrow it: the dock has its own search, and a
  // badge that shrank while the board was filtered would misreport the archive.
  it("badges the server's archived total for the scope, whatever the board search", () => {
    expect(board).toContain("listTasks(target, null, \"\", undefined, 1, { archived: true })");
    expect(board).toContain("archivedCount = archive.total;");
    expect(board).toContain("{#if archivedCount > 0}<span class=\"gp-pill\">{archivedCount}</span>{/if}");
  });
});

describe("the dock stays current after a write", () => {
  // A write used to reload the dock from its first page, because the store's
  // cursor runs forward only. Re-reading the pages already loaded, each from
  // where the fresh previous one ends, keeps the reader's place.
  it("refreshes the pages it has in place instead of starting over", () => {
    expect(source).toContain("await refreshPages(mode === \"first\" || !result ? 1 : pages, read)");
    expect(source).toContain('untrack(() => { void load("refresh"); });');
    // Scope, search or view changing is the one case that starts over.
    expect(source).toContain('untrack(() => { void load("first"); });');
  });

  // `workbench-changed` is a delivery hint the host may lose, so the board
  // also tells the dock directly at each point where it knows a write
  // committed. Either path alone leaves a restored task sitting in the list.
  it("listens for the live event and accepts a direct nudge", () => {
    expect(source).toContain('listen("workbench-changed", schedule)');
    expect(source).toContain("refreshToken");
    expect(board).toContain("refreshToken={archiveToken}");
  });

  it("nudges from every place the board finishes a write, and nowhere else", () => {
    const calls = board.match(/taskWritten\(\);/g) ?? [];
    expect(calls.length, "taskWritten must be called from all six write sites").toBe(6);
    // Bringing deleted tasks back is a write too.
    const restoring = board.slice(board.indexOf("async function restoreDeleted("), board.indexOf("async function archiveDockAction("));
    expect(restoring).toContain("if (result.deleted.length) { taskWritten(); void loadBoard(); }");
    expect(board).toContain("if (done) { taskWritten(); await loadBoard(); }");
    expect(board).toContain("if (ids.length) taskWritten();");
    expect(board).toContain("function onEditorSaved(saved: Task) {\n    taskWritten();");
    // Linking tasks to the GitHub issues filed from them writes their labels,
    // both in the run and in its Retry link.
    const filing = board.slice(board.indexOf("async function fileTaskIssues("), board.indexOf("async function retryIssueLink("));
    expect(filing).toContain("if (linked.length) {\n        taskWritten();");
    const relinking = board.slice(board.indexOf("async function retryIssueLink("), board.indexOf("async function onMenuAction("));
    expect(relinking).toContain("toastStore.success(announce);\n      taskWritten();");
    // Not from loadBoard: that also runs on every debounced keystroke, and
    // would reset the dock's paging and selection under a reader who is
    // only typing in the board's search box.
    const loadBoard = board.slice(board.indexOf("async function loadBoard("), board.indexOf("function taskWritten("));
    expect(loadBoard).not.toContain("taskWritten()");
  });
});
