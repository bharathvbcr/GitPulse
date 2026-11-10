import { describe, expect, it } from "vitest";
import { isSentenceCase, toSentenceCase } from "./sentenceCase";

/**
 * Several protected spans in one label: the spans are found phrase-first and
 * chord-second, so the rewrite only reads correctly if they are put back in
 * reading order (and an overlap keeps the earlier, longer one). A single span
 * never exercises that ordering.
 */
describe("toSentenceCase with several protected spans", () => {
  it("keeps two shortcut chords verbatim and lowercases the words between them", () => {
    expect(toSentenceCase("Press Cmd+K Or Ctrl+P To Search")).toBe("Press Cmd+K or Ctrl+P to search");
  });

  it("orders a phrase found first and a chord found later by position in the label", () => {
    // Phrases are collected before chords, so the chord at the start of the
    // label is discovered after the proper-noun phrase that ends it.
    expect(toSentenceCase("Cmd+K Opens Command Palette", ["Command Palette"])).toBe("Cmd+K opens Command Palette");
  });

  it("is idempotent across several spans", () => {
    const once = toSentenceCase("Use Cmd+K Or Ctrl+P In GitHub Panel", ["GitHub Panel"]);
    expect(toSentenceCase(once, ["GitHub Panel"])).toBe(once);
    expect(isSentenceCase(once, ["GitHub Panel"])).toBe(true);
  });
});
