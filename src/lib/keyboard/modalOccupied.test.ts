import { describe, expect, it } from "vitest";
import { isModalOccupied } from "./modalOccupied";

const doc = (open: boolean) => {
  const seen: string[] = [];
  return {
    seen,
    querySelector: (selector: string) => {
      seen.push(selector);
      return open ? ({} as Element) : null;
    },
  };
};

describe("isModalOccupied", () => {
  it("is false when no aria-modal dialog is on screen", () => {
    expect(isModalOccupied(doc(false))).toBe(false);
  });

  it("is true when one is", () => {
    expect(isModalOccupied(doc(true))).toBe(true);
  });

  it("asks for any aria-modal surface by default", () => {
    const fake = doc(false);
    isModalOccupied(fake);
    expect(fake.seen).toEqual(['[aria-modal="true"]']);
  });

  it("excludes the named surface, matching the command palette's own check", () => {
    const fake = doc(false);
    isModalOccupied(fake, "data-command-palette");
    expect(fake.seen).toEqual(['[aria-modal="true"]:not([data-command-palette])']);
  });
});
