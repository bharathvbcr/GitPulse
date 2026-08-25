import { describe, expect, it } from "vitest";
import { beginGeneration, createAsyncGuard } from "./guard";

describe("async guards", () => {
  it("cancels in-flight work so a stale completion cannot commit", () => {
    const guard = createAsyncGuard();
    expect(guard.isLive()).toBe(true);
    guard.cancel();
    expect(guard.isLive()).toBe(false);
  });

  it("only the latest generation is current", () => {
    const gen = beginGeneration();
    const first = gen.next();
    const second = gen.next();
    expect(gen.isCurrent(first)).toBe(false);
    expect(gen.isCurrent(second)).toBe(true);
    expect(gen.isCurrent(second + 1)).toBe(false);
  });
});
