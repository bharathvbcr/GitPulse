import { describe, expect, it } from "vitest";
import { shouldRunStatusPoll } from "./statusPoll";

describe("shouldRunStatusPoll", () => {
  it("runs only when a visible, idle session exists and nothing is in flight", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: false,
        inflight: false,
      })
    ).toBe(true);
  });

  it("skips when the window is hidden", () => {
    expect(
      shouldRunStatusPoll({
        hidden: true,
        hasSession: true,
        isLoading: false,
        inflight: false,
      })
    ).toBe(false);
  });

  it("skips with no open repository", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: false,
        isLoading: false,
        inflight: false,
      })
    ).toBe(false);
  });

  it("never overlaps a hydrate/refresh or the previous poll", () => {
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: true,
        inflight: false,
      })
    ).toBe(false);
    expect(
      shouldRunStatusPoll({
        hidden: false,
        hasSession: true,
        isLoading: false,
        inflight: true,
      })
    ).toBe(false);
  });
});
