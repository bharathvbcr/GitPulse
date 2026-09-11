import { describe, expect, it } from "vitest";
import { applyNotesToDraft, canAskManvi, consumeNotes, sanitizeNotes, titleFromNotes } from "./taskCompose";

const ABBREVIATIONS = ["Dr", "Mr", "Mrs", "Ms", "Prof", "vs", "etc", "e.g", "i.e", "U.S"] as const;

describe("notes extraction adversarial corpus", () => {
  it("never returns a lone list marker or abbreviation as the title", () => {
    for (let n = 1; n <= 200; n++) {
      const notes = `${n}. Keep E${n} across both repository links`;
      const title = titleFromNotes(notes);
      expect(title, notes).not.toBe(`${n}.`);
      expect(title, notes).toContain(`Keep E${n}`);
      expect(applyNotesToDraft({ title: "", description: "" }, notes).title).toBe(title);
    }
    for (const abbr of ABBREVIATIONS) {
      const dotted = /[.]$/.test(abbr) ? abbr : `${abbr}.`;
      const notes = `${dotted} Smith reported E42 on login.`;
      const title = titleFromNotes(notes);
      expect(title, notes).not.toBe(dotted);
      expect(title.toLowerCase(), notes).toContain("smith");
    }
  });

  it("keeps identifiers through consume/apply and still gates oversized UTF-8", () => {
    const samples = [
      "Must keep E42. Do not drop the reproduction steps across both repos.",
      "1. Fix pkg/api/handler.go for #21\nNever invent a passing test.",
      "日本語の再現手順を残す. Must keep E42.",
      "😀 Keep the original \"invalid value\" in both links.",
      "# Restore https://example.com/issue/21 in the description",
      "- [x] Preserve `src/main.go` and E42",
      "A.\nSecond line holds the real work for E42.",
      `${"Keep E42. ".repeat(80)}Do not truncate identifiers.`,
    ];
    for (const notes of samples) {
      const applied = applyNotesToDraft({ title: "", description: "" }, notes);
      expect(applied.extracted).toBe(true);
      expect(applied.title.length).toBeGreaterThan(2);
      expect(applied.title).not.toMatch(/^(?:\d+|[A-Za-z]{1,4}|[A-Za-z](?:\.[A-Za-z])+)\.$/);
      expect(applied.description).toBe(sanitizeNotes(notes));
      const consumed = consumeNotes({ title: "", description: "" }, notes);
      expect(consumed.notes).toBe("");
      expect(consumed.extracted).toBe(true);
      expect(canAskManvi({ title: "", repository_ids: ["r"] }, notes)).toBeNull();
    }
    expect(canAskManvi({ title: "Keep", repository_ids: ["r"] }, "界".repeat(22_000))).toMatch(/64 KB/);
  });

  it("does not invent titles or drop notes under empty, hostile, and duplicate input", () => {
    expect(titleFromNotes("")).toBe("");
    expect(titleFromNotes("\u0000\u0007\n")).toBe("");
    expect(applyNotesToDraft({ title: "Keep E42", description: "Keep E42" }, "Keep E42")).toMatchObject({
      title: "Keep E42",
      description: "Keep E42",
      extracted: true,
    });
    const once = consumeNotes({ title: "", description: "" }, "Preserve repository scope.");
    expect(consumeNotes({ title: once.title, description: once.description }, once.notes).extracted).toBe(false);
  });
});
