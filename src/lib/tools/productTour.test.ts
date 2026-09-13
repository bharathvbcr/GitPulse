import { describe, expect, it } from "vitest";
import { get } from "svelte/store";
import { createProductTour, TOUR_STEPS } from "./productTour";

function fixture(raw: string | null = null) {
  let saved = raw;
  const storage = { getItem: () => saved, setItem: (_key: string, value: string) => { saved = value; } };
  return { storage, tour: createProductTour(() => storage) };
}

describe("product walkthrough lifecycle", () => {
  it("opens on first launch and resumes the saved step", () => {
    const { tour, storage } = fixture();
    tour.initialize();
    expect(get(tour).open).toBe(true);
    tour.next();
    const resumed = createProductTour(() => storage);
    resumed.initialize();
    expect(get(resumed).step).toBe(1);
  });
  it("persists Later without claiming completion and reopens at that step", () => {
    const { tour, storage } = fixture();
    tour.initialize(); tour.next(); tour.dismiss();
    const resumed = createProductTour(() => storage);
    resumed.initialize();
    expect(get(resumed).open).toBe(false);
    expect(get(resumed).status).toBe("deferred");
    resumed.open();
    expect(get(resumed).step).toBe(1);
  });
  it("persists manual resume so a reload keeps the active tour open", () => {
    const { tour, storage } = fixture();
    tour.initialize(); tour.next(); tour.dismiss(); tour.open();
    const resumed = createProductTour(() => storage);
    resumed.initialize();
    expect(get(resumed).open).toBe(true);
    expect(get(resumed).step).toBe(1);
  });
  it("bounds navigation, completes only at the end, and replays from the start", () => {
    const { tour, storage } = fixture();
    tour.initialize(); tour.back();
    expect(get(tour).step).toBe(0);
    expect(tour.finish()).toBe(false);
    for (let i = 0; i < 50; i++) tour.next();
    expect(get(tour).step).toBe(TOUR_STEPS.length - 1);
    expect(tour.finish()).toBe(true);
    const resumed = createProductTour(() => storage);
    resumed.initialize();
    expect(get(resumed).open).toBe(false);
    resumed.open();
    expect(get(resumed).step).toBe(0);
  });
  it.each(['null', '{}', '{', '{"version":2}', '{"version":1,"step":-1,"status":"active"}', '{"version":1,"step":99,"status":"active"}', '{"version":1,"step":0.5,"status":"active"}', '{"version":1,"step":0,"status":"granted"}'])("recovers invalid data: %s", raw => {
    const { tour } = fixture(raw);
    tour.initialize();
    expect(get(tour).open).toBe(true);
    expect(get(tour).step).toBe(0);
  });
  it("reports unavailable storage and allows an explicit session-only exit", () => {
    const tour = createProductTour(() => { throw new Error("denied"); });
    tour.initialize();
    expect(get(tour).error).toContain("could not be loaded");
    expect(tour.dismiss()).toBe(false);
    expect(get(tour).open).toBe(true);
    expect(get(tour).error).toContain("could not be saved");
    tour.closeForSession();
    expect(get(tour).open).toBe(false);
  });
  it("does not claim saved completion on quota failure and can retry", () => {
    let fail = true;
    const tour = createProductTour(() => ({ getItem: () => null, setItem: () => { if (fail) throw new Error("quota"); } }));
    tour.initialize();
    for (let i = 1; i < TOUR_STEPS.length; i++) tour.next();
    expect(tour.finish()).toBe(false);
    expect(get(tour).status).toBe("active");
    fail = false;
    expect(tour.finish()).toBe(true);
    expect(get(tour).error).toBe(null);
  });
  it("initializes once and ignores navigation while closed", () => {
    const { tour } = fixture();
    tour.initialize(); tour.dismiss(); tour.initialize(); tour.next(); tour.back();
    expect(get(tour).open).toBe(false);
    expect(get(tour).step).toBe(0);
    expect(tour.finish()).toBe(false);
  });
});
