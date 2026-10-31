import { describe, expect, it } from "vitest";
import { escapeClearsSearch, isDialogEscape } from "./dialogEscape";

describe("isDialogEscape", () => {
  it("accepts a plain Escape", () => {
    expect(isDialogEscape({ key: "Escape" })).toBe(true);
  });

  it("ignores the Escape that belongs to an IME composition", () => {
    expect(isDialogEscape({ key: "Escape", isComposing: true })).toBe(false);
    expect(isDialogEscape({ key: "Escape", keyCode: 229 })).toBe(false);
  });

  it("ignores every other key", () => {
    expect(isDialogEscape({ key: "Enter" })).toBe(false);
  });
});

describe("escapeClearsSearch", () => {
  it("clears a non-empty search first", () => {
    expect(escapeClearsSearch({ key: "Escape" }, "theme")).toBe(true);
  });

  it("lets Escape on an empty field close the dialog", () => {
    expect(escapeClearsSearch({ key: "Escape" }, "")).toBe(false);
  });

  it("leaves an IME Escape alone even with text in the field", () => {
    expect(escapeClearsSearch({ key: "Escape", isComposing: true }, "theme")).toBe(false);
  });
});
