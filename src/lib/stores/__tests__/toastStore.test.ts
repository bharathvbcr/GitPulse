import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { get } from "svelte/store";
import { toastStore } from "../toastStore";

describe("toastStore", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    toastStore.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("adds toasts and generates unique ids", () => {
    const id1 = toastStore.info("Hello world");
    const id2 = toastStore.success("Operation completed");

    const toasts = get(toastStore);
    expect(toasts).toHaveLength(2);
    expect(toasts[0].id).toBe(id1);
    expect(toasts[0].message).toBe("Hello world");
    expect(toasts[0].kind).toBe("info");
    expect(toasts[1].id).toBe(id2);
    expect(toasts[1].message).toBe("Operation completed");
    expect(toasts[1].kind).toBe("success");
  });

  it("auto-dismisses toasts after duration", () => {
    toastStore.success("Saved", undefined, 1000);
    expect(get(toastStore)).toHaveLength(1);

    vi.advanceTimersByTime(500);
    expect(get(toastStore)).toHaveLength(1);

    vi.advanceTimersByTime(600);
    expect(get(toastStore)).toHaveLength(0);
  });

  it("supports manual dismissal", () => {
    const id = toastStore.warning("Warning message");
    expect(get(toastStore)).toHaveLength(1);

    toastStore.dismiss(id);
    expect(get(toastStore)).toHaveLength(0);
  });

  it("supports actions attached to toasts", async () => {
    const onClick = vi.fn();
    toastStore.error("Something went wrong", { label: "Retry", onClick });

    const toasts = get(toastStore);
    expect(toasts[0].action?.label).toBe("Retry");
    toasts[0].action?.onClick();
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("caps maximum toasts to 5", () => {
    for (let i = 0; i < 7; i++) {
      toastStore.info(`Message ${i}`, undefined, 0);
    }
    const toasts = get(toastStore);
    expect(toasts).toHaveLength(5);
    expect(toasts[0].message).toBe("Message 2");
    expect(toasts[4].message).toBe("Message 6");
  });
});

describe("an error is not allowed to expire silently", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    toastStore.clear();
  });
  afterEach(() => vi.useRealTimers());

  it("keeps an error up until it is dismissed", () => {
    // The regression: errors expired after 8 s, and `repoStore.error` is
    // routed to a toast and nowhere else — so a failed git operation left no
    // record at all once the toast went.
    toastStore.error("push rejected");
    vi.advanceTimersByTime(60_000);
    expect(get(toastStore)).toHaveLength(1);
  });

  it("still lets a caller ask for a bounded error", () => {
    toastStore.error("transient", undefined, 1_000);
    vi.advanceTimersByTime(1_500);
    expect(get(toastStore)).toHaveLength(0);
  });

  it("gives an action long enough to be reached", () => {
    // "Undo" after a branch delete rode the 4 s info default and could expire
    // while the pointer was still travelling.
    toastStore.action("Deleted branch", "Undo", () => {});
    vi.advanceTimersByTime(5_000);
    expect(get(toastStore)).toHaveLength(1);
  });
});

describe("countdowns pause while the user is on the stack", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    toastStore.clear();
  });
  afterEach(() => vi.useRealTimers());

  it("does not dismiss while paused", () => {
    toastStore.success("saved");
    toastStore.pauseAll();
    vi.advanceTimersByTime(60_000);
    expect(get(toastStore)).toHaveLength(1);
  });

  it("restarts the clock on resume rather than resuming a stale remainder", () => {
    toastStore.success("saved");
    vi.advanceTimersByTime(3_000);
    toastStore.pauseAll();
    toastStore.resumeAll();
    // Someone who moved to the toast is reading it; the countdown starts over.
    vi.advanceTimersByTime(3_000);
    expect(get(toastStore)).toHaveLength(1);
    vi.advanceTimersByTime(1_000);
    expect(get(toastStore)).toHaveLength(0);
  });

  it("leaves a never-expiring toast alone through a pause/resume cycle", () => {
    toastStore.error("push rejected");
    toastStore.pauseAll();
    toastStore.resumeAll();
    vi.advanceTimersByTime(60_000);
    expect(get(toastStore)).toHaveLength(1);
  });

  it("does not resurrect a toast dismissed while paused", () => {
    const id = toastStore.success("saved");
    toastStore.pauseAll();
    toastStore.dismiss(id);
    toastStore.resumeAll();
    vi.advanceTimersByTime(10_000);
    expect(get(toastStore)).toHaveLength(0);
  });
});
