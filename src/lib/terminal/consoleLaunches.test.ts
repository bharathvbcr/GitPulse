import { get } from "svelte/store";
import { afterEach, describe, expect, it } from "vitest";
import {
  consoleLaunchRequests,
  consumeConsoleLaunch,
  enqueueConsoleLaunch,
} from "./consoleLaunches";

afterEach(() => {
  while (consumeConsoleLaunch()) {
    /* drain */
  }
});

describe("consoleLaunchRequests", () => {
  it("queues and consumes one command", () => {
    enqueueConsoleLaunch({ command: "cargo install --git x --locked --force devmap-cli", label: "Install" });
    expect(get(consoleLaunchRequests)).toHaveLength(1);
    const got = consumeConsoleLaunch();
    expect(got?.label).toBe("Install");
    expect(get(consoleLaunchRequests)).toHaveLength(0);
  });

  it("refuses an empty command", () => {
    expect(() => enqueueConsoleLaunch({ command: "  ", label: "x" })).toThrow(/empty/);
  });

  it("dedupes an identical waiting command", () => {
    enqueueConsoleLaunch({ command: "cargo install x", label: "a" });
    enqueueConsoleLaunch({ command: "cargo install x", label: "b" });
    expect(get(consoleLaunchRequests)).toHaveLength(1);
  });

  it("refuses a command larger than the console bound", () => {
    expect(() =>
      enqueueConsoleLaunch({ command: "x".repeat(9000), label: "big" }),
    ).toThrow(/bound|long|large/i);
  });

  it("caps the waiting queue at two", () => {
    enqueueConsoleLaunch({ command: "a", label: "a" });
    enqueueConsoleLaunch({ command: "b", label: "b" });
    expect(() => enqueueConsoleLaunch({ command: "c", label: "c" })).toThrow(/Two console/);
  });
});
