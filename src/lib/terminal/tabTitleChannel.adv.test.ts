import { describe, expect, it } from "vitest";
import { cleanTabName } from "./tabs";
import { clean } from "./sessionActivity";

// A tab title is set by the program (OSC 0/2) and shown to the reader, so it is
// a channel for the same hidden characters the notification path drops.
describe("channel bypass: tab titles are cleaned by something other than the one owner", () => {
  const spoof = ["evil‮txt.exe", "a​b", "x", "a⁦b⁩c", "\u{E0041}tag"];
  for (const text of spoof) {
    it(`cleanTabName drops hidden characters from ${JSON.stringify(text)}`, () => {
      expect(cleanTabName(text, 64)).toBe(clean(text, 64) ?? "");
    });
  }
});
