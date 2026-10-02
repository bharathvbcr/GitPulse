import { describe, expect, it } from "vitest";
import { isEditingElsewhere, isTerminalKeyTarget, shellOwnsKey } from "./terminalFocus";

/** Just enough of an Element: `closest` answers for the selectors given. */
function element(matches: string[]): { closest(selector: string): object | null } {
  return { closest: (selector: string) => (matches.includes(selector) ? {} : null) };
}

const inTerminal = element(["[data-terminal-session] .xterm"]);
const elsewhere = element([]);

describe("keys typed into a terminal", () => {
  it("recognises a terminal's input and nothing else", () => {
    expect(isTerminalKeyTarget(inTerminal)).toBe(true);
    expect(isTerminalKeyTarget(elsewhere)).toBe(false);
    for (const odd of [null, undefined, 42, "x", {}, { closest: "not a function" }]) {
      expect(isTerminalKeyTarget(odd)).toBe(false);
    }
  });

  it("gives Ctrl+K and every other plain Control chord to the shell", () => {
    expect(shellOwnsKey({ target: inTerminal, ctrlKey: true, metaKey: false })).toBe(true);
    expect(shellOwnsKey({ target: inTerminal, ctrlKey: true, metaKey: false, altKey: true })).toBe(true);
  });

  it("leaves Command chords, unmodified keys, and keys outside a terminal to the app", () => {
    expect(shellOwnsKey({ target: inTerminal, ctrlKey: false, metaKey: true })).toBe(false);
    expect(shellOwnsKey({ target: inTerminal, ctrlKey: true, metaKey: true })).toBe(false);
    expect(shellOwnsKey({ target: inTerminal, ctrlKey: false, metaKey: false })).toBe(false);
    expect(shellOwnsKey({ target: elsewhere, ctrlKey: true, metaKey: false })).toBe(false);
    expect(shellOwnsKey({ ctrlKey: true, metaKey: false })).toBe(false);
  });
});

describe("a session that starts while the user types elsewhere", () => {
  const host = { contains: (node: unknown) => node === inside };
  const inside = { tagName: "TEXTAREA" };
  it("does not take focus from a field outside the terminal", () => {
    expect(isEditingElsewhere({ tagName: "INPUT" }, host)).toBe(true);
    expect(isEditingElsewhere({ tagName: "select" }, host)).toBe(true);
    expect(isEditingElsewhere({ tagName: "DIV", isContentEditable: true }, host)).toBe(true);
  });
  it("takes focus from the page, a button, or its own input", () => {
    expect(isEditingElsewhere(inside, host)).toBe(false);
    expect(isEditingElsewhere({ tagName: "BODY" }, host)).toBe(false);
    expect(isEditingElsewhere({ tagName: "BUTTON" }, host)).toBe(false);
    expect(isEditingElsewhere(null, host)).toBe(false);
    expect(isEditingElsewhere({ tagName: "INPUT" }, null)).toBe(true);
  });
});
