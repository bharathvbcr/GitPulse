import { describe, expect, it } from "vitest";
import { activeSelection, clearActiveSelection, setActiveSelection, type ActiveSelection } from "./activeSelection";

const selection = (path: string): ActiveSelection => ({
  path, startLine: 1, endLine: 2, sendReference() {}, sendContent() {}, addToTray() {},
});

describe("the active file selection", () => {
  it("is what the last viewer to select registered, until that viewer clears it", () => {
    const a = {};
    expect(activeSelection()).toBeNull();
    setActiveSelection(a, selection("a.ts"));
    expect(activeSelection()?.path).toBe("a.ts");
    clearActiveSelection(a);
    expect(activeSelection()).toBeNull();
  });

  it("is not removed by a viewer that no longer owns it", () => {
    const a = {};
    const b = {};
    setActiveSelection(a, selection("a.ts"));
    setActiveSelection(b, selection("b.ts"));
    clearActiveSelection(a);
    expect(activeSelection()?.path).toBe("b.ts");
    clearActiveSelection(b);
    expect(activeSelection()).toBeNull();
  });
});
