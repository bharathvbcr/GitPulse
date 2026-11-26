import { describe, expect, it } from "vitest";
import { BUSY_GRACE_FAILURES, BUSY_GRACE_MS, createBusyGrace, isBusy } from "./busyGrace";
import { WorkbenchError } from "./client";

const busy = () => new WorkbenchError("busy", "Task storage is busy");

function setup() {
  let now = 1_000_000;
  const grace = createBusyGrace(() => now);
  return { grace, advance: (ms: number) => { now += ms; }, now: () => now };
}

describe("isBusy", () => {
  it("is true only for the host's busy refusal", () => {
    expect(isBusy(busy())).toBe(true);
    expect(isBusy(new WorkbenchError("store_error", "Task storage is busy"))).toBe(false);
    expect(isBusy(new Error("busy"))).toBe(false);
    expect(isBusy({ code: "busy" })).toBe(false);
    expect(isBusy(null)).toBe(false);
  });
});

describe("createBusyGrace", () => {
  it("holds nothing before a read has landed", () => {
    const { grace } = setup();
    expect(grace.failed(busy())).toBe(false);
    expect(grace.held()).toBeNull();
  });

  it("holds the last good answer through busy refusals that are few or recent", () => {
    const { grace, advance, now } = setup();
    const readAt = now();
    grace.succeeded();
    expect(grace.failed(busy())).toBe(true);
    expect(grace.held()).toBe(readAt);
    // Many refusals inside the window: still a burst.
    advance(BUSY_GRACE_MS - 1);
    for (let i = 0; i < 5; i++) expect(grace.failed(busy())).toBe(true);
  });

  it("lets go once the refusals are repeated AND have gone on for the grace", () => {
    const { grace, advance } = setup();
    grace.succeeded();
    expect(BUSY_GRACE_FAILURES).toBe(2);
    expect(grace.failed(busy())).toBe(true);
    advance(BUSY_GRACE_MS);
    expect(grace.failed(busy())).toBe(false);
    expect(grace.held()).toBeNull();
    // With nothing left to hold, the next refusal is reported too.
    expect(grace.failed(busy())).toBe(false);
  });

  // An idle board's last good read can be a minute old; counting the grace from it ended the grace at the
  // second refusal of every burst (1.5 s apart under the loader's backoff), which put the banner in and took it out.
  it("counts the grace from the first refusal, not from a read that was already old", () => {
    const { grace, advance } = setup();
    grace.succeeded();
    advance(60_000);
    expect(grace.failed(busy())).toBe(true);
    advance(1_500);
    expect(grace.failed(busy())).toBe(true);
    advance(2_000);
    expect(grace.failed(busy())).toBe(true);
    advance(BUSY_GRACE_MS);
    expect(grace.failed(busy())).toBe(false);
  });

  it("a success in between starts the count and the clock again", () => {
    const { grace, advance } = setup();
    grace.succeeded();
    advance(BUSY_GRACE_MS * 2);
    expect(grace.failed(busy())).toBe(true);
    grace.succeeded();
    expect(grace.held()).toBeNull();
    expect(grace.failed(busy())).toBe(true);
    expect(grace.failed(busy())).toBe(true);
  });

  it("any other failure is reported at once and ends the hold", () => {
    const { grace } = setup();
    grace.succeeded();
    expect(grace.failed(busy())).toBe(true);
    expect(grace.failed(new WorkbenchError("store_error", "Task storage offline"))).toBe(false);
    expect(grace.held()).toBeNull();
    expect(grace.failed(busy())).toBe(false);
  });

  it("forget drops the hold, as a scope change does", () => {
    const { grace } = setup();
    grace.succeeded();
    expect(grace.failed(busy())).toBe(true);
    grace.forget();
    expect(grace.held()).toBeNull();
    expect(grace.failed(busy())).toBe(false);
  });
});
