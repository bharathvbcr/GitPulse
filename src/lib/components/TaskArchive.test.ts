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

  it("hands both actions to the board as a TaskAction", () => {
    expect(source).toContain("onaction([...chosen], restoreAction(restoreTo))");
    expect(source).toContain('onaction([...chosen], { kind: "delete" })');
  });

  it("stops at the same selection cap the batch enforces, rather than inventing one", () => {
    expect(source).toContain("MAX_TASK_SELECTION");
    expect(source).toContain("chosen.length > MAX_TASK_SELECTION");
    expect(source).not.toMatch(/const\s+\w*[Cc]ap\w*\s*=\s*\d+/);
  });
});

describe("the dock reads only what the store actually has", () => {
  it("asks for the archived status by name from one place", () => {
    expect(source).toContain("listTasks(scope, ARCHIVE_STATUS, query, cursor)");
    // The status literal lives in `taskArchive.ts` alone. Spelling it here
    // too is how the dock and the board's note end up disagreeing about
    // which column the archive holds.
    expect(source).not.toMatch(/["']done["']/);
  });

  // `updated_at` is the last write. The store keeps no completion stamp and
  // `items.list` takes no sort parameter, so a row that said "Completed 2h
  // ago" would be inventing both the event and the ordering.
  it("labels the timestamp as the last update, never as a completion time", () => {
    expect(source).toContain("Updated {formatRelativeTime(card.updated_at, now)}");
    expect(source).not.toMatch(/Completed \{/);
    expect(source).not.toContain("completed_at");
    expect(source).not.toMatch(/\.sort\(/);
  });
});

describe("the dock never presents a page as the whole archive", () => {
  it("renders the summary that carries both numbers", () => {
    expect(source).toContain("archiveSummary(result ? rows.length : null, result?.total ?? 0)");
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

describe("the dock and the board agree about the Done column", () => {
  it("reads the board's own hidden-column preference instead of a second flag", () => {
    expect(source).toContain("boardPresence(hiddenColumns)");
    expect(source).toContain("{presence.sentence}");
    expect(source).toContain("{presence.actionLabel}");
    expect(board).toContain("ontogglecolumn={() => interfaceStore.toggleTaskColumn(ARCHIVE_STATUS)}");
    const mount = board.slice(board.indexOf("<TaskArchive"), board.indexOf("/>", board.indexOf("<TaskArchive")));
    expect(mount).toContain("{hiddenColumns}");
    expect(board).toContain("const hiddenColumns = $derived($interfaceStore.taskHiddenColumns);");
  });

  it("is reachable from the board header and from the hidden-column note", () => {
    expect(board).toContain('aria-label="Archive"');
    expect(board).toContain("aria-pressed={showArchive}");
    expect(board).toContain("offersArchive(hiddenWork.statuses)");
    expect(board).toContain("Open archive");
  });

  // The badge is the server's total for the scope, not the loaded page size:
  // a board showing 30 of 412 completed tasks must not advertise 30.
  it("badges the server's completed total", () => {
    expect(board).toContain("displayColumns[ARCHIVE_STATUS]?.total ?? 0");
    expect(board).toContain("{#if completedCount > 0}<span class=\"gp-pill\">{completedCount}</span>{/if}");
  });
});

describe("the dock stays current after a write", () => {
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
    expect(calls.length, "taskWritten must be called from all three write sites").toBe(3);
    expect(board).toContain("if (done) { taskWritten(); await loadBoard(); }");
    expect(board).toContain("if (ids.length) taskWritten();");
    expect(board).toContain("function onEditorSaved(saved: Task) {\n    taskWritten();");
    // Not from loadBoard: that also runs on every debounced keystroke, and
    // would reset the dock's paging and selection under a reader who is
    // only typing in the board's search box.
    const loadBoard = board.slice(board.indexOf("async function loadBoard("), board.indexOf("function taskWritten("));
    expect(loadBoard).not.toContain("taskWritten()");
  });
});
