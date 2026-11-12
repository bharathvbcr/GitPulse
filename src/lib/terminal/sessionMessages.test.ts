import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { TASK_TERMINAL_STARTING, TERMINAL_SESSION_PENDING } from "./sessionMessages";

/** The production sources that used to carry a copy of one of these sentences. */
const owners = ["../browser/browseRequests.ts", "./launchRequests.ts", "./sessionLifecycle.ts"];

describe("terminal session notices", () => {
  it("say start, never the retired verb launch (UI_RULES §3)", () => {
    for (const text of [TERMINAL_SESSION_PENDING, TASK_TERMINAL_STARTING]) {
      expect(text).not.toMatch(/launch/i);
      expect(text).toMatch(/starting/);
    }
  });

  it("are two different sentences", () => {
    expect(TERMINAL_SESSION_PENDING).not.toBe(TASK_TERMINAL_STARTING);
  });

  it("are the only copy: no owner spells either sentence out", () => {
    for (const file of owners) {
      const source = readFileSync(new URL(file, import.meta.url), "utf8");
      expect(source, file).not.toMatch(/already starting|still starting|launch is still pending/);
    }
  });
});
