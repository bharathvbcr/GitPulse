import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createSuggestWatch, suggestSignature } from "./suggestFreshness";
import type { Page, TaskCard, TaskStatus } from "./client";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

const card = (id: string, status: TaskStatus, priority: number, extra: Partial<TaskCard> = {}) => ({ id, status, priority, title: id, position: 1, ...extra }) as TaskCard;
const columns = (...cards: TaskCard[]) => ({ inbox: { items: cards } as unknown as Page<TaskCard> });

describe("suggestSignature", () => {
  it("changes with a task's priority, status or presence and ignores order, titles and positions", () => {
    const base = suggestSignature(columns(card("a", "inbox", 2), card("b", "ready", 1)));
    expect(suggestSignature(columns(card("b", "ready", 1, { title: "renamed", position: 9 }), card("a", "inbox", 2)))).toBe(base);
    expect(suggestSignature(columns(card("a", "inbox", 0), card("b", "ready", 1)))).not.toBe(base);
    expect(suggestSignature(columns(card("a", "ready", 2), card("b", "ready", 1)))).not.toBe(base);
    expect(suggestSignature(columns(card("a", "inbox", 2)))).not.toBe(base);
  });
});

describe("createSuggestWatch", () => {
  it("treats the first board as a baseline and asks nothing", () => {
    const ask = vi.fn();
    const watch = createSuggestWatch(ask, 300);
    watch.observe(null, "x");
    watch.observe("board-1", "a");
    watch.observe("board-1", "a");
    vi.advanceTimersByTime(1000);
    expect(ask).not.toHaveBeenCalled();
  });
  it("asks once, after the quiet window, for a burst of changes", () => {
    const ask = vi.fn();
    const watch = createSuggestWatch(ask, 300);
    watch.observe("k", "a");
    for (const sig of ["b", "c", "d"]) { watch.observe("k", sig); vi.advanceTimersByTime(100); }
    expect(ask).not.toHaveBeenCalled();
    vi.advanceTimersByTime(300);
    expect(ask).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(5000);
    expect(ask).toHaveBeenCalledTimes(1);
  });
  it("does not ask for an unchanged signature, and a change back still counts", () => {
    const ask = vi.fn();
    const watch = createSuggestWatch(ask, 100);
    watch.observe("k", "a");
    watch.observe("k", "a");
    vi.advanceTimersByTime(500);
    expect(ask).not.toHaveBeenCalled();
    watch.observe("k", "b");
    vi.advanceTimersByTime(100);
    watch.observe("k", "a");
    vi.advanceTimersByTime(100);
    expect(ask).toHaveBeenCalledTimes(2);
  });
  it("drops a pending re-ask when another board loads, so a scope change is not asked twice", () => {
    const ask = vi.fn();
    const watch = createSuggestWatch(ask, 300);
    watch.observe("k1", "a");
    watch.observe("k1", "b");
    watch.observe("k2", "z");
    vi.advanceTimersByTime(1000);
    expect(ask).not.toHaveBeenCalled();
  });
  it("reset cancels a pending ask and re-baselines", () => {
    const ask = vi.fn();
    const watch = createSuggestWatch(ask, 100);
    watch.observe("k", "a");
    watch.observe("k", "b");
    watch.reset();
    vi.advanceTimersByTime(500);
    expect(ask).not.toHaveBeenCalled();
    watch.observe("k", "c");
    watch.observe("k", "d");
    vi.advanceTimersByTime(100);
    expect(ask).toHaveBeenCalledTimes(1);
  });
});
