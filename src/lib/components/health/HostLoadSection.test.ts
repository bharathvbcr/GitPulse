import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import HostLoadSection from "./HostLoadSection.svelte";
import { hostLoadStatusFixture } from "../../../../harness/hostLoadFixture";
import { hostLoadStatus, refreshHostLoad, resetHostLoadForTests } from "../../stores/hostLoadStore";
import { createAdaptiveTimer, type AdaptiveHost } from "../../runtime/adaptiveTimer";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../platform")>();
  return { ...actual, isTauri: () => true };
});

afterEach(() => {
  vi.useRealTimers();
  vi.mocked(invoke).mockReset();
  resetHostLoadForTests();
});

describe("HostLoadSection", () => {
  it("renders Free disk with total when total_disk is present and omits 'of ...' when total_disk is unavailable", async () => {
    const withTotal = hostLoadStatusFixture();
    vi.mocked(invoke).mockResolvedValueOnce(withTotal);
    await refreshHostLoad();

    const fullBody = render(HostLoadSection).body;
    const withTotalDd = fullBody.match(/data-testid="host-load-free-disk">([^<]+)</)?.[1] ?? "";
    expect(withTotalDd).toContain(" of ");
    expect(fullBody).toContain("Measures immediate unallocated space available to GitPulse");

    const withoutTotal = hostLoadStatusFixture();
    withoutTotal.snapshot!.host.total_disk = { status: "unavailable", value: "older" };
    vi.mocked(invoke).mockResolvedValueOnce(withoutTotal);
    await refreshHostLoad();

    const olderBody = render(HostLoadSection).body;
    const withoutTotalDd = olderBody.match(/data-testid="host-load-free-disk">([^<]+)</)?.[1] ?? "";
    expect(withoutTotalDd).not.toBe("");
    expect(withoutTotalDd).not.toContain(" of ");
    expect(withoutTotalDd).not.toContain("not published");
  });
});

const source = readFileSync(new URL("./HostLoadSection.svelte", import.meta.url), "utf8");

/** A host on vitest's fake clock; `hidden` lets a test put the window in the background. */
function fakeHost(): AdaptiveHost {
  return {
    setTimeout: (handler, ms) => setTimeout(handler, ms),
    clearTimeout: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
    addEventListener: () => {},
    removeEventListener: () => {},
    isHidden: () => false,
  };
}

/**
 * D-frontend-07 / AC13. Vitest has no DOM, so onMount never runs under
 * `render`; the mounted component is driven by the Health browser harness
 * (harness/healthChecks.js, runHostLoadChecks). This pins the same contract in
 * node: the component's own wiring (source contract), the real adaptive timer
 * and the real store on a fake clock against a slow cmd_host_load_status, and
 * what the component renders while that read is pending.
 */
describe("HostLoadSection refresh while a read is slow", () => {
  it("is wired to the adaptive timer and the shared single-flight read, not a bare interval", () => {
    expect(source).toContain("createAdaptiveTimer(() => void refresh(), REFRESH_MS)");
    expect(source).not.toMatch(/\bsetInterval\(/);
    expect(source).toContain("await refreshHostLoad();");
    expect(source).toContain("stopRefresh?.();");
  });

  it("issues one invoke for overlapping ticks, keeps the previous snapshot on screen, and reads again once it settles", async () => {
    vi.useFakeTimers();
    const first = hostLoadStatusFixture();
    vi.mocked(invoke).mockResolvedValueOnce(first);
    await refreshHostLoad();
    expect(vi.mocked(invoke)).toHaveBeenCalledTimes(1);

    // The next read is slow: it settles only when the test says so.
    const next = hostLoadStatusFixture();
    next.sensor.age_ms = 1234;
    let settle: (status: typeof next) => void = () => {};
    vi.mocked(invoke).mockImplementation(
      () => new Promise((resolve) => { settle = resolve as typeof settle; }),
    );

    // The component's wiring: the adaptive timer calling refreshHostLoad.
    const failures: unknown[] = [];
    const stop = createAdaptiveTimer(
      () => void refreshHostLoad().catch((cause) => failures.push(cause)),
      4_000,
      fakeHost(),
      { readLag: () => 0 },
    );
    try {
      await vi.advanceTimersByTimeAsync(4_000);
      expect(vi.mocked(invoke)).toHaveBeenCalledTimes(2);
      // Three more ticks land while that read is in flight: none issues an invoke.
      await vi.advanceTimersByTimeAsync(12_000);
      expect(vi.mocked(invoke)).toHaveBeenCalledTimes(2);

      // Meanwhile the section shows the previous snapshot: not blank, no error.
      expect(get(hostLoadStatus)).toBe(first);
      const pending = render(HostLoadSection).body;
      expect(pending).toContain('data-testid="host-load-sensor"');
      expect(pending).not.toContain("Reading host load");
      expect(pending).not.toContain('role="alert"');

      settle(next);
      await vi.advanceTimersByTimeAsync(0);
      expect(get(hostLoadStatus)).toBe(next);
      expect(failures).toEqual([]);

      // Settled, so the following tick reads again.
      await vi.advanceTimersByTimeAsync(4_000);
      expect(vi.mocked(invoke)).toHaveBeenCalledTimes(3);
    } finally {
      stop();
    }
  });

  it("backs off while the event loop is late and stops while the window is hidden", async () => {
    vi.useFakeTimers();
    vi.mocked(invoke).mockResolvedValue(hostLoadStatusFixture());
    let lag = 0;
    let hidden = false;
    const host: AdaptiveHost = { ...fakeHost(), isHidden: () => hidden };
    const reads = vi.fn();
    const stop = createAdaptiveTimer(() => { reads(); void refreshHostLoad(); }, 4_000, host, { readLag: () => lag });
    try {
      await vi.advanceTimersByTimeAsync(4_000);
      expect(reads).toHaveBeenCalledTimes(1);

      lag = 800;
      await vi.advanceTimersByTimeAsync(4_000); // the tick armed before the lag still fires
      expect(reads).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(4_000); // re-armed on the stretched period
      expect(reads).toHaveBeenCalledTimes(2);
      await vi.advanceTimersByTimeAsync(40_000);
      expect(reads.mock.calls.length).toBeGreaterThanOrEqual(3);

      hidden = true;
      await vi.advanceTimersByTimeAsync(0);
      const before = reads.mock.calls.length;
      await vi.advanceTimersByTimeAsync(300_000);
      expect(reads.mock.calls.length).toBe(before);
    } finally {
      stop();
    }
  });
});
