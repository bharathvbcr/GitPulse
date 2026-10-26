import { describe, expect, it } from "vitest";
import { cleanTabName } from "./tabs";
import { clean } from "./sessionActivity";

// A tab title is set by the program (OSC 0/2) and shown to the reader, so it is
// a channel for the same hidden characters the notification path drops.
// `cleanTabName` routes through `clean()`, the one owner of what is hidden.
describe("channel bypass: tab titles are cleaned by the one owner", () => {
  // A bidi override, a zero-width space, a private-use character, bidi
  // isolates, and a tag character.
  const spoof = ["evil‮txt.exe", "a​b", "​x", "a⁦b⁩c", "\u{E0041}tag"];
  for (const text of spoof) {
    it(`cleanTabName drops hidden characters from ${JSON.stringify(text)}`, () => {
      expect(cleanTabName(text, 64)).toBe(clean(text, 64) ?? "");
    });
  }

  it("the code-unit pre-bound never leaves a lone surrogate", () => {
    const title = cleanTabName(`a${"\u{1F600}".repeat(200)}`, 64);
    expect(title).toMatch(/^a(?:\u{1F600})*$/u);
    expect(title.isWellFormed()).toBe(true);
    expect(Array.from(title).length).toBeLessThanOrEqual(64);
  });

  it("hidden characters ahead of an emoji neither hide it nor split it", () => {
    // Hidden characters are dropped from inside the raw bound, so the emoji
    // survives whole however many of them come first.
    const title = cleanTabName(`${"​".repeat(127)}\u{1F600}`, 64);
    expect(title).toBe("\u{1F600}");
  });

  it("a pair the raw bound cuts in two is dropped whole, not left as a lone surrogate", () => {
    const title = cleanTabName(`${"x".repeat(65_535)}\u{1F600}`, 100_000);
    expect(title.isWellFormed()).toBe(true);
    expect(title).toBe("x".repeat(65_535));
  });

  // ADVERSARIAL (L1): `clean()` keeps a lone surrogate that is not the trailing
  // one the pre-bound drops. A program can emit one through OSC 0/2 (xterm hands
  // the renderer a JS string); it renders as U+FFFD and `JSON.stringify` escapes
  // it as `\ud800`, which serde_json refuses, so a title that is later sent to
  // the backend would fail to deserialise.
  it("a lone surrogate in the middle of a title is not kept", () => {
    for (const title of ["a\ud800b", "a\udc00b", "\udc00x", "ab\ud800\ud800cd"]) {
      expect(cleanTabName(title, 64).isWellFormed(), JSON.stringify(title)).toBe(true);
    }
  });

  // ADVERSARIAL (L1): the code-unit pre-bound runs before the hidden characters
  // are dropped, so enough of them ahead of the title push the visible text out
  // of the window and the tab falls back to the launcher label.
  it("hidden characters ahead of the title do not push it out of the bound", () => {
    expect(cleanTabName(`${"\u200b".repeat(200)}visible`, 64)).toBe("visible");
    expect(cleanTabName(`${" ".repeat(600)}visible`, 256)).toBe("visible");
  });

  it("a megabyte title stays bounded and a title of hidden characters is empty", () => {
    expect(Array.from(cleanTabName("x".repeat(1_000_000), 256)).length).toBe(256);
    expect(cleanTabName("​‮\u{E0041}", 256)).toBe("");
  });
});
