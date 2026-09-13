import { describe, expect, it } from "vitest";
import {
  MAX_QUICK_ADD_LABELS,
  MAX_QUICK_ADD_LENGTH,
  MAX_QUICK_ADD_TITLE,
  matchQuickAddRepository,
  parseQuickAdd,
  parseQuickAddDue,
  quickAddDraft,
} from "./taskQuickAdd";

const repositories = [
  { id: "r0", name: "GitPulse" },
  { id: "r1", name: "Manvi" },
  { id: "r2", name: "Manvi-agent" },
];

// A Wednesday, 10:00 local.
const NOW = new Date(2026, 8, 9, 10, 0, 0, 0).getTime();

const parse = (text: string) => parseQuickAdd(text, { repositories, now: NOW });
const codes = (text: string) => parse(text).warnings.map((warning) => warning.code);

describe("parseQuickAdd", () => {
  it("reads a plain line as nothing but a title", () => {
    const result = parse("Fix the retry loop");
    expect(result).toMatchObject({
      title: "Fix the retry loop",
      description: "",
      priority: null,
      labels: [],
      owner: null,
      kind: null,
      repositoryId: null,
      dueAt: null,
      usable: true,
    });
    expect(result.warnings).toEqual([]);
  });

  it("extracts every marker and leaves the rest as the title", () => {
    const result = parse('Fix retry loop !1 #reliability #"needs design" @ada ~bug ^GitPulse due:2026-09-30');
    expect(result).toMatchObject({
      title: "Fix retry loop",
      priority: 1,
      labels: ["reliability", "needs design"],
      owner: "ada",
      kind: "bug",
      repositoryId: "r0",
    });
    expect(result.dueAt).toBe(Math.floor(new Date(2026, 8, 30, 17, 0, 0, 0).getTime() / 1000));
    expect(result.warnings).toEqual([]);
  });

  it("accepts markers in any position, including before the title", () => {
    expect(parse("!0 @sam Ship the fix").title).toBe("Ship the fix");
    expect(parse("!0 @sam Ship the fix")).toMatchObject({ priority: 0, owner: "sam" });
  });

  it("only treats a marker as a marker at the start of a word", () => {
    const result = parse("Support C# and mail ada@example.com and 3^2 exponents");
    expect(result.title).toBe("Support C# and mail ada@example.com and 3^2 exponents");
    expect(result).toMatchObject({ owner: null, labels: [], repositoryId: null });
  });

  it("types a lone marker through as literal text", () => {
    expect(parse("Rename # to hash").title).toBe("Rename # to hash");
    expect(parse("Is it ! or ?").title).toBe("Is it ! or ?");
  });

  it("lets a backslash escape a leading marker", () => {
    const result = parse("Document \\#hashtags and \\@mentions");
    expect(result.title).toBe("Document #hashtags and @mentions");
    expect(result.labels).toEqual([]);
    expect(result.owner).toBeNull();
  });

  it("splits a description at :: and leaves its markers alone", () => {
    const result = parse("Fix retry loop !1 :: Reproduce with #ci and ping @ada about ^GitPulse");
    expect(result.title).toBe("Fix retry loop");
    expect(result.priority).toBe(1);
    expect(result.description).toBe("Reproduce with #ci and ping @ada about ^GitPulse");
    expect(result.labels).toEqual([]);
    expect(result.owner).toBeNull();
  });

  it("splits a pasted multi-line entry at the first newline", () => {
    const result = parse("Fix retry loop #ci\nSteps:\n1. run it\n2. watch it fail");
    expect(result.title).toBe("Fix retry loop");
    expect(result.labels).toEqual(["ci"]);
    expect(result.description).toBe("Steps:\n1. run it\n2. watch it fail");
  });

  it("prefers whichever of :: and newline comes first", () => {
    expect(parse("A :: one\ntwo").description).toBe("one\ntwo");
    expect(parse("A\none :: two").description).toBe("one :: two");
  });

  it("accepts every priority spelling and rejects the rest loudly", () => {
    for (const [text, value] of [["!0", 0], ["!urgent", 0], ["!p1", 1], ["!high", 1], ["!2", 2], ["!normal", 2], ["!medium", 2], ["!3", 3], ["!low", 3]] as const) {
      expect(parse(`Task ${text}`).priority, text).toBe(value);
    }
    expect(parse("Task !9").priority).toBeNull();
    expect(codes("Task !9")).toContain("bad_priority");
    expect(codes("Task !critical")).toContain("bad_priority");
  });

  it("resolves a repository by exact name or unique prefix, and refuses an ambiguous one", () => {
    expect(parse("A ^gitpulse").repositoryId).toBe("r0");
    expect(parse("A ^Git").repositoryId).toBe("r0");
    // "Manvi" is an exact name even though "Manvi-agent" also starts with it.
    expect(parse("A ^Manvi").repositoryId).toBe("r1");
    expect(parse("A ^Man").repositoryId).toBeNull();
    expect(codes("A ^Man")).toContain("unknown_repository");
  });

  it("keeps an unresolved repository token in the title rather than dropping it", () => {
    const result = parse("Ship ^Unknown today");
    expect(result.title).toBe("Ship ^Unknown today");
    expect(result.repositoryId).toBeNull();
  });

  it("deduplicates labels and caps how many one line may set", () => {
    expect(parse("A #ui #ui #ux").labels).toEqual(["ui", "ux"]);
    const many = `A ${Array.from({ length: MAX_QUICK_ADD_LABELS + 5 }, (_, i) => `#l${i}`).join(" ")}`;
    const result = parse(many);
    expect(result.labels).toHaveLength(MAX_QUICK_ADD_LABELS);
    expect(result.warnings.map((w) => w.code)).toContain("label_cap");
  });

  it("warns instead of guessing when a due token is not a date", () => {
    expect(parse("A due:someday").dueAt).toBeNull();
    expect(codes("A due:someday")).toContain("bad_due");
    // Date.parse would accept this on some engines; the parser must not.
    expect(parse("A due:March").dueAt).toBeNull();
  });

  it("accepts by: as an alias for due:", () => {
    expect(parse("A by:tomorrow").dueAt).toBe(parse("A due:tomorrow").dueAt);
  });

  it("keeps a title-only marker line unusable and says why", () => {
    const result = parse("#ui @ada");
    expect(result.usable).toBe(false);
    expect(result.warnings.map((w) => w.code)).toContain("empty_title");
  });

  it("treats an entirely empty line as neither usable nor a complaint", () => {
    const result = parse("   ");
    expect(result.usable).toBe(false);
    expect(result.warnings).toEqual([]);
  });

  it("produces segments that reconstruct the input exactly", () => {
    for (const line of [
      "Fix retry loop !1 #ci @ada ~bug ^GitPulse due:friday :: notes here",
      "  leading and   inner   spaces  ",
      "A\nmulti\nline",
      "#ui",
      "",
    ]) {
      const result = parseQuickAdd(line, { repositories, now: NOW });
      expect(result.segments.map((segment) => segment.text).join(""), JSON.stringify(line)).toBe(line);
    }
  });

  it("labels each segment with what the parser did with it", () => {
    const kinds = parse("Fix !1 #ci @ada ~bug ^GitPulse due:friday :: notes")
      .segments.filter((segment) => segment.kind !== "text")
      .map((segment) => segment.kind);
    expect(kinds).toEqual(["priority", "label", "owner", "kind", "repository", "due", "separator", "description"]);
  });

  it("strips control characters but keeps tabs, newlines and the segment sum", () => {
    const raw = "Fix\u0007 retry\u0000 loop #ci";
    const result = parseQuickAdd(raw, { repositories, now: NOW });
    expect(result.title).toBe("Fix retry loop");
    expect(result.labels).toEqual(["ci"]);
    // Segments reconstruct the *cleaned* text, which is what the preview draws.
    expect(result.segments.map((s) => s.text).join("")).toBe("Fix retry loop #ci");
    expect(result.segments.some((s) => s.text.includes("\u0007"))).toBe(false);
    expect(parseQuickAdd("A\tb :: c\nd", { now: NOW }).description).toBe("c\nd");
  });
});

describe("parseQuickAddDue", () => {
  const at = (y: number, m: number, d: number, h = 17, min = 0) =>
    Math.floor(new Date(y, m - 1, d, h, min, 0, 0).getTime() / 1000);

  it("reads the relative words against the injected clock", () => {
    expect(parseQuickAddDue("today", NOW)).toBe(at(2026, 9, 9));
    expect(parseQuickAddDue("eod", NOW)).toBe(at(2026, 9, 9));
    expect(parseQuickAddDue("tomorrow", NOW)).toBe(at(2026, 9, 10));
    expect(parseQuickAddDue("tom", NOW)).toBe(at(2026, 9, 10));
    expect(parseQuickAddDue("next week", NOW)).toBe(at(2026, 9, 16));
  });

  it("reads offsets in days, weeks and months", () => {
    expect(parseQuickAddDue("+3d", NOW)).toBe(at(2026, 9, 12));
    expect(parseQuickAddDue("+2w", NOW)).toBe(at(2026, 9, 23));
    expect(parseQuickAddDue("+1m", NOW)).toBe(at(2026, 10, 9));
  });

  it("moves a bare weekday forward, never to today", () => {
    // NOW is a Wednesday.
    expect(parseQuickAddDue("fri", NOW)).toBe(at(2026, 9, 11));
    expect(parseQuickAddDue("friday", NOW)).toBe(at(2026, 9, 11));
    expect(parseQuickAddDue("mon", NOW)).toBe(at(2026, 9, 14));
    expect(parseQuickAddDue("wed", NOW)).toBe(at(2026, 9, 16));
    expect(parseQuickAddDue("next-wed", NOW)).toBe(at(2026, 9, 16));
  });

  it("reads an absolute date, with or without a time", () => {
    expect(parseQuickAddDue("2026-09-30", NOW)).toBe(at(2026, 9, 30));
    expect(parseQuickAddDue("2026-09-30T08:15", NOW)).toBe(at(2026, 9, 30, 8, 15));
  });

  it("rejects impossible and malformed dates instead of rolling them over", () => {
    expect(parseQuickAddDue("2026-02-31", NOW)).toBeNull();
    expect(parseQuickAddDue("2026-13-01", NOW)).toBeNull();
    expect(parseQuickAddDue("2026-09-30T25:00", NOW)).toBeNull();
    expect(parseQuickAddDue("2026-09-30T10:75", NOW)).toBeNull();
    expect(parseQuickAddDue("26-09-30", NOW)).toBeNull();
    expect(parseQuickAddDue("", NOW)).toBeNull();
    expect(parseQuickAddDue("x".repeat(64), NOW)).toBeNull();
    expect(parseQuickAddDue(null, NOW)).toBeNull();
    expect(parseQuickAddDue(42 as unknown as string, NOW)).toBeNull();
  });

  it("refuses an offset that would leave the safe integer range", () => {
    expect(parseQuickAddDue("+9999d", NOW)).toBeGreaterThan(0);
    expect(parseQuickAddDue("+99999d", NOW)).toBeNull();
  });

  it("survives a broken clock without throwing", () => {
    expect(parseQuickAddDue("today", Number.NaN)).toBeGreaterThan(0);
    expect(parseQuickAddDue("today", Number.POSITIVE_INFINITY)).toBeGreaterThan(0);
  });
});

describe("matchQuickAddRepository", () => {
  it("refuses an empty query and an ambiguous prefix", () => {
    expect(matchQuickAddRepository("", repositories)).toBeNull();
    expect(matchQuickAddRepository("   ", repositories)).toBeNull();
    expect(matchQuickAddRepository("Man", repositories)).toBeNull();
  });

  it("refuses to pick between two repositories with the same name", () => {
    const duplicated = [{ id: "a", name: "Same" }, { id: "b", name: "Same" }];
    expect(matchQuickAddRepository("Same", duplicated)).toBeNull();
  });
});

describe("quickAddDraft", () => {
  const defaults = {
    status: "ready" as const,
    kind: "feature",
    repositoryIds: ["r0", "r1"],
    primaryRepositoryId: "r1",
    homeWorkspaceId: "w1",
    position: 42,
  };

  it("returns nothing for a line that is not usable", () => {
    expect(quickAddDraft(parse("   "), defaults)).toBeNull();
    expect(quickAddDraft(parse("#ui"), defaults)).toBeNull();
  });

  it("falls through to the caller's defaults for every unset token", () => {
    expect(quickAddDraft(parse("Ship the fix"), defaults)).toMatchObject({
      title: "Ship the fix",
      status: "ready",
      kind: "feature",
      priority: 2,
      owner: null,
      due_at: null,
      labels: [],
      repository_ids: ["r0", "r1"],
      primary_repository_id: "r1",
      home_workspace_id: "w1",
      position: 42,
    });
  });

  it("makes a named repository primary and keeps the rest linked", () => {
    const draft = quickAddDraft(parse("Ship the fix ^GitPulse"), defaults);
    expect(draft).toMatchObject({ primary_repository_id: "r0", repository_ids: ["r0", "r1"] });
  });

  it("adds a named repository that was not already linked", () => {
    const draft = quickAddDraft(parse("Ship ^Manvi-agent"), { ...defaults, repositoryIds: ["r0"], primaryRepositoryId: "r0" });
    expect(draft).toMatchObject({ primary_repository_id: "r2", repository_ids: ["r2", "r0"] });
  });

  it("refuses to build a draft with no repository at all", () => {
    expect(quickAddDraft(parse("Ship it"), { ...defaults, repositoryIds: [], primaryRepositoryId: "" })).toBeNull();
  });

  it("repairs a primary that is not among the linked repositories", () => {
    const draft = quickAddDraft(parse("Ship it"), { ...defaults, primaryRepositoryId: "gone" });
    expect(draft?.primary_repository_id).toBe("r0");
  });

  it("never starts a task in a locked or partially-enhanced state", () => {
    const draft = quickAddDraft(parse("Ship it !0 @ada"), defaults);
    expect(draft).toMatchObject({ locked_fields: [], acceptance_criteria: [], severity: null });
  });
});

describe("quick add under hostile input", () => {
  it("refuses an oversized line instead of truncating it into a task", () => {
    const result = parseQuickAdd("x".repeat(MAX_QUICK_ADD_LENGTH + 1), { repositories, now: NOW });
    expect(result.usable).toBe(false);
    expect(result.title).toBe("");
    expect(result.warnings.map((w) => w.code)).toEqual(["too_long"]);
  });

  it("caps a very long title without losing the markers before it", () => {
    const result = parse(`!0 ${"word ".repeat(400)}`);
    expect(result.priority).toBe(0);
    expect(result.title.length).toBeLessThanOrEqual(MAX_QUICK_ADD_TITLE);
    expect(result.title.endsWith("…")).toBe(true);
  });

  it("refuses an oversized description with the line, rather than half-saving it", () => {
    // There is no separate description cap. The line cap is the only cap, so
    // a huge paste is refused whole instead of arriving silently truncated.
    const result = parse(`Title :: ${"n".repeat(MAX_QUICK_ADD_LENGTH)}`);
    expect(result.usable).toBe(false);
    expect(result.description).toBe("");
    expect(result.warnings.map((w) => w.code)).toEqual(["too_long"]);
  });

  it("keeps a description that fits under the line cap intact", () => {
    const body = "n".repeat(MAX_QUICK_ADD_LENGTH - 100);
    expect(parse(`Title :: ${body}`).description).toBe(body);
  });

  it("parses a pathological marker-only line in linear time", () => {
    const hostile = `Title ${"#".repeat(1_500)} ${"!".repeat(1_500)} ${'"'.repeat(500)}`;
    expect(hostile.length).toBeLessThanOrEqual(MAX_QUICK_ADD_LENGTH);
    const started = performance.now();
    const result = parseQuickAdd(hostile, { repositories, now: NOW });
    // Catches catastrophic backtracking, which on this input does not finish
    // at all: seconds, not milliseconds, is the separation that matters, and a
    // tighter bound only buys failures on a loaded machine.
    expect(performance.now() - started).toBeLessThan(2_000);
    expect(result.usable).toBe(true);
  });

  it("cannot be made to read a lookup table's inherited keys", () => {
    // `PRIORITY_WORDS["constructor"]` on a plain object literal is a function.
    // It used to land in `priority` and flow straight into a task draft.
    for (const key of ["constructor", "__proto__", "toString", "valueOf", "hasOwnProperty"]) {
      const result = parse(`Ship it !${key}`);
      expect(result.priority, key).toBeNull();
      expect(result.warnings.map((w) => w.code), key).toContain("bad_priority");
      expect(parse(`Ship it due:${key}`).dueAt, key).toBeNull();
    }
    expect(quickAddDraft(parse("Ship it !constructor"), {
      status: "ready", kind: "feature", repositoryIds: ["r0"],
      primaryRepositoryId: "r0", homeWorkspaceId: null, position: 1,
    })?.priority).toBe(2);
  });

  it("parses a long unterminated quote without hanging", () => {
    const hostile = `Title #"${"a b ".repeat(500)}`;
    const started = performance.now();
    const result = parseQuickAdd(hostile, { repositories, now: NOW });
    // Catches catastrophic backtracking, which on this input does not finish
    // at all: seconds, not milliseconds, is the separation that matters, and a
    // tighter bound only buys failures on a loaded machine.
    expect(performance.now() - started).toBeLessThan(2_000);
    expect(result.labels).toHaveLength(1);
  });

  it("stays bounded when every word is a due token", () => {
    const hostile = Array.from({ length: 250 }, (_, i) => `due:2026-09-${String((i % 28) + 1).padStart(2, "0")}`).join(" ");
    const line = `Title ${hostile}`;
    expect(line.length).toBeLessThanOrEqual(MAX_QUICK_ADD_LENGTH);
    const started = performance.now();
    const result = parseQuickAdd(line, { repositories, now: NOW });
    // Catches catastrophic backtracking, which on this input does not finish
    // at all: seconds, not milliseconds, is the separation that matters, and a
    // tighter bound only buys failures on a loaded machine.
    expect(performance.now() - started).toBeLessThan(2_000);
    // Last one wins; no warning, because every token parsed.
    expect(result.dueAt).toBeGreaterThan(0);
    expect(result.warnings).toEqual([]);
  });

  it("never throws, whatever it is handed", () => {
    for (const input of [undefined, null, 0, {}, [], Symbol.iterator, () => {}, Number.NaN]) {
      expect(() => parseQuickAdd(input as unknown as string, { repositories, now: NOW })).not.toThrow();
      expect(parseQuickAdd(input as unknown as string).usable).toBe(false);
    }
  });

  it("survives a catalog of hostile repository names", () => {
    const hostile = Array.from({ length: 5_000 }, (_, i) => ({ id: `r${i}`, name: `${"x".repeat(200)}${i}` }));
    const result = parseQuickAdd("Title ^xxx", { repositories: hostile, now: NOW });
    expect(result.repositoryId).toBeNull();
    expect(result.warnings.map((w) => w.code)).toContain("unknown_repository");
  });
});
