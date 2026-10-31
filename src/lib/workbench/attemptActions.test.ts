import { describe, expect, it } from "vitest";
import { canAct, slotHeldNote } from "./attemptActions";

describe("canAct", () => {
  it("allows everything when nothing is running", () => {
    expect(canAct("a", {}, null, false)).toBe(true);
    expect(canAct("a", {}, null, true)).toBe(true);
  });

  it("locks every button on the card whose action is in flight, and only that card", () => {
    const inflight = { a: "Merging" };
    expect(canAct("a", inflight, "a", false)).toBe(false);
    expect(canAct("a", inflight, "a", true)).toBe(false);
    expect(canAct("b", inflight, "a", false)).toBe(true);
  });

  it("holds mutating actions on other cards behind the slot, never stopping or opening ones", () => {
    const inflight = { a: "Merging" };
    expect(canAct("b", inflight, "a", true)).toBe(false);
    // Stop agent, Stop managed run, Cancel preparation, Show terminal, View output.
    expect(canAct("b", inflight, "a", false)).toBe(true);
  });

  it("a non-mutating action in flight takes no slot, so other cards may still mutate", () => {
    expect(canAct("b", { a: "Stopping" }, null, true)).toBe(true);
  });
});

describe("slotHeldNote", () => {
  it("names what the holder is doing, on other cards only", () => {
    expect(slotHeldNote("b", { a: "Merging" }, "a")).toBe("Waiting: another attempt is merging.");
    expect(slotHeldNote("a", { a: "Merging" }, "a")).toBeNull();
    expect(slotHeldNote("b", {}, null)).toBeNull();
  });

  it("still says something when the holder's verb is unknown", () => {
    expect(slotHeldNote("b", {}, "a")).toBe("Waiting: another attempt's action is running.");
  });
});
