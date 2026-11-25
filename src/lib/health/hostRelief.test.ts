import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { get } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import HostReliefPanel from "../components/health/HostReliefPanel.svelte";
import {
  hostRelief,
  loadHostRelief,
  pausedRows,
  reliefChipInput,
  reliefHeadline,
  reliefIsCalm,
  reliefStatusKey,
  requestResumeAll,
  type FrozenTree,
  type ReliefOutcome,
  type ReliefStatus,
} from "./hostRelief";
import { statusChip } from "./hostLoad";
import { bottomBarMetrics } from "./hostLoadMetrics";
import type { HostLoadPulse } from "../stores/hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform")>();
  return { ...actual, isTauri: () => true };
});

afterEach(() => {
  vi.mocked(invoke).mockReset();
});

const NOW = 1_000_000;

function tree(over: Partial<FrozenTree> = {}): FrozenTree {
  return {
    pid: 4242,
    start_us: 7,
    agent_root_pid: 4000,
    terminal_session_id: "term-4000-ab12",
    repo_path: "/work/gitpulse",
    command: "cargo test --workspace",
    footprint_bytes: 9 * 1024 ** 3,
    since_ms: NOW - 120_000,
    advised_only: false,
    ...over,
  };
}

function status(over: Partial<ReliefStatus> = {}): ReliefStatus {
  return {
    version: 1,
    written_at_ms: NOW,
    writer_pid: 321,
    writer_start_us: 1_700_000_000_000_000,
    mode: "act",
    rung: "freeze",
    binding: "compressor_segments",
    slowed: 3,
    would_slow: 0,
    frozen: [tree()],
    last_action: { at_ms: NOW - 120_000, kind: "freeze", detail: "cargo test" },
    failures: [],
    cap_hits: 0,
    ...over,
  };
}

const fresh = (s: ReliefStatus = status()): ReliefOutcome => ({ state: "fresh", status: s });
const stale = (s: ReliefStatus = status()): ReliefOutcome => ({
  state: "stale",
  status: s,
  age_ms: 95_000,
  cause: "age",
});
const unreadable: ReliefOutcome = { state: "unreadable", reason: "not a status this build understands" };

const pulse = (over: Partial<HostLoadPulse> = {}): HostLoadPulse => ({
  state: "fresh",
  pressure: "normal",
  level: "normal",
  age_ms: 1_000,
  enabled: true,
  compressor_thrash: false,
  role: "writer",
  governor: {
    admit: true,
    shed_background: false,
    shed_reactive: false,
    binding: null,
    refusing_for_s: null,
    notice: false,
    overridden: false,
    largest_foreign: null,
  },
  ...over,
});

describe("the Health row for memory protection", () => {
  it("shows mode, rung, binding, and each paused process with agent, repo, command, footprint and age", () => {
    const body = render(HostReliefPanel, { props: { outcome: fresh(), now: NOW } }).body;
    expect(body).toContain('data-state="fresh"');
    expect(body).toContain(">Act<");
    expect(body).toContain("1 build or test paused, 3 agent process(es) slowed");
    expect(body).toContain("the memory compressor is nearly out of segments");
    expect(body).toContain("cargo test --workspace");
    expect(body).toContain(">gitpulse<");
    expect(body).toContain('title="/work/gitpulse"');
    expect(body).toContain("session term-4000-ab12");
    expect(body).toContain("9.00 GB");
    expect(body).toContain("2 min ago");
    expect(body).toContain("1 build or test paused by memory protection");
    expect(body).toContain("Resume all");
    expect(body).toContain("3 agent process(es) at background priority");
  });

  it("lists the failures and the last action", () => {
    const body = render(HostReliefPanel, {
      props: {
        outcome: fresh(
          status({
            failures: [{ at_ms: NOW, pid: 99, what: "SIGSTOP refused: EPERM" }],
            cap_hits: 2,
            last_action: { at_ms: NOW, kind: "cap_resume", detail: "cargo test" },
          }),
        ),
        now: NOW,
      },
    }).body;
    expect(body).toContain("pid 99: SIGSTOP refused: EPERM");
    expect(body).toContain("Last action: cap resume");
    expect(body).toContain("reached 2 time(s)");
  });

  it("says Stale in words, keeps the list, and never says nothing is paused", () => {
    const body = render(HostReliefPanel, { props: { outcome: stale(), now: NOW } }).body;
    expect(body).toContain('data-state="stale"');
    expect(body).toContain("last written 2 min ago");
    expect(body).toContain("may no longer be running");
    expect(body).toContain("a paused process may still be paused");
    expect(body).toContain("cargo test --workspace");
    expect(body).toContain("Resume all");
    expect(body).not.toContain("Memory protection is idle");
    expect(body).not.toContain('data-tone="ok"');
  });

  it("does not promise a Stale supervisor will act on Resume all", () => {
    const requested = render(HostReliefPanel, {
      props: { outcome: stale(), now: NOW, requested: true },
    }).body;
    expect(requested).toContain("host-relief-requested-stale");
    expect(requested).toContain("If the supervisor is not running");
    expect(requested).toContain("kill -CONT");
    expect(requested).not.toContain("resumes them on its next check");
    const live = render(HostReliefPanel, { props: { outcome: fresh(), now: NOW, requested: true } }).body;
    expect(live).toContain("resumes them on its next check");
    expect(live).not.toContain("kill -CONT");
  });

  it("says Unreadable with its reason and shows no list that could pass for an empty one", () => {
    const body = render(HostReliefPanel, { props: { outcome: unreadable, now: NOW } }).body;
    expect(body).toContain('data-state="unreadable"');
    expect(body).toContain("couldn't be read, so whether anything is paused is unknown");
    expect(body).toContain("not a status this build understands");
    expect(body).not.toContain("host-relief-paused");
    expect(body).not.toContain("Memory protection is idle");
    expect(body).not.toContain('data-tone="ok"');
  });

  it("says absent is not a pause, and a calm fresh status is idle", () => {
    const absent = render(HostReliefPanel, { props: { outcome: { state: "absent" }, now: NOW } }).body;
    expect(absent).toContain("No memory protection status has been published");
    expect(absent).not.toContain("Resume all");
    const calm = render(HostReliefPanel, {
      props: { outcome: fresh(status({ rung: "none", frozen: [], slowed: 0, binding: null })), now: NOW },
    }).body;
    expect(calm).toContain("Memory protection is idle");
    expect(calm).toContain('data-tone="ok"');
    expect(calm).not.toContain("Resume all");
  });

  const advise = (over: Partial<ReliefStatus> = {}) =>
    fresh(
      status({
        mode: "advise",
        slowed: 0,
        would_slow: 4,
        frozen: [tree({ advised_only: true })],
        ...over,
      }),
    );

  it("in Advise mode says it would pause, never that something is paused", () => {
    const body = render(HostReliefPanel, { props: { outcome: advise(), now: NOW } }).body;
    expect(body).not.toContain("host-relief-paused");
    expect(body).not.toContain("Resume all");
    expect(body).not.toContain("build or test is paused");
    expect(body).not.toContain("build or test paused");
    expect(body).toContain(">Advise only<");
    expect(body).toContain("In Advise mode it would pause 1 build or test and slow 4 agent process(es)");
    expect(body).toContain("Would pause");
    expect(body).toContain("cargo test --workspace");
    expect(body).toContain("Memory protection is in Advise mode, so nothing was paused or slowed");
    expect(body).not.toContain("Memory protection is idle");
  });

  it("is not idle, and not calm, when the rung is freeze but only slowing happened", () => {
    const outcome = fresh(status({ frozen: [], slowed: 2 }));
    expect(reliefHeadline(outcome)).toBe("2 agent process(es) slowed; nothing is paused.");
    const body = render(HostReliefPanel, { props: { outcome, now: NOW } }).body;
    expect(body).not.toContain("Memory protection is idle");
    expect(body).not.toContain('data-tone="ok"');
    expect(body).toContain("2 agent process(es) slowed");
    expect(body).not.toContain("build or test paused");
  });

  it("is not idle when a pause was tried and failed", () => {
    const outcome = fresh(
      status({
        frozen: [],
        slowed: 0,
        failures: [{ at_ms: NOW, pid: 4242, what: "SIGSTOP refused: EPERM" }],
      }),
    );
    expect(reliefHeadline(outcome)).toContain("tried to act and couldn't");
    const body = render(HostReliefPanel, { props: { outcome, now: NOW } }).body;
    expect(body).not.toContain("Memory protection is idle");
    expect(body).not.toContain('data-tone="ok"');
    expect(body).toContain("Tried to act and couldn't");
    expect(body).toContain("SIGSTOP refused: EPERM");
  });

  it("is idle, and calm, only when nothing is happening and nothing was tried", () => {
    const rest = status({ rung: "none", frozen: [], slowed: 0, binding: null });
    expect(reliefIsCalm(fresh(rest))).toBe(true);
    // A leftover failure from an earlier episode does not make rest uneasy.
    expect(
      reliefIsCalm(fresh({ ...rest, failures: [{ at_ms: 1, pid: 1, what: "old" }] })),
    ).toBe(true);
    expect(reliefIsCalm(fresh(status({ rung: "slow", frozen: [], slowed: 0 })))).toBe(false);
    expect(reliefIsCalm(stale(rest))).toBe(false);
    expect(reliefIsCalm(null)).toBe(false);
  });

  it("offers Resume all when the status is unreadable, since the request can still be written", () => {
    const body = render(HostReliefPanel, { props: { outcome: unreadable, now: NOW } }).body;
    expect(body).toContain("host-relief-resume");
    expect(body).toContain("Resume all");
    const sent = render(HostReliefPanel, { props: { outcome: unreadable, now: NOW, requested: true } }).body;
    expect(sent).toContain("couldn't be verified");
    expect(sent).toContain("kill -CONT");
    const absent = render(HostReliefPanel, { props: { outcome: { state: "absent" }, now: NOW } }).body;
    expect(absent).not.toContain("Resume all");
  });

  it("says why a Stale status is stale", () => {
    const gone: ReliefOutcome = { state: "stale", status: status(), age_ms: 2_000, cause: "writer_gone" };
    expect(reliefHeadline(gone)).toContain("no longer running");
    const ahead: ReliefOutcome = { state: "stale", status: status(), age_ms: 0, cause: "clock_ahead" };
    expect(reliefHeadline(ahead)).toContain("ahead of this computer's clock");
  });

  it("the request-sent note lasts as long as the status it was made against", () => {
    const frozen = fresh();
    const key = reliefStatusKey(frozen);
    // A refresh of the same pause (a later write time) is the same account.
    expect(reliefStatusKey(fresh(status({ written_at_ms: NOW + 5_000 })))).toBe(key);
    // The pause ended: the account moved on, and so does the note.
    expect(reliefStatusKey(fresh(status({ rung: "none", frozen: [], slowed: 0 })))).not.toBe(key);
    expect(reliefStatusKey(stale())).not.toBe(key);
    expect(reliefStatusKey(unreadable)).not.toBe(key);
  });
});

describe("the status bar text", () => {
  it("appends the paused count to what the bar already says", () => {
    const hot = pulse({ level: "critical", pressure: "critical" });
    const plain = statusChip(hot);
    expect(plain).not.toBeNull();
    const chip = statusChip(hot, { paused: 2, unverified: false });
    expect(chip?.text).toBe(`${plain?.text} · 2 paused`);
    expect(statusChip(hot, { paused: 1, unverified: true })?.text).toBe(
      `${plain?.text} · 1 paused (unverified)`,
    );
  });

  it("speaks alone when the bar would have been quiet, and says an unreadable status was not verified", () => {
    const calm = pulse();
    expect(statusChip(calm)).toBeNull();
    expect(statusChip(calm, { paused: 1, unverified: false })?.text).toBe("Memory protection · 1 paused");
    const unknown = statusChip(calm, { paused: 0, unverified: true });
    expect(unknown?.text).toBe("Memory protection · pause status unverified");
    expect(unknown?.tone).toBe("muted");
    expect(statusChip(calm, { paused: 0, unverified: false })).toBeNull();
  });

  it("shows the relief part of the bar even before any host-load reading", () => {
    expect(bottomBarMetrics(null, "full").visible).toBe(false);
    const paused = bottomBarMetrics(null, "full", { paused: 2, unverified: false });
    expect(paused.visible).toBe(true);
    expect(paused.alertChip?.text).toBe("Memory protection · 2 paused");
    expect(paused.severity).toBe("warn");
    const unknown = bottomBarMetrics(null, "full", { paused: 0, unverified: true });
    expect(unknown.visible).toBe(true);
    expect(unknown.alertChip?.text).toBe("Memory protection · pause status unverified");
  });

  it("carries into the bottom bar's alert chip", () => {
    const model = bottomBarMetrics(pulse(), "full", { paused: 1, unverified: false });
    expect(model.alertChip?.text).toContain("1 paused");
    expect(bottomBarMetrics(pulse(), "full").alertChip).toBeNull();
  });

  it("derives the chip input from each outcome, never reading an unreadable one as zero-and-clear", () => {
    expect(reliefChipInput(null)).toEqual({ paused: 0, unverified: false });
    expect(reliefChipInput({ state: "absent" })).toEqual({ paused: 0, unverified: false });
    expect(reliefChipInput(fresh())).toEqual({ paused: 1, unverified: false });
    expect(reliefChipInput(stale())).toEqual({ paused: 1, unverified: true });
    expect(reliefChipInput(unreadable)).toEqual({ paused: 0, unverified: true });
    expect(reliefChipInput(fresh(status({ frozen: [tree({ advised_only: true })] })))).toEqual({
      paused: 0,
      unverified: false,
    });
  });
});

describe("reading and resuming", () => {
  it("rows are one per paused tree", () => {
    const rows = pausedRows(status({ frozen: [tree(), tree({ pid: 5, advised_only: true })] }), NOW);
    expect(rows).toHaveLength(1);
    expect(rows[0].key).toBe("4242:7");
  });

  it("the headline leads with the unverified states", () => {
    expect(reliefHeadline(null)).toContain("Reading");
    expect(reliefHeadline(unreadable)).toContain("unknown");
    expect(reliefHeadline(stale())).toContain("out of date");
  });

  it("calls the status and resume commands", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(fresh());
    expect(await loadHostRelief()).toEqual(fresh());
    expect(invoke).toHaveBeenCalledWith("cmd_host_relief_status");
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    await requestResumeAll();
    expect(vi.mocked(invoke).mock.calls.at(-1)?.[0]).toBe("cmd_host_relief_request_resume");
  });

  it("a read that fails is an unreadable outcome, not the last good one", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(fresh());
    const stop = hostRelief.subscribe(() => {});
    await vi.waitFor(() => expect(get(hostRelief)?.state).toBe("fresh"));
    stop();
    vi.mocked(invoke).mockRejectedValueOnce(new Error("ipc down"));
    const seen: Array<ReliefOutcome | null> = [];
    const stopAgain = hostRelief.subscribe((value) => seen.push(value));
    await vi.waitFor(() => expect(seen.at(-1)?.state).toBe("unreadable"));
    stopAgain();
    expect(seen.at(-1)).toMatchObject({ state: "unreadable", reason: "ipc down" });
  });
});
