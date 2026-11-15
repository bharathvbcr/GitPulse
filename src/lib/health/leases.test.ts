import { describe, expect, it } from "vitest";
import {
  activityLine,
  capacityLine,
  holderCells,
  missesLine,
  parkOffered,
  unleasedLine,
  waiterCells,
  waitingLabel,
} from "./leases";
import type { IdleSession, LeaseRecord, LeaseStatus } from "../stores/hostLoadStore";

const record = (over: Partial<LeaseRecord> = {}): LeaseRecord => ({
  pid: 10,
  start_us: 1,
  session_key: "terminal:term-1",
  priority: "terminal",
  terminal_session: "term-1",
  run_id: null,
  cwd: "/work/repo",
  label: "cargo build",
  enqueued_mono_ns: 1_000_000_000,
  granted_mono_ns: 1_000_000_000,
  tier_at_grant: "clear",
  verdict_unavailable_reason: null,
  overflow: false,
  ...over,
});

const status = (over: Partial<LeaseStatus> = {}): LeaseStatus => ({
  on: true,
  configured: 4,
  capacity: { configured: 4, slots: 4, refuses: false, tier: "clear", note: null },
  holders: [],
  queue: [],
  unreadable: [],
  error: null,
  links_missing: null,
  misses: 0,
  misses_truncated: false,
  misses_error: null,
  unleased_running: 0,
  unleased_error: null,
  now_mono_ns: 721_000_000_000,
  ...over,
});

describe("lease wording", () => {
  it("says how many slots may run and why, and says when leases are off", () => {
    expect(capacityLine(status())).toBe("4 of 4 build slots may run.");
    expect(
      capacityLine(
        status({ capacity: { configured: 4, slots: 2, refuses: false, tier: "elevated", note: "halved from 4 while the host load is elevated" } }),
      ),
    ).toBe("2 of 4 build slots may run (halved from 4 while the host load is elevated).");
    expect(
      capacityLine(status({ capacity: { configured: 4, slots: 0, refuses: true, tier: "severe", note: "no new builds while the host load is severe" } })),
    ).toContain("No new build may start");
    expect(capacityLine(status({ on: false }))).toContain("switched off");
  });

  it("never reads a count that couldn't be taken as zero", () => {
    expect(missesLine(status({ misses: null, misses_error: "no such directory" }))).toContain("couldn't be counted: no such directory");
    expect(missesLine(status({ misses: 0 }))).toContain("No build ran without a lease");
    expect(missesLine(status({ misses: 2 }))).toBe("2 builds ran without a lease in the last 15 minutes.");
    // A capped read is a floor and never says "No build".
    const capped = missesLine(status({ misses: 0, misses_truncated: true }));
    expect(capped).toContain("At least 0 builds");
    expect(capped).not.toContain("No build ran");
    expect(missesLine(status({ misses: 3, misses_truncated: true }))).toContain("At least 3 builds");
    expect(unleasedLine(status({ unleased_running: null, unleased_error: "the process table couldn't be read" }))).toContain("couldn't be counted");
    expect(unleasedLine(status({ unleased_running: 0 }))).toContain("Every running heavy build holds a slot");
    expect(unleasedLine(status({ unleased_running: 3 }))).toContain("3 cargo or go builds running hold no slot");
  });

  it("shows a lock with no record as an unknown holder, and ages a holder and a waiter", () => {
    expect(holderCells({ slot: 2, pid: 99, record: null }, 5)).toEqual(["2", "pid 99, unknown", "unknown", "unknown"]);
    const cells = holderCells({ slot: 1, pid: 10, record: record({ overflow: true }) }, 721_000_000_000);
    expect(cells[1]).toContain("started without a slot");
    expect(cells[3]).toBe("12 min");
    expect(waiterCells({ name: "n", record: record({ granted_mono_ns: null }) }, 1, 721_000_000_000)[0]).toBe("2");
  });

  it("names a terminal tab's place in the queue", () => {
    const queue = [
      { name: "a", record: record({ terminal_session: "term-a" }) },
      { name: "b", record: record({ terminal_session: "term-b" }) },
      { name: "c", record: record({ terminal_session: "term-c" }) },
    ];
    expect(waitingLabel(queue, "term-a")).toBe("Waiting for a build slot (1st)");
    expect(waitingLabel(queue, "term-b")).toBe("Waiting for a build slot (2nd)");
    expect(waitingLabel(queue, "term-c")).toBe("Waiting for a build slot (3rd)");
    expect(waitingLabel(queue, "term-z")).toBeNull();
  });

  it("explains each activity state", () => {
    const base: IdleSession = {
      terminal_session: "term-1", kind: "claude", pid: 1, start_us: 1, cwd: "/w",
      footprint: { status: "value", value: 1 }, park_blocked: null,
      activity: { state: "idle", since_ms: 1_200_000, why: "it waits" },
    };
    expect(activityLine(base)).toBe("Idle for 20 min: it waits.");
    expect(activityLine({ ...base, activity: { state: "busy", why: "it builds" } })).toBe("Not idle: it builds.");
    expect(activityLine({ ...base, activity: { state: "unknown", why: "CPU unreadable" } })).toContain("Not known to be idle");
  });

  it("offers Park only for an idle, resumable session that has been idle past the park time", () => {
    const idle: IdleSession = {
      terminal_session: "term-1", kind: "claude", pid: 1, start_us: 1, cwd: "/w",
      footprint: { status: "value", value: 1 }, park_blocked: null,
      activity: { state: "idle", since_ms: 3_600_000, why: "it waits" },
    };
    const report = { park_after_s: 3600 };
    expect(parkOffered(idle, report)).toBe(true);
    expect(parkOffered({ ...idle, activity: { state: "idle", since_ms: 3_599_999, why: "x" } }, report)).toBe(false);
    expect(parkOffered({ ...idle, park_blocked: "GitPulse did not launch it" }, report)).toBe(false);
    expect(parkOffered({ ...idle, terminal_session: null }, report)).toBe(false);
    expect(parkOffered({ ...idle, activity: { state: "unknown", why: "x" } }, report)).toBe(false);
    expect(parkOffered({ ...idle, activity: { state: "busy", why: "x" } }, report)).toBe(false);
  });
});
