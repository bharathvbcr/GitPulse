import { describe, expect, it, vi } from "vitest";
import {
  COMMIT_PANE_BOUNDS,
  clampPaneHeight,
  fitPaneHeight,
  nudgedPaneHeight,
  startPaneDrag,
  type PaneBounds,
} from "./paneHeight";

const BOUNDS: PaneBounds = { min: 100, max: 500, fallback: 200 };

describe("clampPaneHeight", () => {
  it("keeps an in-range height and rounds to whole pixels", () => {
    expect(clampPaneHeight(300, BOUNDS)).toBe(300);
    expect(clampPaneHeight(300.4, BOUNDS)).toBe(300);
    expect(clampPaneHeight(300.6, BOUNDS)).toBe(301);
  });

  it("holds the height inside its bounds", () => {
    expect(clampPaneHeight(10, BOUNDS)).toBe(100);
    expect(clampPaneHeight(9_999, BOUNDS)).toBe(500);
  });

  it("fails closed to the fallback on a non-finite height", () => {
    expect(clampPaneHeight(Number.NaN, BOUNDS)).toBe(200);
    expect(clampPaneHeight(Number.POSITIVE_INFINITY, BOUNDS)).toBe(200);
    expect(clampPaneHeight(Number.NEGATIVE_INFINITY, BOUNDS)).toBe(200);
  });

  it("the commit pane keeps today's 256px as its default, inside its range", () => {
    expect(COMMIT_PANE_BOUNDS.fallback).toBe(256);
    expect(clampPaneHeight(256, COMMIT_PANE_BOUNDS)).toBe(256);
    expect(clampPaneHeight(0, COMMIT_PANE_BOUNDS)).toBe(COMMIT_PANE_BOUNDS.min);
  });
});

describe("fitPaneHeight", () => {
  it("honours the request when the container has room", () => {
    expect(fitPaneHeight(300, 900, 160, BOUNDS)).toBe(300);
  });

  it("leaves the view above at least its minimum", () => {
    expect(fitPaneHeight(500, 400, 160, BOUNDS)).toBe(240);
  });

  it("returns the minimum when the container cannot hold both", () => {
    expect(fitPaneHeight(400, 200, 160, BOUNDS)).toBe(100);
  });

  it("treats an unmeasured container as unknown, not as no room", () => {
    expect(fitPaneHeight(300, 0, 160, BOUNDS)).toBe(300);
    expect(fitPaneHeight(300, Number.NaN, 160, BOUNDS)).toBe(300);
    expect(fitPaneHeight(9_999, 0, 160, BOUNDS)).toBe(500);
  });
});

describe("nudgedPaneHeight", () => {
  it("grows on ArrowUp, shrinks on ArrowDown, and ignores other keys", () => {
    expect(nudgedPaneHeight("ArrowUp", 256, 24)).toBe(280);
    expect(nudgedPaneHeight("ArrowDown", 256, 24)).toBe(232);
    expect(nudgedPaneHeight("Enter", 256, 24)).toBeNull();
  });
});

describe("startPaneDrag", () => {
  function handle() {
    const target = new EventTarget();
    const setPointerCapture = vi.fn();
    const releasePointerCapture = vi.fn();
    return Object.assign(target, { setPointerCapture, releasePointerCapture });
  }
  function pointerDown(target: EventTarget, init: { button?: number; clientY?: number } = {}) {
    const preventDefault = vi.fn();
    const event = { button: 0, clientY: 500, pointerId: 7, currentTarget: target, preventDefault, ...init };
    return { event: event as unknown as PointerEvent, preventDefault };
  }
  function fire(target: EventTarget, type: string, clientY = 0) {
    target.dispatchEvent(Object.assign(new Event(type), { clientY }));
  }

  it("ignores a non-primary button without capturing or preventing anything", () => {
    const target = handle();
    const { event, preventDefault } = pointerDown(target, { button: 2 });
    const handlers = { onHeight: vi.fn(), onEnd: vi.fn() };
    expect(startPaneDrag(event, 300, BOUNDS, handlers)).toBe(false);
    expect(preventDefault).not.toHaveBeenCalled();
    expect(target.setPointerCapture).not.toHaveBeenCalled();
    fire(target, "pointermove", 100);
    expect(handlers.onHeight).not.toHaveBeenCalled();
  });

  it("captures the pointer and reports clamped heights, up growing the pane", () => {
    const target = handle();
    const { event, preventDefault } = pointerDown(target, { clientY: 500 });
    const handlers = { onHeight: vi.fn(), onEnd: vi.fn() };
    expect(startPaneDrag(event, 300, BOUNDS, handlers)).toBe(true);
    expect(preventDefault).toHaveBeenCalledOnce();
    expect(target.setPointerCapture).toHaveBeenCalledWith(7);

    fire(target, "pointermove", 450);
    expect(handlers.onHeight).toHaveBeenLastCalledWith(350);
    fire(target, "pointermove", 600);
    expect(handlers.onHeight).toHaveBeenLastCalledWith(200);
    fire(target, "pointermove", 0);
    expect(handlers.onHeight).toHaveBeenLastCalledWith(500);
    fire(target, "pointermove", 5_000);
    expect(handlers.onHeight).toHaveBeenLastCalledWith(100);
    expect(handlers.onEnd).not.toHaveBeenCalled();
  });

  it.each(["pointerup", "pointercancel"])("ends once on %s, releases capture and stops listening", (type) => {
    const target = handle();
    const { event } = pointerDown(target);
    const handlers = { onHeight: vi.fn(), onEnd: vi.fn() };
    startPaneDrag(event, 300, BOUNDS, handlers);
    fire(target, type);
    expect(handlers.onEnd).toHaveBeenCalledOnce();
    expect(target.releasePointerCapture).toHaveBeenCalledWith(7);
    handlers.onHeight.mockClear();
    fire(target, "pointermove", 0);
    fire(target, "pointerup");
    fire(target, "pointercancel");
    expect(handlers.onHeight).not.toHaveBeenCalled();
    expect(handlers.onEnd).toHaveBeenCalledOnce();
  });
});
