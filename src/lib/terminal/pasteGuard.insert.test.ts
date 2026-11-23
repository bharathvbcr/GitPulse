import { describe, expect, it } from "vitest";
import { PASTE_END, planInsert } from "./pasteGuard";

describe("planInsert: text GitPulse inserts into a session", () => {
  it("types a single line as before, and pastes it only into a bracketed shell", () => {
    expect(planInsert("@a.ts#L1 ", { bracketed: false, shell: false })).toEqual({ kind: "input", text: "@a.ts#L1 " });
    expect(planInsert("@a.ts#L1 ", { bracketed: true, shell: false })).toEqual({ kind: "input", text: "@a.ts#L1 " });
    expect(planInsert("@a.ts#L1 ", { bracketed: true, shell: true })).toEqual({ kind: "paste", text: "@a.ts#L1 " });
  });

  it("pastes a block of lines as one bracketed paste into an agent or a shell", () => {
    expect(planInsert("a\n```\nb\n```", { bracketed: true, shell: false }).kind).toBe("paste");
    expect(planInsert("a\nb", { bracketed: true, shell: true }).kind).toBe("paste");
  });

  it("refuses a block of lines for a session that is not in bracketed-paste mode, with the reason", () => {
    const plan = planInsert("a\nb", { bracketed: false, shell: true });
    expect(plan.kind).toBe("refuse");
    if (plan.kind === "refuse") expect(plan.reason).toContain("line by line");
  });

  it("removes a paste terminator hidden in the text so the bracket cannot end early", () => {
    const plan = planInsert(`a${PASTE_END}rm -rf ~\nb`, { bracketed: true, shell: true });
    expect(plan).toEqual({ kind: "paste", text: "arm -rf ~\nb" });
  });

  it("refuses a block that carries a control character", () => {
    expect(planInsert(`a${String.fromCharCode(3)}\nb`, { bracketed: true, shell: true }).kind).toBe("refuse");
  });
});
