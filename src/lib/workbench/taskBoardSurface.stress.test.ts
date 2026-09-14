/**
 * Adversarial stress for the Tasks board's new decision modules.
 *
 * These four — quick add, the saved view, the context menu and the handoff —
 * share one property that makes them worth attacking together: every one of
 * them turns *reader-controlled text or stored preferences* into something
 * that reaches an IPC payload, a lookup table, or a disabled/enabled control.
 * A parser that yields `Object.prototype.constructor`, a preference file that
 * hides every column, a menu that silently drops the action it is named for,
 * or a gate that opens on a value it never examined are all the same class of
 * defect: trusting a shape instead of checking one.
 *
 * The budgets here are deliberately loose. A tight wall-clock assertion on a
 * machine running concurrent builds measures load, not a regression, and the
 * gap between a linear parse and a catastrophic one is orders of magnitude.
 */
import { describe, expect, it } from "vitest";
import { MAX_QUICK_ADD_LENGTH, parseQuickAdd, parseQuickAddDue, quickAddDraft } from "./taskQuickAdd";
import {
  hiddenColumnReport,
  sanitizeCardFields,
  sanitizeHiddenStatuses,
  toggleHiddenStatus,
  visibleBoardStatuses,
  TASK_CARD_FIELDS,
} from "../ui/taskView";
import { STATUSES, type TaskStatus } from "./vocabulary";
import { ARCHIVE_STATUS, archivable, archiveState, isArchived } from "./taskArchive";
import { taskMenuItems, typeAheadIndex } from "./taskMenu";
import {
  checkoutCandidates,
  handoffGate,
  normalizeCheckout,
  reconcileHandoff,
  sanitizeHandoff,
} from "./taskHandoff";

/** Names that exist on every object and have bitten this codebase before. */
const POLLUTION = ["__proto__", "constructor", "prototype", "toString", "valueOf", "hasOwnProperty", "isPrototypeOf"];

/** Characters that look like nothing and are not nothing. */
/**
 * Characters that look like nothing and are not nothing.
 *
 * Written as escapes, never as the bytes themselves: a raw NUL makes
 * ripgrep treat this whole file as binary and skip it, so the test guarding
 * against control characters would itself become unsearchable.
 */
const INVISIBLE = ["\u202e", "\u200b", "\u00a0", "\u0000", "\u001b", "\u007f"];

const HOSTILE = [
  "", " ", "\t\n", ...INVISIBLE,
  "!".repeat(500), "#".repeat(500), "@".repeat(500), "~".repeat(500), "^".repeat(500),
  "due:".repeat(200), "::".repeat(300), "a".repeat(MAX_QUICK_ADD_LENGTH + 50),
  "!!!! ### @@@ ~~~ ^^^ due:due:due: :::: title",
  "Fix  the retry loop", "Fix the loop \r\n\r\n :: notes",
  ...POLLUTION.flatMap((name) => [`t !${name}`, `t #${name}`, `t @${name}`, `t ~${name}`, `t ^${name}`, `t due:${name}`]),
];

describe("quick add under hostile input", () => {
  it("never yields a non-string, a prototype member, or an unparsed marker", () => {
    const repositories = [{ id: "r1", name: "GitPulse" }, { id: "r2", name: "Manvi" }];
    for (const input of HOSTILE) {
      const parsed = parseQuickAdd(input, { repositories, now: 1_760_000_000_000 });
      expect(typeof parsed.title, JSON.stringify(input)).toBe("string");
      expect(typeof parsed.description, JSON.stringify(input)).toBe("string");
      expect(Array.isArray(parsed.labels), JSON.stringify(input)).toBe(true);
      // A value read out of a plain-object lookup can be a function, and it
      // would travel all the way into an IPC payload before anything noticed.
      for (const value of [parsed.priority, parsed.owner, parsed.kind, parsed.repositoryId, parsed.dueAt]) {
        expect(typeof value, JSON.stringify(input)).not.toBe("function");
        expect(["string", "number", "object"], JSON.stringify(input)).toContain(typeof value);
      }
      for (const label of parsed.labels) {
        expect(typeof label, JSON.stringify(input)).toBe("string");
        expect(label.length, JSON.stringify(input)).toBeGreaterThan(0);
      }
      if (parsed.priority !== null) expect([0, 1, 2, 3], JSON.stringify(input)).toContain(parsed.priority);
      if (parsed.dueAt !== null) expect(Number.isSafeInteger(parsed.dueAt), JSON.stringify(input)).toBe(true);
      // Markers must not survive into the title; that is the whole promise.
      expect(parsed.title.startsWith("!"), JSON.stringify(input)).toBe(false);
      expect(parsed.title.startsWith("#"), JSON.stringify(input)).toBe(false);
    }
  });

  it("produces a draft that is safe to send, or no draft at all", () => {
    const control = /[\u0000-\u001f\u007f]/;
    for (const input of HOSTILE) {
      const parsed = parseQuickAdd(input, { repositories: [{ id: "r1", name: "GitPulse" }] });
      const draft = quickAddDraft(parsed, {
        status: "inbox",
        kind: "feature",
        repositoryIds: ["r1"],
        primaryRepositoryId: "r1",
        homeWorkspaceId: null,
        position: 1,
      });
      if (!draft) continue;
      expect(draft.title.trim().length, JSON.stringify(input)).toBeGreaterThan(0);
      expect(draft.title.length, JSON.stringify(input)).toBeLessThanOrEqual(300);
      expect(draft.repository_ids, JSON.stringify(input)).toContain(draft.primary_repository_id);
      expect(new Set(draft.labels).size, JSON.stringify(input)).toBe(draft.labels.length);
      expect([0, 1, 2, 3], JSON.stringify(input)).toContain(draft.priority);
      // Anything the store would refuse must be refused here instead.
      expect(control.test(draft.title), JSON.stringify(input)).toBe(false);
      for (const label of draft.labels) expect(control.test(label), label).toBe(false);
      if (draft.owner !== null) expect(control.test(draft.owner), draft.owner).toBe(false);
    }
  });

  it("parses a pathological line in linear time", () => {
    // Alternation-free, fixed-width patterns only; this is the assertion that
    // would fail if someone reached for a convenient nested quantifier.
    const nasty = `${"!high ".repeat(300)}${"#label ".repeat(300)}${"due:friday ".repeat(60)}title`;
    const started = performance.now();
    for (let i = 0; i < 40; i++) parseQuickAdd(nasty.slice(0, MAX_QUICK_ADD_LENGTH), { repositories: [] });
    expect(performance.now() - started).toBeLessThan(3_000);
  });

  it("refuses every due value it cannot place on the calendar", () => {
    const now = Date.UTC(2026, 8, 12, 10, 0, 0);
    for (const value of ["", " ", "never", "friday13", "2026-13-45", "+0d", "+9999999d", "-3d", "due:", ...INVISIBLE, ...POLLUTION]) {
      const parsed = parseQuickAddDue(value, now);
      expect(parsed === null || Number.isSafeInteger(parsed), JSON.stringify(value)).toBe(true);
      if (parsed !== null) expect(parsed, JSON.stringify(value)).toBeGreaterThan(0);
    }
  });
});

describe("saved board preferences under a corrupted store", () => {
  const GARBAGE = [null, undefined, 0, "", "inbox", {}, NaN, [], [null], [{}], ["nonsense"], [...POLLUTION], Object.create(null)];

  it("never hides every column, whatever was stored", () => {
    for (const value of [...GARBAGE, [...STATUSES], [...STATUSES, ...STATUSES]]) {
      const hidden = sanitizeHiddenStatuses(value);
      expect(Array.isArray(hidden), JSON.stringify(value)).toBe(true);
      expect(hidden.length, JSON.stringify(value)).toBeLessThan(STATUSES.length);
      for (const status of hidden) expect(STATUSES, JSON.stringify(value)).toContain(status);
      // And the board drawn from it always has something on it.
      const counts = Object.fromEntries(STATUSES.map((status) => [status, 0])) as Record<TaskStatus, number>;
      expect(visibleBoardStatuses(hidden, counts, false).length, JSON.stringify(value)).toBeGreaterThan(0);
    }
  });

  it("cannot be walked into an empty board one toggle at a time", () => {
    let hidden: TaskStatus[] = [];
    for (let pass = 0; pass < 4; pass++) {
      for (const status of STATUSES) hidden = toggleHiddenStatus(hidden, status);
    }
    expect(hidden.length).toBeLessThan(STATUSES.length);
  });

  it("keeps card fields to the known set and an explicit empty choice", () => {
    for (const value of GARBAGE) {
      const fields = sanitizeCardFields(value);
      expect(Array.isArray(fields), JSON.stringify(value)).toBe(true);
      for (const field of fields) expect(TASK_CARD_FIELDS, JSON.stringify(value)).toContain(field);
      expect(new Set(fields).size, JSON.stringify(value)).toBe(fields.length);
    }
    // An empty array is a real choice ("no chips"), not drift to repair.
    expect(sanitizeCardFields([])).toEqual([]);
    expect(sanitizeCardFields(["nonsense"])).toEqual([...TASK_CARD_FIELDS]);
  });

  it("never reports hidden work it cannot count", () => {
    const counts = Object.fromEntries(STATUSES.map((status, index) => [status, index * 7])) as Record<TaskStatus, number>;
    for (const value of GARBAGE) {
      const report = hiddenColumnReport(sanitizeHiddenStatuses(value), counts);
      if (!report) continue;
      expect(Number.isSafeInteger(report.tasks)).toBe(true);
      expect(report.tasks).toBeGreaterThanOrEqual(0);
      expect(report.summary.trim().length).toBeGreaterThan(0);
      expect(report.statuses.length).toBeGreaterThan(0);
      // The count must equal what it claims to be summarizing.
      expect(report.tasks).toBe(report.statuses.reduce((total, status) => total + counts[status], 0));
    }
  });
});

describe("the context menu under hostile selections", () => {
  const card = (over: Record<string, unknown> = {}) => ({
    id: "c1", revision: 1, title: "Card", status: "ready" as TaskStatus, priority: 2,
    kind: "bug", owner: null, due_at: null, labels: [] as string[],
    repository_ids: ["r"], primary_repository_id: "r", ...over,
  }) as never;

  it("always offers a usable row, and never a checked-and-enabled current value", () => {
    const selections = [
      [card()],
      [card(), card({ id: "c2", status: "done", priority: 0 })],
      [card({ owner: " " }), card({ owner: "x".repeat(400) })],
      [card({ labels: POLLUTION }), card({ labels: ["ci"] })],
      Array.from({ length: 60 }, (_, i) => card({ id: `c${i}`, status: STATUSES[i % STATUSES.length] })),
    ];
    for (const cards of selections) {
      for (const busy of [false, true]) {
        const items = taskMenuItems({
          cards,
          column: null,
          busy,
          vocabulary: { owners: POLLUTION, labels: POLLUTION },
          canHandoff: true,
        });
        expect(items.length).toBeGreaterThan(0);
        for (const item of items) {
          expect(typeof item.label).toBe("string");
          expect(item.label.trim().length).toBeGreaterThan(0);
          // A row showing the value a task already has must not also invite
          // setting it again: that write would spend a revision to change
          // nothing. Label rows are exempt because their action toggles —
          // pressing a checked label removes it, which is not a no-op.
          if (item.checked === true && item.action.kind !== "label") {
            expect(item.disabled, item.label).toBe(true);
          }
        }
        // Type-ahead must land on a row that exists, or report none.
        for (const query of ["", "z", " ", "move", "m".repeat(50), ...INVISIBLE, ...POLLUTION]) {
          const index = typeAheadIndex(items, query, 0);
          expect(index === -1 || (index >= 0 && index < items.length), JSON.stringify(query)).toBe(true);
        }
      }
    }
  });

  it("offers no agent handoff when the board says it cannot hand off", () => {
    const items = taskMenuItems({
      cards: [card()],
      column: null,
      busy: false,
      vocabulary: { owners: [], labels: [] },
      canHandoff: false,
    });
    expect(items.some((item) => item.label.includes("Send to agent"))).toBe(false);
  });
});

describe("the handoff gate under hostile settings", () => {
  it("restores something launchable from any stored value, and never bypass", () => {
    const stored = [
      null, undefined, 0, "", [], {},
      { provider: "claude", kind: "managed", permission: "bypass" },
      { provider: "nonsense", kind: "nonsense", permission: "nonsense" },
      Object.fromEntries(POLLUTION.map((name) => [name, name])),
      { provider: { toString: () => "codex" }, kind: ["managed"], permission: 7 },
    ];
    for (const value of stored) {
      const settings = sanitizeHandoff(value);
      expect(["codex", "claude"], JSON.stringify(value)).toContain(settings.provider);
      expect(["external_terminal", "managed"], JSON.stringify(value)).toContain(settings.kind);
      expect(settings.permission, JSON.stringify(value)).not.toBe("bypass");
      // Reconciling is idempotent, so a round trip cannot drift.
      expect(reconcileHandoff(settings)).toEqual(settings);
      expect(sanitizeHandoff(settings)).toEqual(settings);
      // And the pair is always one the launch button could actually run.
      if (settings.kind === "managed") expect(settings.provider).toBe("codex");
    }
  });

  it("refuses a checkout it cannot vouch for, and says why exactly once", () => {
    const settings = sanitizeHandoff(null);
    for (const checkout of ["", " ", "\n", "/", "x".repeat(5_000), ...INVISIBLE, ...POLLUTION]) {
      if (normalizeCheckout(checkout)) continue;
      const gate = handoffGate({ checkout, settings, acknowledgedBypass: false, dirty: false, busy: false });
      expect(gate.ok, JSON.stringify(checkout)).toBe(false);
      expect(gate.reason.trim().length, JSON.stringify(checkout)).toBeGreaterThan(0);
    }
    // One reason at a time, in the order the reader should act on them.
    expect(handoffGate({ checkout: "", settings, acknowledgedBypass: false, dirty: true, busy: true }).reason)
      .toBe("A launch is already in progress.");
    expect(handoffGate({ checkout: "", settings, acknowledgedBypass: false, dirty: true, busy: false }).reason)
      .toContain("Save your task edits");
  });

  it("never offers the same checkout twice, however the fixture spells it", () => {
    const repositories = [{ id: "r", identity_key: "local:/work/GitPulse/.git" }];
    const tabs = [
      { path: "/work/GitPulse", label: "GitPulse" },
      { path: "/work/GitPulse/", label: "trailing slash" },
      { path: "/WORK/gitpulse", label: "different case" },
      { path: "/work//GitPulse", label: "double separator" },
      { path: " ", label: "unusable" },
      { path: "", label: "empty" },
    ];
    for (const caseInsensitive of [true, false]) {
      const candidates = checkoutCandidates("r", repositories, tabs, { caseInsensitive });
      const keys = candidates.map((candidate) => (caseInsensitive ? candidate.path.toLowerCase() : candidate.path));
      expect(new Set(keys).size, String(caseInsensitive)).toBe(keys.length);
      for (const candidate of candidates) {
        expect(normalizeCheckout(candidate.path), candidate.path).toBeTruthy();
        expect(candidate.label.trim().length).toBeGreaterThan(0);
      }
    }
    expect(checkoutCandidates("missing", repositories, tabs, { caseInsensitive: true })).toEqual([]);
  });
});

/**
 * The archive seam under selections nobody would build on purpose.
 *
 * `archiveState` decides whether a control is offered and `archivable` decides
 * what a batch writes. They answer the same question from opposite ends, and a
 * disagreement between them is a click that either does nothing or writes more
 * than the reader asked for. The status field they read is a wire value: it
 * arrives from the store as JSON, and a card whose status this build does not
 * have must not silently count as archived.
 */
describe("the archive seam under hostile selections", () => {
  const task = (status: unknown, id = "t") => ({ id, status }) as { id: string; status: TaskStatus };
  const NOT_A_STATUS: unknown[] = [
    undefined, null, "", " done ", "DONE", "Done", 0, 1, true, {}, [], "archived", "completed",
    ...POLLUTION, ...INVISIBLE.map((mark) => `done${mark}`),
  ];

  it("treats exactly one spelling as archived, whatever the wire sent", () => {
    for (const value of NOT_A_STATUS) {
      expect(isArchived(task(value)), JSON.stringify(value)).toBe(false);
      expect(archiveState([task(value)]), JSON.stringify(value)).toBe("none");
      expect(archivable([task(value)]).length, JSON.stringify(value)).toBe(1);
    }
    expect(isArchived(task(ARCHIVE_STATUS))).toBe(true);
  });

  it("never lets the two readings disagree about whether there is work to do", () => {
    const pool: TaskStatus[] = [...STATUSES, ...(NOT_A_STATUS as TaskStatus[])];
    // Deterministic pseudo-random selections: a fixed seed keeps a failure
    // reproducible, which a Math.random sweep would not.
    let seed = 0x2f6e2b1;
    const next = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    for (let trial = 0; trial < 2000; trial += 1) {
      const size = Math.floor(next() * 6);
      const cards = Array.from({ length: size }, (_, i) => task(pool[Math.floor(next() * pool.length)], `t${i}`));
      const state = archiveState(cards);
      const wanted = archivable(cards);
      const nothingToDo = cards.length === 0 || state === "all";
      expect(wanted.length === 0, JSON.stringify(cards)).toBe(nothingToDo);
      // What survives the filter is exactly what is not archived, in order,
      // and never anything the caller did not hand in.
      expect(wanted).toEqual(cards.filter((card) => !isArchived(card)));
      expect(wanted.length <= cards.length).toBe(true);
      if (state === "some") expect(wanted.length).toBeGreaterThan(0);
      if (state === "none") expect(wanted.length).toBe(cards.length);
    }
  });

  it("stays linear on a selection far past anything the board can build", () => {
    // MAX_TASK_SELECTION is 100. This is three orders of magnitude past it,
    // so a quadratic scan would be visible even on a loaded machine.
    const huge = Array.from({ length: 100_000 }, (_, i) =>
      task(STATUSES[i % STATUSES.length], `t${i}`));
    // Counted, not divided: 100_000 does not divide evenly by six statuses,
    // and an expectation that rounds is an expectation that stops asserting.
    const archived = huge.filter((entry) => entry.status === ARCHIVE_STATUS).length;
    expect(archived).toBeGreaterThan(0);
    const started = performance.now();
    expect(archiveState(huge)).toBe("some");
    expect(archivable(huge).length).toBe(huge.length - archived);
    expect(performance.now() - started).toBeLessThan(2000);
  });

  it("offers the menu's Archive row exactly when the batch would write something", () => {
    const menuCard = (status: TaskStatus, id: string) => ({
      id, revision: 1, title: "Card", status, priority: 2, kind: "bug",
      owner: null, due_at: null, labels: [] as string[],
      repository_ids: ["r"], primary_repository_id: "r",
    }) as never;
    const selections: TaskStatus[][] = [
      ["ready"],
      [ARCHIVE_STATUS],
      ["ready", ARCHIVE_STATUS],
      [ARCHIVE_STATUS, ARCHIVE_STATUS],
      [...STATUSES],
      Array.from({ length: 120 }, () => ARCHIVE_STATUS),
      Array.from({ length: 120 }, (_, i) => (i === 119 ? "ready" : ARCHIVE_STATUS)) as TaskStatus[],
    ];
    for (const statuses of selections) {
      const cards = statuses.map((status, i) => menuCard(status, `c${i}`));
      const row = taskMenuItems({ cards, column: null, busy: false }).find((item) => item.id === "archive");
      expect(row, JSON.stringify(statuses)).toBeDefined();
      // The row and the write agree: enabled exactly when something changes.
      const writes = archivable(statuses.map((status, i) => ({ id: `c${i}`, status }))).length;
      expect(row?.disabled, JSON.stringify(statuses)).toBe(writes === 0);
      expect(row?.hint?.trim().length).toBeGreaterThan(0);
    }
    // Busy closes it regardless of what the write would have done.
    const cards = [menuCard("ready", "c0")];
    expect(taskMenuItems({ cards, column: null, busy: true }).find((item) => item.id === "archive")?.disabled).toBe(true);
  });
});
