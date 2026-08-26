import { describe, expect, it } from "vitest";
import { isKeyboardFocus } from "./focusVisibility";

function fakeElement(matches: (selector: string) => boolean): Element {
  return { matches } as unknown as Element;
}

describe("isKeyboardFocus", () => {
  it("reflects :focus-visible when the engine supports it", () => {
    const queried: string[] = [];
    const yes = fakeElement((s) => {
      queried.push(s);
      return true;
    });
    expect(isKeyboardFocus(yes)).toBe(true);
    expect(queried).toEqual([":focus-visible"]);
    expect(isKeyboardFocus(fakeElement(() => false))).toBe(false);
  });

  it("fails open when the selector is unsupported", () => {
    // A keyboard user must never lose the accessible card because the
    // environment cannot answer the modality question.
    const throwing = fakeElement(() => {
      throw new SyntaxError("unknown pseudo-class");
    });
    expect(isKeyboardFocus(throwing)).toBe(true);
  });
});
