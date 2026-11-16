import { describe, expect, it } from "vitest";
import { STATUSES, STATUS_LABELS, asTaskStatus, isLegacyStatus } from "./vocabulary";

describe("task statuses", () => {
  it("are five columns, in board order, with no inbox", () => {
    expect([...STATUSES]).toEqual(["backlog", "ready", "in_progress", "review", "done"]);
    expect(Object.keys(STATUS_LABELS)).toEqual([...STATUSES]);
    expect(Object.values(STATUS_LABELS)).not.toContain("Inbox");
  });

  it("reads every current status as itself", () => {
    for (const status of STATUSES) expect(asTaskStatus(status)).toBe(status);
  });

  it("reads the retired inbox as backlog, and only that spelling", () => {
    expect(asTaskStatus("inbox")).toBe("backlog");
    expect(isLegacyStatus("inbox")).toBe(true);
    expect(isLegacyStatus("backlog")).toBe(false);
    for (const spelling of ["Inbox", "INBOX", " inbox", "inbox ", "__proto__", "constructor", "toString"]) {
      expect(asTaskStatus(spelling), spelling).toBeNull();
      expect(isLegacyStatus(spelling), spelling).toBe(false);
    }
  });

  it("refuses what is not a string status", () => {
    for (const value of [null, undefined, 0, {}, [], ["inbox"], true]) expect(asTaskStatus(value)).toBeNull();
  });
});
