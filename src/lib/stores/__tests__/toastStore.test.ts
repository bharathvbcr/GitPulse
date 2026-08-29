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
