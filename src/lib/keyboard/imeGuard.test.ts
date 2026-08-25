import { describe, expect, it } from "vitest";
import { isImeComposition } from "./imeGuard";

describe("isImeComposition", () => {
  it("flags IME composition keystrokes so they never run commands", () => {
    expect(isImeComposition({ isComposing: true, keyCode: 13 })).toBe(true);
    expect(isImeComposition({ isComposing: false, keyCode: 229 })).toBe(true);
    expect(isImeComposition({ keyCode: 229 })).toBe(true);
  });

  it("passes plain keystrokes through", () => {
    expect(isImeComposition({ isComposing: false, keyCode: 13 })).toBe(false);
    expect(isImeComposition({ keyCode: 40 })).toBe(false);
    expect(isImeComposition({})).toBe(false);
  });
});
