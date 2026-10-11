import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { compile } from "svelte/compiler";

const source = readFileSync(new URL("./TaskArchive.svelte", import.meta.url), "utf8");
const board = readFileSync(new URL("./TaskBoard.svelte", import.meta.url), "utf8");

/**
 * The archive dock's behaviour is rendered and driven by the tasks harness
 * (`harness/tasksChecks.js`, "The archive, and what it is willing to claim"
 * and "Archiving a task"): the badge is the server's total and ignores the
 * board search, archived work is off the Done column, the list is ordered and
 * stamped by completion, the rule line shows with rows on screen, a partial
 * page states both numbers, Load more lives inside the scroller and goes when
 * the store has no next page, select-all names and selects exactly the loaded
 * rows, the action bar and select-all head stay visible while only the rows
 * scroll, restore goes through the board's confirm dialog and refreshes the
 * loaded pages in place, filters are applied locally and never sent to the
 * store, a backgrounded window never reports an unread archive as empty, and
 * the Deleted view restores by id with no second Delete.
 *
 * What stays here is what a rendered page cannot show.
 */
describe("TaskArchive", () => {
  it("compiles", () => {
    const { warnings } = compile(source, { generate: "client", filename: "TaskArchive.svelte" });
    expect(warnings.filter((w) => w.code !== "css-unused-selector")).toEqual([]);
  });
});

/**
 * Grounds. On macOS `html.macos` remaps `bg-surface` to a translucent
 * material; a local `rgb(var(--c-surface))` is invisible to that remap and
 * paints an opaque slab over a glass panel. The harness runs without the
 * macOS class, where both spellings paint the same pixels, so this is the
 * one place the difference can be caught. Same shape as
 * `scripts/mac-material-contract.test.ts`, asserted beside the rules that need it.
 */
describe("the dock takes its grounds from the shared material", () => {
  const styles = source.slice(source.lastIndexOf("<style>"));
  const rule = (selector: string) => {
    const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const match = styles.match(new RegExp(`(?:^|[;}\\s])${escaped}\\{([^}]*)\\}`, "m"));
    expect(match, `no CSS rule for \`${selector}\``).not.toBeNull();
    return match?.[1] ?? "";
  };
  const ownFill = /(?:^|;)background(?:-color)?:/;

  it("paints the panel and the pinned action bar with the shared class, not a token fill", () => {
    expect(rule(".archive")).not.toMatch(ownFill);
    expect(source).toContain('class="archive gp-glass bg-surface"');
    expect(rule(".actions")).not.toMatch(ownFill);
    expect(source).toContain('class="actions bg-surface"');
  });

  it("gives the select-all head no ground, because no row can pass beneath it", () => {
    expect(rule(".list-head")).not.toMatch(ownFill);
    expect(source).not.toContain('class="list-head bg-surface"');
  });
});

/**
 * Ownership contracts. The harness proves restore and delete go through the
 * board's dialog; these keep the dock from growing a second write path or a
 * second policy beside it, which a rendered run cannot distinguish.
 */
describe("the dock owns no write path or policy of its own", () => {
  it("never calls the store's mutations itself", () => {
    for (const forbidden of ["putTask", "deleteTask", "taskWrite", "new TaskBatch", "restoreDeletedTask"]) {
      expect(source, `TaskArchive must not call ${forbidden}`).not.toContain(forbidden);
    }
  });

  it("stops at the caps the batch and the delete pass enforce, rather than inventing one", () => {
    // A cap of 100 (or 50) cannot be reached with the harness fixture's 34
    // archived tasks; the claim is that both are imported, never re-declared.
    expect(source).toMatch(/import \{[^}]*\bMAX_TASK_SELECTION\b[^}]*\} from "\.\.\/workbench\/taskActions"/);
    expect(source).toMatch(/import \{[^}]*\bMAX_DELETE_BATCH\b[^}]*\} from "\.\.\/workbench\/taskDelete"/);
    expect(source).not.toMatch(/const\s+\w*[Cc]ap\w*\s*=\s*\d+/);
  });

  it("never sorts or re-spells the status of a page it was handed", () => {
    // The fixture returns rows already in the store's order, so a client-side
    // sort of one page would render identically there and be wrong on page two.
    expect(source).not.toMatch(/\.sort\(/);
    expect(source).not.toMatch(/["']done["']/);
  });
});

/**
 * `workbench-changed` is a delivery hint the host may lose, so the board also
 * nudges the dock directly wherever it knows a write committed. In the harness
 * both paths fire for every write, so a rendered refresh cannot tell which one
 * did the work; losing either is only visible here.
 */
describe("the dock stays current after a write", () => {
  it("subscribes to the live event and accepts the board's direct nudge", () => {
    expect(source).toContain("loader.connect(");
    expect(readFileSync(new URL("../workbench/liveLoader.ts", import.meta.url), "utf8")).toContain("changed: schedule");
    expect(source).toContain("refreshToken");
    expect(board).toContain("refreshToken={archiveToken}");
  });

  it("nudges from every place the board finishes a write, and not from loadBoard", () => {
    const calls = board.match(/taskWritten\(\);/g) ?? [];
    expect(calls.length, "taskWritten must be called from all six write sites").toBe(6);
    // Not from loadBoard: that also runs on every debounced keystroke, and
    // would reset the dock's paging and selection under a reader who is
    // only typing in the board's search box.
    const loadBoard = board.slice(board.indexOf("async function loadBoard("), board.indexOf("function taskWritten("));
    expect(loadBoard).not.toContain("taskWritten()");
  });
});

/**
 * Claims the harness fixture cannot discriminate. Its store matches the whole
 * query as one substring, so OR alternatives and phrases never reach the
 * dock's own ranking there; its scope always holds archived work, so the
 * empty state is never drawn; and it has no undo window open while the dock
 * is, so the pending-delete adjustment is never exercised.
 */
describe("what the fixture cannot reach", () => {
  it("checks OR alternatives and phrases locally, never relaxed into partial matches", () => {
    expect(source).toContain('{ order: "board", relax: false }');
  });

  it("teaches the archive rule in the empty state from the same constant as the header", () => {
    expect(source).toContain("Nothing here yet. ${ARCHIVE_RULE}");
    expect(source).toContain("ARCHIVE_RULE :");
  });

  it("does not count tasks in the board's undo window toward the archive total", () => {
    expect(source).toContain("hiddenHere = $derived((result?.items.length ?? 0) - live.length)");
    expect(source).toContain("Math.max(0, (result?.total ?? 0) - hiddenHere)");
  });
});
