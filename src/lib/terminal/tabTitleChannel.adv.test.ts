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

  it("a cut that lands inside a pair after hidden characters leaves no lone surrogate", () => {
    // 127 hidden units fill the pre-bound up to the emoji's high surrogate.
    const title = cleanTabName(`${"​".repeat(127)}\u{1F600}`, 64);
    expect(title).toBe("");
  });

  it("a megabyte title stays bounded and a title of hidden characters is empty", () => {
    expect(Array.from(cleanTabName("x".repeat(1_000_000), 256)).length).toBe(256);
    expect(cleanTabName("​‮\u{E0041}", 256)).toBe("");
  });
});
