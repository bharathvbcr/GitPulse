import type { HostLoadSettings, HostLoadStatus, IdleReport, LeaseStatus, PreviousBoot, ProcView, Reading, TracePeaks, TraceRecord } from "../src/lib/stores/hostLoadStore";

/**
 * What `cmd_host_load_status` returns on a calm Mac where another GitPulse
 * process is the snapshot's writer and this one reads it: a fresh sensor at
 * normal pressure with a whole snapshot behind it, and no process view (only
 * the writer has one). The status bar reads the pulse from it on mount, so a
 * harness that renders the app shell needs it; it is typed against the store
 * so a field the backend adds or renames fails the type check here rather
 * than drifting silently.
 */
export function hostLoadStatusFixture(): HostLoadStatus {
  const value = <T>(v: T): Reading<T> => ({ status: "value", value: v });
  const writer = { pid: 4321, start_us: 1_760_000_000_000_000, version: "fixture", path: "/Applications/GitPulse.app/Contents/MacOS/gitpulse" };
  const tick = { ticks: 120, overruns: 0, missed: 0, last_us: 2_400, max_us: 9_800, cpu_ns: 310_000_000 };
  return {
    platform: "governed",
    role: "reader",
    sensor: { state: "fresh", age_ms: 600, level: "normal", enabled: true, reason: null },
    pressure: "normal",
    governor: {
      verdict: {
        admit: true,
        shed: { background: false, reactive: false, cause: null },
        reason: "healthy",
        tier: "clear",
        binding: null,
        sensor: "fresh",
        age_ms: 600,
        message: "memory pressure normal (kernel level 1)",
        warnings: [],
      },
      refusing_for_s: null,
      notice: false,
      largest_foreign: null,
      override_remaining_s: null,
    },
    snapshot_path: "/Users/fixture/Library/Application Support/GitPulse/host-load.v1",
    writer,
    writer_is_self: false,
    snapshot: {
      version: 1,
      length: 512,
      seq: 240,
      written: { wall_ms: 1_760_000_240_000, mono_ns: 240_000_000_000, boot_id: 1 },
      enabled: true,
      platform: "governed",
      cadence_ms: 2_000,
      writer,
      host: {
        pressure: value("normal"),
        swap_used: value(512 << 20),
        swap_total: value(2048 << 20),
        compressor: value({ thrashing_detected: 0, free_below_reserved: 0, swapouts_pressure: 0, major_compaction_segments_freed: 0 }),
        compressed_bytes: value(1_200_000_000),
        compressor_segments: value({ total: 254_515, limit: 1_677_721 }),
        load: value([3.1, 2.8, 2.5]),
        cores: value({ logical: 12, levels: 2, perf0: 8, perf1: 4 }),
        memsize: value(36 * 2 ** 30),
        open_files: value(9_000),
        max_files: value(122_880),
        free_disk: value(180 * 2 ** 30),
        thermal: value("nominal"),
        power: value("ac"),
      },
      compressor_delta: value({ thrashing_detected: 0, free_below_reserved: 0, swapouts_pressure: 0, major_compaction_segments_freed: 0 }),
      compressor_thrash: false,
      processes: value(640),
      unreadable_processes: 0,
      claude: value({ launched: 1, observed: 2 }),
      codex: value({ launched: 0, observed: 0 }),
      launched_footprint: value(900 << 20),
      observed_footprint: value(1_800 << 20),
      largest_foreign: value(2_200 << 20),
      launched_write_rate: value(0),
      max_tree_write_rate: value(0),
      memguard_present: false,
      memguard_warn: false,
      memguard_pid: 0,
      orphans: 0,
      tick,
      disk_fill_rate: value(0),
      disk_fill_sustained: value(0),
      verdict: value({ tier: "clear", binding: null, admit: true, shed_background: false, shed_reactive: false }),
    },
    procs: null,
    tick,
    problems: [],
    previous_boot: null,
  };
}

/**
 * `cmd_host_load_settings` as the backend answers it: every field of
 * `HostLoadSettings`, thresholds included, with the starting values written
 * out here rather than imported from the store, so a changed default is a
 * fixture edit a reviewer sees. A reply short of `thresholds` is not a smaller
 * fixture: the input rows read `thresholds[input]` and throw on mount.
 */
export function hostLoadSettingsFixture(): HostLoadSettings {
  return {
    enabled: true,
    govern_launches: true,
    hook_mode: "enforce",
    leases: { mode: "on", slots: 4, wait_max_s: 1800, wait_hard_s: 14400, incremental_off: false },
    idle: { idle_after_s: 900, park_after_s: 3600 },
    thresholds: {
      swap: { mode: "refuse", elevated_at: 0.5, severe_at: 0.75 },
      compressor_segments: { mode: "refuse", elevated_at: 0.6, severe_at: 0.8 },
      load: { mode: "shed", elevated_at: 3, severe_at: 6 },
      thermal: { mode: "shed" },
      battery: { mode: "off" },
      // The automatic floors for a 64 GiB computer, as the backend shows them.
      disk: { mode: "refuse", elevated_at: 32, severe_at: 16 },
      disk_auto: true,
      disk_guard: {
        fill: { mode: "refuse", elevated_within_min: 30, severe_within_min: 10 },
        coverage: { mode: "refuse", headroom_gib: 60 },
      },
    },
  };
}

/**
 * `cmd_host_load_leases` on a calm Mac with nothing building: every count
 * read, none held, none waiting.
 */
export function leaseStatusFixture(): LeaseStatus {
  return {
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
    now_mono_ns: 5_000_000_000_000,
  };
}

/** `cmd_host_load_idle` with no agent session running. */
export function idleReportFixture(): IdleReport {
  return { sessions: [], idle: 0, unknown: 0, idle_after_s: 900, park_after_s: 3600, error: null };
}

const GIB = 2 ** 30;

/** The trace record of the previous boot's `i`th reading, one second apart, swap climbing. */
function traceRecordFixture(i: number): TraceRecord {
  return {
    wall_s: 1_760_000_000 + i,
    mono_ms: 5_000_000 + i * 1_000,
    boot_id: 7,
    pressure: i === 2 ? "critical" : "warn",
    tier: i === 2 ? "severe" : "elevated",
    binding: i === 2 ? "swap" : null,
    admit: i !== 2,
    swap_used: (10 + i * 3) * GIB,
    compressed_bytes: 8 * GIB,
    load_1m: 14.5 + i,
    free_disk: 42 * GIB,
    launched_sessions: 3,
    observed_sessions: 4,
    largest_foreign: 6 * GIB,
    launched_footprint: 12 * GIB,
    thermal: "fair",
    power: "ac",
    segment_fraction: 0.91,
    dropped_before: 0,
    dropped_total: 0,
  };
}

/**
 * What the last boot's trace ended with: three readings, the governor
 * refusing on the third, with `dropped` readings the trace write lost. The
 * peaks are written out, not derived, so the page shows a number a person can
 * look up in this file.
 */
/**
 * `HostLoadStatus.procs` as the process scan answers it on the writer: three
 * Claude sessions (one started by GitPulse, two observed), each with a disk
 * write rate in bytes per second. `rates` is one reading per session, in pid
 * order, so a scenario can say "no baseline yet" or "nobody is writing".
 */
export function procViewFixture(rates: [Reading<number>, Reading<number>, Reading<number>], truncated = false): ProcView {
  const value = <T>(v: T): Reading<T> => ({ status: "value", value: v });
  const session = (pid: number, cwd: string | null, tree: string | null, rate: Reading<number>) => ({
    kind: "claude" as const,
    pid,
    start_us: 1_760_000_000_000_000 + pid,
    cwd,
    tree,
    run_id: null,
    parent_pid: null,
    cpu_rate: value(0),
    footprint: value(600 << 20),
    write_rate: rate,
    members: 3,
    unreadable_members: 0,
  });
  return {
    ok: true,
    error: null,
    processes: 640,
    other_users: 0,
    unreadable: 0,
    claude: { launched: 1, observed: 2 },
    codex: { launched: 0, observed: 0 },
    other: { launched: 0, observed: 0 },
    sessions: [
      session(1201, "/Users/fixture/code/alpha", "session:term-1", rates[0]),
      session(1202, "/Users/fixture/code/beta", null, rates[1]),
      session(1203, null, null, rates[2]),
    ],
    sessions_truncated: truncated,
    trees: [],
    launched_footprint: value(600 << 20),
    observed_footprint: value(1_200 << 20),
    foreign_top: [],
    largest_foreign: value(0),
    launched_write_rate: rates[0],
    max_tree_write_rate: rates[0],
    memguard: {
      present: false,
      pid: null,
      start_us: null,
      exec: null,
      flags: [],
      log_path: { status: "unavailable", value: "unsupported" },
      heartbeat_age_s: { status: "unavailable", value: "unsupported" },
      heartbeat_interval_s: null,
      warn: false,
      warnings: [],
    },
    orphans: [],
  };
}

export function previousBootFixture(dropped: number): PreviousBoot {
  const peaks: TracePeaks = {
    records: 3,
    max_tier: "severe",
    refused: true,
    max_swap_used: 16 * GIB,
    max_compressed_bytes: 8 * GIB,
    max_load_1m: 16.5,
    min_free_disk: 42 * GIB,
    max_launched_footprint: 12 * GIB,
    max_largest_foreign: 6 * GIB,
    max_segment_fraction: 0.91,
    worst_thermal: "fair",
    max_pressure: "critical",
  };
  return { ended_wall_s: 1_760_000_002, records: [0, 1, 2].map(traceRecordFixture), peaks, dropped, error: null };
}
