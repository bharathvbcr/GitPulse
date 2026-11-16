import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import {
  FOLD_MIN_WIDTH,
  contextSlot,
  foldTarget,
  registerContextSlot,
  reportViewBarWidth,
} from "./headerSlots";

// Stand-ins for the view bar's slot node: the module only compares identity.
const node = (name: string) => ({ name }) as unknown as HTMLElement;

const disposers: Array<() => void> = [];
const register = (el: HTMLElement) => {
  const dispose = registerContextSlot(el);
  disposers.push(dispose);
  return dispose;
};

afterEach(() => {
  while (disposers.length) disposers.pop()?.();
  reportViewBarWidth(0);
});

describe("headerSlots", () => {
  it("offers the slot only while the bar reports room for it", () => {
    const a = node("a");
    register(a);
    reportViewBarWidth(FOLD_MIN_WIDTH - 1);
    expect(get(foldTarget)).toBeNull();
    reportViewBarWidth(FOLD_MIN_WIDTH);
    expect(get(foldTarget)).toBe(a);
  });

  it("keeps the second bar's fold when the replaced first bar's disposer runs late", () => {
    // A remount where the new bar's onMount runs before the old one's cleanup:
    // the old disposer must leave the live registration, and its width, alone.
    const first = node("first");
    const second = node("second");
    const disposeFirst = register(first);
    register(second);
    reportViewBarWidth(FOLD_MIN_WIDTH + 200);
    disposeFirst();
    expect(get(contextSlot)).toBe(second);
    expect(get(foldTarget)).toBe(second);
  });

  it("forgets the slot and its width when the live bar goes", () => {
    const a = node("a");
    const dispose = register(a);
    reportViewBarWidth(FOLD_MIN_WIDTH + 200);
    dispose();
    expect(get(contextSlot)).toBeNull();
    expect(get(foldTarget)).toBeNull();
    // A bar registered next starts unfolded until it measures itself.
    const b = node("b");
    register(b);
    expect(get(foldTarget)).toBeNull();
  });
});
