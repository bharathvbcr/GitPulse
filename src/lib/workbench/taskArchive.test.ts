import { describe, expect, it } from "vitest";
import {
  ARCHIVE_RULE,
  archivable,
  archiveAction,
  archiveStamp,
  archiveState,
  archiveSummary,
  isArchived,
  refreshPages,
  restoreAction,
} from "./taskArchive";
import { STATUSES, type TaskStatus } from "./vocabulary";

const at = (archived: boolean, id: string = String(archived), status: TaskStatus = "ready") => ({ id, archived, status });

describe("what the archive holds", () => {
  // Archived is its own flag now. Every status can be archived and every
  // status can be live; the status says nothing about which.
  it("reads archived off the flag, whatever the status", () => {
    for (const status of STATUSES) {
      expect(isArchived(at(true, "a", status))).toBe(true);
      expect(isArchived(at(false, "a", status))).toBe(false);
    }
  });

  it("teaches the mechanism, and no longer claims Done archives a task", () => {
    expect(ARCHIVE_RULE).toMatch(/Archive/);
    expect(ARCHIVE_RULE).toMatch(/Restore/);
    expect(ARCHIVE_RULE).not.toMatch(/reaches/);
  });
});

describe("archiving and restoring", () => {
  it("writes the flag alone, never the status", () => {
    expect(archiveAction()).toEqual({ kind: "update", changes: { archived: true } });
    expect(restoreAction()).toEqual({ kind: "update", changes: { archived: false } });
    // A restore that moved the status would put a Done task back in Ready, or
    // a Ready one in Done: the defect the separate flag exists to remove.
    for (const action of [archiveAction(), restoreAction()]) {
      expect(action.kind === "update" && "status" in action.changes).toBe(false);
    }
  });

  it("separates none, some and all so a mixed selection is still offered", () => {
    expect(archiveState([])).toBe("none");
    expect(archiveState([at(false, "a"), at(false, "b")])).toBe("none");
    expect(archiveState([at(false, "a"), at(true, "b")])).toBe("some");
    expect(archiveState([at(true, "a"), at(true, "b")])).toBe("all");
  });

  // The defect this function was added for: archiving a mixed selection
  // re-wrote the already-archived cards, spending a revision each to store
  // the value they already had and handing the batch more writes to fail on.
  it("writes only the tasks archiving would change, in the order selected", () => {
    const selection = [at(false, "a"), at(true, "b"), at(false, "c", "done")];
    expect(archivable(selection).map((task) => task.id)).toEqual(["a", "c"]);
    expect(archivable([at(true, "b")])).toEqual([]);
    expect(archivable([])).toEqual([]);
    // Never a superset, and never the caller's own array.
    const untouched = [at(false, "a"), at(false, "c")];
    for (const input of [selection, untouched, []]) {
      expect(archivable(input)).not.toBe(input);
      expect(archivable(input).every((task) => input.includes(task))).toBe(true);
    }
    expect(archivable(untouched)).toEqual(untouched);
  });

  // Two readings of the same question, used together — the menu disables on
  // one, the board filters on the other. They may not disagree.
  it("agrees with archiveState about whether there is anything to write", () => {
    const cases = [[], [at(false, "a")], [at(true, "a")], [at(false, "a"), at(true, "b")]];
    for (const selection of cases) {
      const empty = archivable(selection).length === 0;
      const nothingToDo = selection.length === 0 || archiveState(selection) === "all";
      expect(empty, `disagreed on ${JSON.stringify(selection)}`).toBe(nothingToDo);
    }
  });
});

describe("the summary line", () => {
  // The whole reason this function exists: a loaded page and a server total
  // are reported together whenever they differ, so 30 rows on screen can
  // never read as the complete archive.
  it("carries both numbers while the archive is only partly loaded", () => {
    expect(archiveSummary(30, 412)).toEqual({ text: "Showing 30 of 412 archived tasks.", partial: true, pending: false });
    expect(archiveSummary(1, 2)).toEqual({ text: "Showing 1 of 2 archived tasks.", partial: true, pending: false });
  });

  it("drops the second number only when everything is on screen", () => {
    expect(archiveSummary(412, 412)).toEqual({ text: "412 archived tasks.", partial: false, pending: false });
    expect(archiveSummary(1, 1)).toEqual({ text: "1 archived task.", partial: false, pending: false });
  });

  it("says nothing is here only for an actually empty scope", () => {
    expect(archiveSummary(0, 0)).toEqual({ text: "No archived tasks in this scope.", partial: false, pending: false });
    expect(archiveSummary(0, 7)).toEqual({ text: "Showing 0 of 7 archived tasks.", partial: true, pending: false });
  });

  // The dock defers its query while its window is in the background, and "0
  // rows loaded" once rendered as an empty archive under a header badge
  // reading 34. A query that never ran must not answer like one that ran.
  it("keeps an unread archive distinct from an empty one", () => {
    expect(archiveSummary(null, 0)).toEqual({ text: "Archived tasks have not loaded yet.", partial: false, pending: true });
    expect(archiveSummary(null, 34)).toEqual({ text: "Archived tasks have not loaded yet.", partial: false, pending: true });
    expect(archiveSummary(0, 0).pending).toBe(false);
    expect(archiveSummary(null, 34).text).not.toBe(archiveSummary(0, 0).text);
  });

  it("never reports fewer tasks in total than it is showing", () => {
    expect(archiveSummary(30, 4)).toEqual({ text: "30 archived tasks.", partial: false, pending: false });
    expect(archiveSummary(30, -1)).toEqual({ text: "30 archived tasks.", partial: false, pending: false });
  });

  it("tolerates fractional and negative counts from a hostile payload", () => {
    expect(archiveSummary(2.7, 9.9)).toEqual({ text: "Showing 2 of 9 archived tasks.", partial: true, pending: false });
    expect(archiveSummary(-5, 0)).toEqual({ text: "No archived tasks in this scope.", partial: false, pending: false });
  });

  it("names what it counts in the deleted view", () => {
    expect(archiveSummary(3, 3, "deleted").text).toBe("3 deleted tasks.");
    expect(archiveSummary(null, 3, "deleted").text).toBe("Deleted tasks have not loaded yet.");
    expect(archiveSummary(0, 0, "deleted").text).toBe("No deleted tasks in this scope.");
  });
});

describe("the row stamp", () => {
  const relative = (at: number) => `t${at}`;
  it("states the completion the dock is ordered by", () => {
    expect(archiveStamp({ completed_at: 50, updated_at: 90 }, relative)).toBe("Completed t50");
  });
  // A task archived out of Ready was never completed. Printing its last
  // update as a completion would put a time on the screen the store does not have.
  it("never invents a completion time", () => {
    expect(archiveStamp({ completed_at: null, updated_at: 90 }, relative)).toBe("Not completed · updated t90");
  });
});

describe("refreshing in place", () => {
  type Row = { id: string };
  /** A keyed store: each cursor names the last id read, like `items.list`. */
  function store(ids: string[], size: number) {
    const reads: (string | undefined)[] = [];
    const read = async (cursor: string | undefined) => {
      reads.push(cursor);
      const from = cursor === undefined ? 0 : ids.indexOf(cursor) + 1;
      const items = ids.slice(from, from + size).map((id) => ({ id }));
      const more = from + size < ids.length;
      return { items, total: ids.length, next_cursor: more ? items[items.length - 1].id : null };
    };
    return { read, reads };
  }

  it("re-reads exactly the pages that were on screen", async () => {
    const { read, reads } = store(["a", "b", "c", "d", "e", "f", "g"], 2);
    const fresh = await refreshPages<Row>(3, read);
    expect(fresh.items.map((row) => row.id)).toEqual(["a", "b", "c", "d", "e", "f"]);
    expect(fresh.next_cursor).toBe("f");
    expect(fresh.pages).toBe(3);
    // Each page from where the fresh previous one ended, not from a stale cursor.
    expect(reads).toEqual([undefined, "b", "d"]);
  });

  // A row restored out of the middle shifts every later page up by one. Read
  // from the old cursors, that page would skip a row; read in chain it does not.
  it("neither loses nor repeats a row that moved between pages", async () => {
    const { read } = store(["a", "c", "d", "e", "f"], 2);
    const fresh = await refreshPages<Row>(2, read);
    expect(fresh.items.map((row) => row.id)).toEqual(["a", "c", "d", "e"]);
  });

  it("stops early when the list became shorter than what was loaded", async () => {
    const { read, reads } = store(["a", "b", "c"], 2);
    const fresh = await refreshPages<Row>(5, read);
    expect(fresh.items.map((row) => row.id)).toEqual(["a", "b", "c"]);
    expect(fresh.next_cursor).toBeNull();
    expect(reads.length).toBe(2);
  });

  it("reads at least the first page", async () => {
    const { read, reads } = store(["a"], 2);
    expect((await refreshPages<Row>(0, read)).items).toEqual([{ id: "a" }]);
    expect(reads).toEqual([undefined]);
  });
});
