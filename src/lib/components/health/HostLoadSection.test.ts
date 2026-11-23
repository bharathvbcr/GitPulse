import { readFileSync } from "node:fs";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import HostLoadSection from "./HostLoadSection.svelte";
import { hostLoadStatusFixture } from "../../../../harness/hostLoadFixture";
import { bytes } from "../../health/hostLoad";
import { hostLoadStatus, refreshHostLoad, resetHostLoadForTests } from "../../stores/hostLoadStore";
import {
  DEFAULT_MANVI_STEWARD_SETTINGS,
  isMasterStewardEnabled,
  manviSteward,
  refreshManviSteward,
  resolveEffectiveStewardPolicy,
  resetManviStewardForTests,
  saveManviStewardSettings,
  setManviStewardForTest,
} from "../../stores/stewardStore";
import {
  BANNED_UI_TERMS,
  RETIRED_VERB_TERMS,
  VOICE_TERMS,
  findBannedTerms,
} from "../../ui/copy/vocabulary";
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
  resetManviStewardForTests();
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

  it("surfaces Manvi system steward status, active critical slowdown, latest directive, reclaimed storage, and Settings deep link", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(hostLoadStatusFixture());
    await refreshHostLoad();

    setManviStewardForTest({
      settings: DEFAULT_MANVI_STEWARD_SETTINGS,
      effective: resolveEffectiveStewardPolicy(DEFAULT_MANVI_STEWARD_SETTINGS, null, null),
      repo_common_dir: null,
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "Claude API via Manvi",
      running: false,
      critical_throttle_active: true,
      throttled_count: 2,
      last_check_ms: 1_763_550_000_000,
      next_check_ms: 1_763_550_180_000,
      history: [
        {
          id: "steward-health-1",
          trigger: "critical_wake",
          ran_at_ms: 1_763_550_000_000,
          outcome: "acted",
          tier: "severe",
          binding: "low_disk",
          manvi_mode: "cloud",
          manvi_model: "claude-sonnet-4-6",
          model_generated: true,
          summary: "Host severe (free disk space); reclaimed eligible caches and messaged sessions.",
          directive: "Pause heavy builds and remove throwaway scratch targets until disk recovers.",
          messaged_sessions: 2,
          messaged_repositories: 1,
          skipped_untrusted_repositories: 0,
          throttled_processes: 2,
          restored_processes: 0,
          reclaimed_bytes: 3 * 1024 * 1024 * 1024,
          cleaned_items: [],
          warnings: [],
        },
      ],
    });

    const body = render(HostLoadSection).body;
    expect(body).toContain('data-testid="host-load-steward"');
    expect(body).toContain('data-testid="host-load-steward-run"');
    expect(body).toContain('data-testid="host-load-steward-configure"');
    expect(body).toContain("Opens Settings → Agents → Manvi system steward.");
    expect(body).toContain("Active (cloud · claude-sonnet-4-6) · checks every 3 min");
    expect(body).toContain("Critical slowdown active · 2 agent worker processes throttled");
    expect(body).toContain(`Reclaimed ${bytes(3 * 1024 * 1024 * 1024)}`);
    expect(body).toContain("Pause heavy builds and remove throwaway scratch targets");

    const stewardHtml = body.slice(body.indexOf('data-testid="host-load-steward"'));
    const visibleStewardText = stewardHtml
      .replace(/<!--[\s\S]*?-->/g, " ")
      .replace(/<[^>]+>/g, " ")
      .replace(/\s+/g, " ")
      .trim();
    expect(findBannedTerms(visibleStewardText, BANNED_UI_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleStewardText, RETIRED_VERB_TERMS)).toEqual([]);
    expect(findBannedTerms(visibleStewardText, VOICE_TERMS)).toEqual([]);
  });

  it("computes machine-wide steward activity accurately across per-repo opt-outs, env overrides, and null-repo refreshes", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(hostLoadStatusFixture());
    await refreshHostLoad();

    // Case 1: Global `enabled = true`, while active repo `/repo/opted-out/.git` has
    // `enabled = false`. Machine-wide Health summary must STILL report `Active (...)`
    // rather than falsely claiming "Switched off in Settings"!
    const globalOnRepoOff = {
      ...DEFAULT_MANVI_STEWARD_SETTINGS,
      enabled: true,
      repositories: {
        "/repo/opted-out/.git": { enabled: false },
      },
    };
    setManviStewardForTest({
      settings: globalOnRepoOff,
      effective: resolveEffectiveStewardPolicy(globalOnRepoOff, "/repo/opted-out/.git", null),
      repo_common_dir: "/repo/opted-out/.git",
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "Claude API via Manvi",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: null,
      next_check_ms: 1_763_550_180_000,
      history: [],
    });
    expect(isMasterStewardEnabled(globalOnRepoOff, null)).toBe(true);
    // A status from a build that predates `env_forced` carries no key at all:
    // that is "no override", not "forced to something".
    const missingKey = (undefined as unknown) as boolean | null;
    expect(isMasterStewardEnabled(globalOnRepoOff, missingKey)).toBe(true);
    expect(isMasterStewardEnabled({ ...globalOnRepoOff, enabled: false }, missingKey)).toBe(false);
    const activeBody = render(HostLoadSection).body;
    expect(activeBody).toContain("Active (cloud · claude-sonnet-4-6) · checks every 3 min");
    expect(activeBody).not.toContain("Switched off in Settings");

    // A key Anthropic refused is not an active model.
    setManviStewardForTest({
      settings: globalOnRepoOff,
      effective: resolveEffectiveStewardPolicy(globalOnRepoOff, "/repo/opted-out/.git", null),
      repo_common_dir: "/repo/opted-out/.git",
      env_override: null,
      env_forced: null,
      manvi_state: "rejected",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "Anthropic rejected the API key Manvi used.",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: null,
      next_check_ms: 1_763_550_180_000,
      history: [],
    });
    const rejectedBody = render(HostLoadSection).body;
    expect(rejectedBody).toContain("Anthropic rejected the API key");
    expect(rejectedBody).not.toContain("Active (cloud");

    // Case 2: `GITPULSE_MANVI_STEWARD=0` forces steward off even when a repository override
    // has `enabled = true`. Machine-wide Health summary must report "Switched off in Settings"!
    const repoOnEnvForcedOff = {
      ...DEFAULT_MANVI_STEWARD_SETTINGS,
      enabled: false,
      repositories: {
        "/repo/enabled/.git": { enabled: true },
      },
    };
    const forcedOffEnv = "GITPULSE_MANVI_STEWARD=0 forces the steward off";
    expect(isMasterStewardEnabled(repoOnEnvForcedOff, false)).toBe(false);
    setManviStewardForTest({
      settings: repoOnEnvForcedOff,
      effective: resolveEffectiveStewardPolicy(repoOnEnvForcedOff, null, false),
      repo_common_dir: null,
      env_override: forcedOffEnv,
      env_forced: false,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "Claude API via Manvi",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: null,
      next_check_ms: null,
      history: [],
    });
    const forcedOffBody = render(HostLoadSection).body;
    expect(forcedOffBody).toContain("Switched off in Settings");

    // Case 3: `refreshManviSteward(null)` (called by Health) preserves `repo_common_dir`
    // held by Settings, and `saveManviStewardSettings` handles a `null` IPC response safely.
    setManviStewardForTest({
      settings: globalOnRepoOff,
      effective: resolveEffectiveStewardPolicy(globalOnRepoOff, "/repo/opted-out/.git", null),
      repo_common_dir: "/repo/opted-out/.git",
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: null,
      next_check_ms: 1_763_550_180_000,
      history: [],
    });
    vi.mocked(invoke).mockResolvedValueOnce({
      settings: globalOnRepoOff,
      effective: resolveEffectiveStewardPolicy(globalOnRepoOff, null, null),
      repo_common_dir: null,
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: 1_763_550_000_000,
      next_check_ms: 1_763_550_180_000,
      history: [],
    });
    await refreshManviSteward(null);
    expect(get(manviSteward).repo_common_dir).toBe("/repo/opted-out/.git");
    expect(get(manviSteward).effective.enabled).toBe(false);

    // When Settings explicitly closes a repository (`preserveRepoContext: false`),
    // stale `repo_common_dir` and per-repository `effective` overrides must clear.
    vi.mocked(invoke).mockResolvedValueOnce({
      settings: globalOnRepoOff,
      effective: resolveEffectiveStewardPolicy(globalOnRepoOff, null, null),
      repo_common_dir: null,
      env_override: null,
      env_forced: null,
      manvi_state: "ready",
      manvi_mode: "cloud",
      manvi_model: "claude-sonnet-4-6",
      manvi_detail: "",
      running: false,
      critical_throttle_active: false,
      throttled_count: 0,
      last_check_ms: 1_763_550_000_000,
      next_check_ms: 1_763_550_180_000,
      history: [],
    });
    await refreshManviSteward(null, { preserveRepoContext: false });
    expect(get(manviSteward).repo_common_dir).toBeNull();
    expect(get(manviSteward).effective.enabled).toBe(true);

    vi.mocked(invoke).mockResolvedValueOnce(null);
    const saved = await saveManviStewardSettings(
      { ...globalOnRepoOff, interval_secs: 300 },
      "/repo/opted-out",
    );
    expect(saved.settings.interval_secs).toBe(300);
    expect(get(manviSteward).settings.interval_secs).toBe(300);
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
