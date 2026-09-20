import { describe, expect, it } from "vitest";
import {
  ASSIST_HEADING_DRAFT,
  ASSIST_HEADING_SAVED,
  EDITOR_SECTION_IDS,
  assistAttention,
  assistDisclosure,
  editorSections,
  riseAssistOpen,
} from "./taskEditorLayout";

const POLLUTION = ["__proto__", "constructor", "prototype", "toString", "valueOf", "hasOwnProperty"];
const INVISIBLE = ["\u202e", "\u200b", "\u00a0", "\u0000", "\u001b", "\u007f"];
const HOSTILE_SAVED: unknown[] = [
  false, true, 0, 1, -1, 2, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY,
  "", "true", "false", "yes", "saved", null, undefined,
  {}, [], Object.create(null), () => true, new Boolean(true),
  ...POLLUTION,
];

function assertValidSections(saved: unknown): void {
  const sections = editorSections(saved);
  const isSaved = saved === true;
  expect(sections).toHaveLength(EDITOR_SECTION_IDS.length);
  expect(sections.map((section) => section.n)).toEqual([1, 2, 3, 4, 5, 6]);
  expect(new Set(sections.map((section) => section.id)).size).toBe(EDITOR_SECTION_IDS.length);
  const ids = sections.map((section) => section.id);
  expect(ids).toEqual(isSaved
    ? ["repositories", "title", "assist", "logs", "schedule", "owner"]
    : ["repositories", "assist", "title", "logs", "schedule", "owner"]);
  const title = sections.find((section) => section.id === "title")!;
  const assist = sections.find((section) => section.id === "assist")!;
  if (isSaved) {
    expect(title.n).toBeLessThan(assist.n);
    expect(assist.heading).toBe(ASSIST_HEADING_SAVED);
    expect(assist.n).toBe(3);
  } else {
    expect(assist.n).toBeLessThan(title.n);
    expect(assist.heading).toBe(ASSIST_HEADING_DRAFT);
    expect(assist.n).toBe(2);
  }
  expect(sections.some((section) => /quick add/i.test(section.heading))).toBe(false);
  for (const section of sections) {
    expect(Number.isSafeInteger(section.n)).toBe(true);
    expect(section.heading).toBe(section.heading.trim());
    expect(section.heading.length).toBeGreaterThan(2);
    expect(section.heading.includes("\0")).toBe(false);
  }
}

describe("task editor layout under hostile input", () => {
  it("never emits a Quick add heading, a gap in numbering, or assist-before-title on a saved sheet", () => {
    for (const saved of HOSTILE_SAVED) assertValidSections(saved);
  });

  it("survives 200 in-place mutations of a previous result", () => {
    for (let n = 1; n <= 200; n++) {
      const previous = editorSections(n % 2 === 0);
      previous.reverse();
      previous[0]!.heading = `mutated-${n}`;
      previous[0]!.n = 99;
      (previous[0] as { id: string }).id = POLLUTION[n % POLLUTION.length]!;
      previous.push({ id: "assist", n: 7, heading: "Quick add" });
      assertValidSections(true);
      assertValidSections(false);
    }
  });

  it("treats only real notes / busy / reviewable as attention across the boolean lattice", () => {
    const notesPool: unknown[] = ["", " ", "\n", "\t\n", ...INVISIBLE, "Keep E42", "  Keep E42  ", null, undefined, 0, 1, true, { text: "x" }, ...POLLUTION];
    const flagPool: unknown[] = [false, true, 0, 1, "true", "false", null, undefined, {}, [], ...POLLUTION];
    let sawTrue = 0;
    let sawFalse = 0;
    for (const notes of notesPool) {
      for (const busy of flagPool) {
        for (const reviewable of flagPool) {
          const result = assistAttention({ notes, busy, reviewable });
          expect(typeof result).toBe("boolean");
          const expectTrue = (typeof notes === "string" && notes.trim().length > 0) || busy === true || reviewable === true;
          expect(result, JSON.stringify({ notes, busy, reviewable })).toBe(expectTrue);
          if (result) sawTrue += 1;
          else sawFalse += 1;
        }
      }
    }
    expect(sawTrue).toBeGreaterThan(0);
    expect(sawFalse).toBeGreaterThan(0);
  });

  it("disclosure never folds a draft and never opens a saved task on a truthy non-boolean", () => {
    for (const saved of HOSTILE_SAVED) {
      for (const opened of HOSTILE_SAVED) {
        const result = assistDisclosure({ saved, opened });
        expect(result.collapsible).toBe(saved === true);
        expect(result.open).toBe(saved !== true || opened === true);
        expect(Object.keys(result).sort()).toEqual(["collapsible", "open"]);
      }
    }
  });

  it("riseAssistOpen only latches the false→true edge, including a 3-bit walk", () => {
    const bits = [false, true];
    for (const prev of bits) {
      for (const now of bits) {
        for (const opened of bits) {
          const result = riseAssistOpen(prev, now, opened);
          expect(result).toBe((now && !prev) || opened);
        }
      }
    }
    for (const prev of HOSTILE_SAVED) {
      for (const now of HOSTILE_SAVED) {
        for (const opened of HOSTILE_SAVED) {
          const result = riseAssistOpen(prev, now, opened);
          expect(typeof result).toBe("boolean");
          expect(result).toBe((now === true && prev !== true) || opened === true);
        }
      }
    }
  });

  it("a saved idle sheet stays collapsed through 1_000 open/close cycles, then a new suggestion opens it once", () => {
    let opened = false;
    let prev = false;
    for (let n = 0; n < 1_000; n++) {
      opened = riseAssistOpen(prev, false, opened);
      prev = false;
      expect(assistDisclosure({ saved: true, opened }).open).toBe(false);
    }
    opened = riseAssistOpen(prev, true, opened);
    prev = true;
    expect(opened).toBe(true);
    expect(assistDisclosure({ saved: true, opened }).open).toBe(true);
    opened = false;
    opened = riseAssistOpen(prev, true, opened);
    expect(opened).toBe(false);
    opened = riseAssistOpen(true, false, true);
    expect(opened).toBe(true);
  });
});

describe("section id table", () => {
  it("is a closed vocabulary the sheet can switch on without a default branch inventing a seventh section", () => {
    const seen = new Set<string>(EDITOR_SECTION_IDS);
    expect(seen.size).toBe(6);
    expect(seen.has("quick-add")).toBe(false);
    expect(seen.has("quickadd")).toBe(false);
    for (const id of EDITOR_SECTION_IDS) {
      expect(id).toMatch(/^[a-z]+$/);
    }
  });
});
