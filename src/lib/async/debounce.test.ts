import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { debounce } from "./debounce";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("debounce", () => {
  it("fires once on the trailing edge with the latest args", () => {
    const fn = vi.fn();
    const d = debounce(fn, 120);
    d("a");
    d("b");
    d("c");
    vi.advanceTimersByTime(119);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith("c");
  });

  it("restarts the quiet window on every call", () => {
    const fn = vi.fn();
    const d = debounce(fn, 100);
    d("x");
    vi.advanceTimersByTime(60);
    d("y");
    vi.advanceTimersByTime(60);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(40);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith("y");
  });

  it("cancel drops the pending call", () => {
    const fn = vi.fn();
    const d = debounce(fn, 50);
    d("x");
    d.cancel();
    vi.advanceTimersByTime(500);
    expect(fn).not.toHaveBeenCalled();
  });

  it("cancel is a no-op with nothing pending and does not block later calls", () => {
    const fn = vi.fn();
    const d = debounce(fn, 50);
    d.cancel();
    d("after-cancel");
    vi.advanceTimersByTime(50);
    expect(fn).toHaveBeenCalledTimes(1);
    expect(fn).toHaveBeenCalledWith("after-cancel");
  });
});
