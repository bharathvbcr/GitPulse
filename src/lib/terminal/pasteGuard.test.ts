import { describe, expect, it } from "vitest";
import { PASTE_END, PASTE_START, planPaste, stripPasteMarkers } from "./pasteGuard";

describe("paste markers inside the clipboard", () => {
  it("removes an embedded terminator so the bracket cannot be closed early", () => {
    const attack = `echo safe${PASTE_END}rm -rf ~\n`;
    const plan = planPaste(attack, true);
    expect(plan.text).toBe("echo saferm -rf ~\n");
    expect(plan.text).not.toContain(PASTE_END);
    expect(plan.question?.message).toContain("ends a paste early");
  });

  it("removes a marker the removal itself would have joined together", () => {
    const nested = `a\x1b[20${PASTE_START}0~b\x1b[20${PASTE_END}1~c`;
    const stripped = stripPasteMarkers(nested);
    expect(stripped).not.toContain(PASTE_START);
    expect(stripped).not.toContain(PASTE_END);
    // Bounded on adversarial nesting.
    let deep = "x";
    for (let i = 0; i < 200; i++) deep = `\x1b[20${deep.includes(PASTE_START) ? PASTE_END : PASTE_START}1~${deep}`;
    expect(stripPasteMarkers(deep)).not.toMatch(/\x1b\[20[01]~/);
  });
});

describe("pastes that run before they are read", () => {
  it("asks before several lines reach a shell without bracketed paste", () => {
    expect(planPaste("make\nmake install\n", false).question?.title).toBe("Paste 2 lines?");
    expect(planPaste("a\r\nb\rc", false).question?.title).toBe("Paste 3 lines?");
  });

  it("does not ask for one command, with or without its newline, or for a bracketed paste", () => {
    for (const text of ["", "ls -la", "ls -la\n", "ls -la\r\n"]) expect(planPaste(text, false).question).toBeNull();
    expect(planPaste("make\nmake install\n", true).question).toBeNull();
    expect(planPaste("make\nmake install\n", true).text).toBe("make\nmake install\n");
  });

  it("asks about control characters even when bracketed, and never alters them", () => {
    const plan = planPaste("vim\x1b:q!\r\x03", true);
    expect(plan.question?.message).toContain("2 control characters");
    expect(plan.text).toBe("vim\x1b:q!\r\x03");
    expect(planPaste("\x7f", true).question?.message).toContain("1 control character,");
  });

  it("treats tabs and Unicode as ordinary text", () => {
    expect(planPaste("a\tb 世界🚀é", false).question).toBeNull();
    const big = "世界🚀é".repeat(25000);
    expect(planPaste(big, false)).toEqual({ text: big, question: null });
  });
});
