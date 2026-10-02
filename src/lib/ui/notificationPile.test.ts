import { describe, expect, it } from "vitest";
import { buriedUnread, focusStayedInside, notificationPile, pileToggleLabel } from "./notificationPile";

describe("notification pile", () => {
  it("draws nothing for an empty or unusable count", () => {
    for (const count of [0, -1, -0.5, Number.NaN, Number.POSITIVE_INFINITY, Number.NEGATIVE_INFINITY]) {
      expect(notificationPile(count, false)).toEqual({ shown: 0, peeks: 0 });
      expect(notificationPile(count, true)).toEqual({ shown: 0, peeks: 0 });
    }
  });

  it("keeps a single notice as one card", () => {
    expect(notificationPile(1, false)).toEqual({ shown: 1, peeks: 0 });
    expect(notificationPile(1.9, true)).toEqual({ shown: 1, peeks: 0 });
  });

  it("stacks the rest behind the newest card", () => {
    expect(notificationPile(2, false)).toEqual({ shown: 1, peeks: 1 });
    expect(notificationPile(3, false)).toEqual({ shown: 1, peeks: 2 });
    expect(notificationPile(30.8, false)).toEqual({ shown: 1, peeks: 2 });
  });

  it("expands to every loaded card and drops the lips", () => {
    expect(notificationPile(2, true)).toEqual({ shown: 2, peeks: 0 });
    expect(notificationPile(30, true)).toEqual({ shown: 30, peeks: 0 });
  });

  it("counts unread notices behind the front card only", () => {
    const readAt = [null, 10, null, null] as const;
    expect(buriedUnread(readAt, false)).toBe(2);
    expect(buriedUnread(readAt, true)).toBe(0);
    expect(buriedUnread([null], false)).toBe(0);
    expect(buriedUnread([], false)).toBe(0);
  });

  it("names the loaded stack and stays quiet when there is nothing to stack", () => {
    expect(pileToggleLabel(0, false, 0)).toBeNull();
    expect(pileToggleLabel(1, false, 0)).toBeNull();
    expect(pileToggleLabel(1, true, 4)).toBeNull();
    expect(pileToggleLabel(Number.NaN, false, 1)).toBeNull();
    expect(pileToggleLabel(2, false, 0)).toBe("2 notifications");
    expect(pileToggleLabel(5, false, 3)).toBe("5 notifications, 3 unread");
    expect(pileToggleLabel(5, false, Number.NaN)).toBe("5 notifications");
    expect(pileToggleLabel(5, true, 3)).toBe("Show stack");
  });

  it("keeps the pile engaged when focus stays inside it", () => {
    const child = { id: "dismiss" };
    const outside = { id: "elsewhere" };
    const root = { contains: (target: unknown) => target === child || target === root };
    expect(focusStayedInside(root, child)).toBe(true);
    expect(focusStayedInside(root, root)).toBe(true);
    expect(focusStayedInside(root, outside)).toBe(false);
    expect(focusStayedInside(root, null)).toBe(false);
    expect(focusStayedInside(undefined, child)).toBe(false);
  });
});
