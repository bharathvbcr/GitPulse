import { describe, expect, it } from "vitest";
import {
  ARCHIVE_RULE,
  ARCHIVE_STATUS,
  RESTORE_STATUSES,
  archivable,
  archiveAction,
  archiveState,
  archiveSummary,
  boardPresence,
  isArchived,
  offersArchive,
  restoreAction,
} from "./taskArchive";
import { STATUSES, STATUS_LABELS, type TaskStatus } from "./vocabulary";

const at = (status: TaskStatus, id: string = status) => ({ id, status });

describe("what the archive holds", () => {
  it("treats exactly one status as completed", () => {
    expect(ARCHIVE_STATUS).toBe("done");
    expect(STATUSES).toContain(ARCHIVE_STATUS);
  });

  // Derived, not listed: a status added upstream becomes a restore target
  // without a second edit, and the archived status can never restore to
  // itself. A hand-written list is what lets those two drift apart.
  it("offers every other status as a restore target, in board order", () => {
    expect(RESTORE_STATUSES).toEqual(STATUSES.filter((status) => status !== ARCHIVE_STATUS));
    expect(RESTORE_STATUSES).not.toContain(ARCHIVE_STATUS);
    expect(RESTORE_STATUSES.length).toBe(STATUSES.length - 1);
  });
});

describe("restoring", () => {
  it("is the same status update the board's Move menu builds", () => {
    for (const status of RESTORE_STATUSES) {
      expect(restoreAction(status)).toEqual({ kind: "update", changes: { status } });
    }
  });

  it("refuses to restore a completed task to completed", () => {
    expect(() => restoreAction(ARCHIVE_STATUS)).toThrow(/Cannot restore/);
  });

  it("refuses a status this build does not have, without naming it undefined", () => {
    const bogus = "shipped" as TaskStatus;
    expect(() => restoreAction(bogus)).toThrow("Cannot restore a completed task to that status.");
  });
});

describe("the summary line", () => {
  // The whole reason this function exists: a loaded page and a server total
  // are reported together whenever they differ, so 30 rows on screen can
  // never read as the complete archive.
  it("carries both numbers while the archive is only partly loaded", () => {
    expect(archiveSummary(30, 412)).toEqual({ text: "Showing 30 of 412 completed tasks.", partial: true, pending: false });
    expect(archiveSummary(1, 2)).toEqual({ text: "Showing 1 of 2 completed tasks.", partial: true, pending: false });
  });

  it("drops the second number only when everything is on screen", () => {
    expect(archiveSummary(412, 412)).toEqual({ text: "412 completed tasks.", partial: false, pending: false });
    expect(archiveSummary(1, 1)).toEqual({ text: "1 completed task.", partial: false, pending: false });
  });

  it("says nothing is here only for an actually empty scope", () => {
    expect(archiveSummary(0, 0)).toEqual({ text: "No completed tasks in this scope.", partial: false, pending: false });
    expect(archiveSummary(0, 7)).toEqual({ text: "Showing 0 of 7 completed tasks.", partial: true, pending: false });
  });

  // The defect this separation was added for: the dock defers its query while
  // its window is in the background, and "0 rows loaded" rendered as "No
  // completed tasks in this scope" underneath a header badge reading 34. A
  // query that never ran must not answer like one that ran and found nothing.
  it("keeps an unread archive distinct from an empty one", () => {
    expect(archiveSummary(null, 0)).toEqual({ text: "Completed tasks have not loaded yet.", partial: false, pending: true });
    expect(archiveSummary(null, 34)).toEqual({ text: "Completed tasks have not loaded yet.", partial: false, pending: true });
    expect(archiveSummary(0, 0).pending).toBe(false);
    expect(archiveSummary(null, 34).text).not.toBe(archiveSummary(0, 0).text);
  });

  // A total that arrives smaller than the rows already loaded is a stale or
  // corrupt count. Believing it would print "Showing 30 of 4", which reads as
  // a bug in the reader's eyes rather than in the count, so the larger of the
  // two wins and the line stays truthful about what is on screen.
  it("never reports fewer tasks in total than it is showing", () => {
    expect(archiveSummary(30, 4)).toEqual({ text: "30 completed tasks.", partial: false, pending: false });
    expect(archiveSummary(30, -1)).toEqual({ text: "30 completed tasks.", partial: false, pending: false });
  });

  it("tolerates fractional and negative counts from a hostile payload", () => {
    expect(archiveSummary(2.7, 9.9)).toEqual({ text: "Showing 2 of 9 completed tasks.", partial: true, pending: false });
    expect(archiveSummary(-5, 0)).toEqual({ text: "No completed tasks in this scope.", partial: false, pending: false });
  });
});

describe("what the dock says about the board", () => {
  it("admits that completed tasks are still on the board, and offers the hide", () => {
    const presence = boardPresence([]);
    expect(presence.onBoard).toBe(true);
    expect(presence.sentence).toContain("Done column on the board");
    expect(presence.actionLabel).toBe("Hide Done on the board");
  });

  it("switches to the other direction once Done is hidden", () => {
    const presence = boardPresence(["done"]);
    expect(presence.onBoard).toBe(false);
    expect(presence.actionLabel).toBe("Show Done on the board");
  });

  it("ignores other hidden columns, which the board's own note already covers", () => {
    expect(boardPresence(["inbox", "review"]).onBoard).toBe(true);
    expect(boardPresence(["review", "done"]).onBoard).toBe(false);
  });

  // The board's note names every hidden column that holds work; only the
  // completed one has a second home, so only it earns the extra action.
  it("offers the archive from the board's note only for the completed column", () => {
    expect(offersArchive(["done"])).toBe(true);
    expect(offersArchive(["review", "done"])).toBe(true);
    expect(offersArchive(["review"])).toBe(false);
    expect(offersArchive([])).toBe(false);
  });

  // Both sentences are fixed chrome above the one scrolling list, and the
  // hidden-direction one had no assertion on it at all — it could have said
  // anything. They are pinned short, and named for the column rather than
  // for a spelling of it.
  it("keeps both sentences to one short line that names the column", () => {
    for (const hidden of [[], [ARCHIVE_STATUS]] as const) {
      const { sentence } = boardPresence(hidden);
      expect(sentence.length, `too long to sit beside the rule: ${sentence}`).toBeLessThanOrEqual(48);
      expect(sentence).toContain(STATUS_LABELS[ARCHIVE_STATUS]);
      expect(sentence.endsWith(".")).toBe(true);
    }
    expect(boardPresence([ARCHIVE_STATUS]).sentence).toContain("hidden");
    expect(boardPresence([]).sentence).not.toContain("hidden");
  });
});

describe("the rule the dock teaches", () => {
  // The sentence used to live only in the empty state — the one moment a
  // reader has no archived work to wonder about. Everyone with completed
  // tasks saw a panel called Archive, a Restore button, and nothing that
  // said how a task gets in. It is now header chrome, so it is asserted to
  // actually name the mechanism rather than to merely exist.
  it("names the column a task reaches, without spelling it a second time", () => {
    expect(ARCHIVE_RULE).toContain(STATUS_LABELS[ARCHIVE_STATUS]);
    expect(ARCHIVE_RULE).toMatch(/^A task is archived when it reaches .+\.$/);
    // Built from the label, so renaming the status upstream renames this.
    expect(ARCHIVE_RULE).toBe(`A task is archived when it reaches ${STATUS_LABELS[ARCHIVE_STATUS]}.`);
  });
});

describe("archiving", () => {
  it("is the same status update restoring is, in the other direction", () => {
    expect(archiveAction()).toEqual({ kind: "update", changes: { status: ARCHIVE_STATUS } });
    // The pair is symmetric: whatever archiving writes, no restore target
    // may write, or a restore would be a no-op dressed up as a move.
    for (const status of RESTORE_STATUSES) {
      expect(restoreAction(status)).not.toEqual(archiveAction());
    }
  });

  it("reads archived off the one field that decides it", () => {
    expect(isArchived(at(ARCHIVE_STATUS))).toBe(true);
    for (const status of RESTORE_STATUSES) expect(isArchived(at(status))).toBe(false);
  });

  it("separates none, some and all so a mixed selection is still offered", () => {
    expect(archiveState([])).toBe("none");
    expect(archiveState([at("ready"), at("inbox")])).toBe("none");
    expect(archiveState([at("ready"), at(ARCHIVE_STATUS)])).toBe("some");
    expect(archiveState([at(ARCHIVE_STATUS, "a"), at(ARCHIVE_STATUS, "b")])).toBe("all");
    // Every status the board has, one at a time: exactly one of them is the
    // archive, so exactly one single-card selection reads "all".
    const all = STATUSES.filter((status) => archiveState([at(status)]) === "all");
    expect(all).toEqual([ARCHIVE_STATUS]);
  });

  // The defect this function was added for: archiving a mixed selection
  // re-wrote the already-archived cards, spending a revision each to store
  // the status they already had and handing the batch more writes to fail on.
  it("writes only the tasks archiving would change, in the order selected", () => {
    const selection = [at("ready", "a"), at(ARCHIVE_STATUS, "b"), at("review", "c")];
    expect(archivable(selection).map((task) => task.id)).toEqual(["a", "c"]);
    expect(archivable([at(ARCHIVE_STATUS, "b")])).toEqual([]);
    expect(archivable([])).toEqual([]);
    // Never a superset, and never the caller's own array — including on the
    // path where nothing is filtered out, which is the one an "avoid the
    // copy when there is nothing to drop" shortcut would take. A caller that
    // sorted the result would otherwise reorder its own selection.
    const untouched = [at("ready", "a"), at("review", "c")];
    for (const input of [selection, untouched, []]) {
      expect(archivable(input)).not.toBe(input);
      expect(archivable(input).every((task) => input.includes(task))).toBe(true);
    }
    expect(archivable(untouched)).toEqual(untouched);
  });

  // `archiveState` and `archivable` are two readings of the same question and
  // are used together — the menu disables on one, the board filters on the
  // other. They may not disagree about whether there is anything to do.
  it("agrees with archiveState about whether there is anything to write", () => {
    const cases: (readonly { id: string; status: TaskStatus }[])[] = [
      [],
      [at("ready", "a")],
      [at(ARCHIVE_STATUS, "a")],
      [at("ready", "a"), at(ARCHIVE_STATUS, "b")],
      STATUSES.map((status) => at(status)),
    ];
    for (const selection of cases) {
      const empty = archivable(selection).length === 0;
      const nothingToDo = selection.length === 0 || archiveState(selection) === "all";
      expect(empty, `disagreed on ${JSON.stringify(selection)}`).toBe(nothingToDo);
    }
  });
});
