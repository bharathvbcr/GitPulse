import { describe, expect, it } from "vitest";
import { ENHANCEMENT_STATE_LABELS } from "./taskEnhance";
import {
  ASSIST_HEADING_DRAFT,
  ASSIST_HEADING_SAVED,
  EDITOR_SECTION_IDS,
  assistAttention,
  assistDisclosure,
  assistFoldStatus,
  editorSections,
  logsDisclosure,
  riseAssistOpen,
  type EditorSectionId,
} from "./taskEditorLayout";

function ids(saved: boolean): EditorSectionId[] {
  return editorSections(saved).map((section) => section.id);
}

describe("editorSections", () => {
  it("gives a draft notes before title, so dictation lands in the fields below", () => {
    expect(ids(false)).toEqual([
      "repositories",
      "assist",
      "title",
      "logs",
      "schedule",
      "owner",
    ]);
    const assist = editorSections(false).find((section) => section.id === "assist");
    const title = editorSections(false).find((section) => section.id === "title");
    expect(assist?.n).toBe(2);
    expect(title?.n).toBe(3);
    expect(assist?.n).toBeLessThan(title?.n ?? 0);
    expect(assist?.heading).toBe(ASSIST_HEADING_DRAFT);
    expect(assist?.heading).not.toBe("Quick add");
  });

  it("gives a saved task the title before the model, and never calls that section Quick add", () => {
    expect(ids(true)).toEqual([
      "repositories",
      "title",
      "assist",
      "logs",
      "schedule",
      "owner",
    ]);
    const sections = editorSections(true);
    const title = sections.find((section) => section.id === "title");
    const assist = sections.find((section) => section.id === "assist");
    expect(title?.n).toBe(2);
    expect(assist?.n).toBe(3);
    expect(title?.n).toBeLessThan(assist?.n ?? 0);
    expect(assist?.heading).toBe(ASSIST_HEADING_SAVED);
    expect(sections.map((section) => section.heading)).not.toContain("Quick add");
  });

  it("numbers every section from 1 with no gaps and no duplicates", () => {
    for (const saved of [false, true]) {
      const sections = editorSections(saved);
      expect(sections.map((section) => section.n)).toEqual([1, 2, 3, 4, 5, 6]);
      expect(new Set(sections.map((section) => section.id)).size).toBe(EDITOR_SECTION_IDS.length);
      expect(sections.map((section) => section.id).sort()).toEqual([...EDITOR_SECTION_IDS].sort());
      for (const section of sections) {
        expect(section.heading.trim().length, section.id).toBeGreaterThan(2);
      }
    }
  });

  it("hands back a fresh array so a caller cannot reorder the source", () => {
    const first = editorSections(true);
    first.reverse();
    first[0]!.heading = "mutated";
    first.pop();
    expect(ids(true)).toEqual(["repositories", "title", "assist", "logs", "schedule", "owner"]);
    expect(editorSections(true)[0]?.heading).toBe("Repositories");
  });

  it("treats only boolean true as saved, so a confused caller still gets the compose order", () => {
    for (const value of [false, 0, 1, "true", "yes", {}, [], null, undefined]) {
      expect(ids(value as never), JSON.stringify(value)).toEqual(ids(false));
    }
    expect(ids(true)).not.toEqual(ids(false));
  });
});

describe("assistAttention", () => {
  it("ignores whitespace-only notes and hostile non-booleans", () => {
    expect(assistAttention({})).toBe(false);
    expect(assistAttention({ notes: "", busy: false, reviewable: false })).toBe(false);
    expect(assistAttention({ notes: " \n\t", busy: 1, reviewable: "yes" })).toBe(false);
    expect(assistAttention({ notes: null, busy: {}, reviewable: [] })).toBe(false);
    expect(assistAttention({ notes: "Keep E42", busy: false, reviewable: false })).toBe(true);
    expect(assistAttention({ notes: "", busy: true, reviewable: false })).toBe(true);
    expect(assistAttention({ notes: "", busy: false, reviewable: true })).toBe(true);
  });

  it("treats failed, interrupted, and uncertain runs as work, and accepted history as not", () => {
    expect(assistAttention({ state: "failed" })).toBe(true);
    expect(assistAttention({ state: "interrupted" })).toBe(true);
    expect(assistAttention({ uncertain: true })).toBe(true);
    expect(assistAttention({ state: "ready" })).toBe(true);
    expect(assistAttention({ state: "running" })).toBe(true);
    expect(assistAttention({ state: "pending" })).toBe(true);
    expect(assistAttention({ state: "cancel_requested" })).toBe(true);
    expect(assistAttention({ state: "accepted" })).toBe(false);
    expect(assistAttention({ state: "undone" })).toBe(false);
    expect(assistAttention({ state: "cancelled" })).toBe(false);
    expect(assistAttention({ state: "dismissed" })).toBe(false);
    expect(assistAttention({ state: "failed", notes: " \n" })).toBe(true);
    expect(assistAttention({ state: "accepted", notes: " \n" })).toBe(false);
    expect(assistAttention({ state: "nope", uncertain: "yes", busy: 1 })).toBe(false);
  });
});

describe("assistFoldStatus", () => {
  it("names remaining work with the same labels the review uses", () => {
    expect(assistFoldStatus({ state: "failed" })).toBe(ENHANCEMENT_STATE_LABELS.failed);
    expect(assistFoldStatus({ state: "interrupted" })).toBe(ENHANCEMENT_STATE_LABELS.interrupted);
    expect(assistFoldStatus({ uncertain: true })).toBe(ENHANCEMENT_STATE_LABELS.interrupted);
    expect(assistFoldStatus({ state: "ready" })).toBe(ENHANCEMENT_STATE_LABELS.ready);
    expect(assistFoldStatus({ state: "running" })).toBe(ENHANCEMENT_STATE_LABELS.running);
    expect(assistFoldStatus({ state: "accepted" })).toBe(ENHANCEMENT_STATE_LABELS.accepted);
    expect(assistFoldStatus({})).toBe("");
    expect(assistFoldStatus({ state: "cancelled" })).toBe("");
    expect(assistFoldStatus({ state: 1, uncertain: "yes" })).toBe("");
  });
});

describe("logsDisclosure", () => {
  it("never folds a draft's paste surface", () => {
    expect(logsDisclosure({ saved: false, hasLogs: false, opened: false })).toEqual({ collapsible: false, open: true });
  });

  it("folds empty logs on a saved task and keeps a dump that already exists", () => {
    expect(logsDisclosure({ saved: true, hasLogs: false, opened: false })).toEqual({ collapsible: true, open: false });
    expect(logsDisclosure({ saved: true, hasLogs: false, opened: true })).toEqual({ collapsible: true, open: true });
    expect(logsDisclosure({ saved: true, hasLogs: true, opened: false })).toEqual({ collapsible: false, open: true });
    expect(logsDisclosure({ saved: "yes", hasLogs: false })).toEqual({ collapsible: false, open: true });
  });
});

describe("assistDisclosure", () => {
  it("never folds a draft, even if opened is false", () => {
    expect(assistDisclosure({ saved: false, opened: false })).toEqual({ collapsible: false, open: true });
    expect(assistDisclosure({ saved: false, opened: true })).toEqual({ collapsible: false, open: true });
    expect(assistDisclosure({})).toEqual({ collapsible: false, open: true });
  });

  it("folds a saved task until something opens it", () => {
    expect(assistDisclosure({ saved: true, opened: false })).toEqual({ collapsible: true, open: false });
    expect(assistDisclosure({ saved: true })).toEqual({ collapsible: true, open: false });
    expect(assistDisclosure({ saved: true, opened: true })).toEqual({ collapsible: true, open: true });
    expect(assistDisclosure({ saved: true, opened: "yes" })).toEqual({ collapsible: true, open: false });
  });
});

describe("riseAssistOpen", () => {
  it("opens on the false→true edge and does not close when attention leaves", () => {
    expect(riseAssistOpen(false, true, false)).toBe(true);
    expect(riseAssistOpen(true, true, false)).toBe(false);
    expect(riseAssistOpen(true, false, true)).toBe(true);
    expect(riseAssistOpen(false, false, false)).toBe(false);
    expect(riseAssistOpen(false, false, true)).toBe(true);
  });

  it("does not treat hostile values as attention arriving", () => {
    expect(riseAssistOpen(false, "yes", false)).toBe(false);
    expect(riseAssistOpen(0, 1, false)).toBe(false);
    expect(riseAssistOpen(false, true, "open")).toBe(true);
  });
});
