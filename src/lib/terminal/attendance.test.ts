import { describe, expect, it } from "vitest";
import { createAttendance, MAX_VISIBLE_SESSIONS } from "./attendance";

/** Runs scheduled work when the test says so, rather than on a microtask. */
function manual() {
  const queue: (() => void)[] = [];
  return {
    schedule: (run: () => void) => queue.push(run),
    run() {
      const pending = queue.splice(0);
      for (const item of pending) item();
    },
    get depth() {
      return queue.length;
    },
  };
}

function harness() {
  const sent: string[][] = [];
  const clock = manual();
  const attendance = createAttendance(async (ids) => {
    sent.push(ids);
  }, clock.schedule);
  return { attendance, sent, clock };
}

describe("terminal attendance", () => {
  it("reports the sessions on screen", async () => {
    const { attendance, sent, clock } = harness();
    attendance.report("term-1", true);
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([["term-1"]]);
  });

  it("sends one list per turn, not one per change", async () => {
    // A tab switch is two reports. Pushing each would tell the backend, for an
    // instant, that no session is visible or that two are — and it acts on it.
    const { attendance, sent, clock } = harness();
    attendance.report("term-1", true);
    attendance.report("term-1", false);
    attendance.report("term-2", true);
    expect(clock.depth).toBe(1);
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([["term-2"]]);
  });

  it("keeps both halves of a split visible", async () => {
    const { attendance, sent, clock } = harness();
    attendance.report("term-1", true);
    attendance.report("term-2", true);
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([["term-1", "term-2"]]);
  });

  it("does not repeat a list that has not changed", async () => {
    const { attendance, sent, clock } = harness();
    attendance.report("term-1", true);
    clock.run();
    await Promise.resolve();
    attendance.report("term-1", true);
    clock.run();
    await Promise.resolve();
    await attendance.flush();
    expect(sent).toEqual([["term-1"]]);
  });

  it("retries after a failed push rather than assuming it landed", async () => {
    const attempts: string[][] = [];
    let fail = true;
    const clock = manual();
    const attendance = createAttendance(async (ids) => {
      attempts.push(ids);
      if (fail) throw new Error("ipc down");
    }, clock.schedule);
    attendance.report("term-1", true);
    clock.run();
    await Promise.resolve();
    await Promise.resolve();
    expect(attempts).toEqual([["term-1"]]);
    fail = false;
    await attendance.flush();
    expect(attempts).toEqual([["term-1"], ["term-1"]]);
  });

  it("forgetting a closed tab withdraws it", async () => {
    const { attendance, sent, clock } = harness();
    attendance.report("term-1", true);
    clock.run();
    await Promise.resolve();
    attendance.forget("term-1");
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([["term-1"], []]);
  });

  it("forgetting a session it never knew changes nothing", async () => {
    const { attendance, sent, clock } = harness();
    attendance.forget("term-9");
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([]);
  });

  it("ignores a session with no backend id yet", async () => {
    // A tab reports before its PTY has been spawned. An empty id would make
    // the list wrong for every session, because an empty string matches none.
    const { attendance, sent, clock } = harness();
    attendance.report("", true);
    clock.run();
    await Promise.resolve();
    expect(sent).toEqual([]);
  });

  it("never sends more than the backend accepts", async () => {
    const { attendance, sent, clock } = harness();
    for (let index = 0; index < MAX_VISIBLE_SESSIONS * 4; index += 1) {
      attendance.report(`term-${index}`, true);
    }
    clock.run();
    await Promise.resolve();
    expect(sent[0]).toHaveLength(MAX_VISIBLE_SESSIONS);
  });
});
