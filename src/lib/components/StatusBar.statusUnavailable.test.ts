/**
 * The status bar's notice that `git status` stopped answering. The poll that
 * raises it is covered in stores/__tests__/repoStore.statusPoll.test.ts and the
 * visibility rule in ui/statusBarMode.test.ts; what only a render can show is
 * that the strip is on screen in each mode, including a hidden bar that the
 * signal forces back.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";
import { interfaceStore } from "../stores/interfaceStore";

const poll = vi.hoisted(() => ({ since: null as number | null }));

// The real store, with the one field the poll would have set.
vi.mock("../stores/repoStore", async (importOriginal) => {
  const real = await importOriginal<typeof import("../stores/repoStore")>();
  return {
    ...real,
    repoStore: {
      ...real.repoStore,
      subscribe: (run: (value: never) => void) =>
        real.repoStore.subscribe((value) =>
          run({ ...value, statusUnavailableSince: poll.since } as never),
        ),
    },
  };
});

const bar = () => render(StatusBar).body;
const STRIP = 'data-testid="status-unavailable"';

afterEach(() => {
  poll.since = null;
  interfaceStore.setStatusBarMode("full");
});

describe("StatusBar status-unavailable strip", () => {
  it("is absent while the status poll answers", () => {
    for (const mode of ["full", "minimal", "hidden"] as const) {
      interfaceStore.setStatusBarMode(mode);
      expect(bar(), mode).not.toContain(STRIP);
    }
  });

  it("is on screen in full and minimal mode, with the time it went quiet", () => {
    poll.since = Date.UTC(2026, 9, 31, 12, 0, 0);
    for (const mode of ["full", "minimal"] as const) {
      interfaceStore.setStatusBarMode(mode);
      const body = bar();
      expect(body, mode).toContain(STRIP);
      expect(body, mode).toContain("Status unavailable");
      expect(body, mode).toContain("git status has not answered since");
    }
  });

  it("pulls a hidden bar back, and says why", () => {
    interfaceStore.setStatusBarMode("hidden");
    poll.since = Date.now();
    const body = bar();
    expect(body).toContain('role="status"');
    expect(body).toContain(STRIP);
    expect(body).toContain("Shown despite the hidden status-bar setting");
  });
});
