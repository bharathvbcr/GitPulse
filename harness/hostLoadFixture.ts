import type { HostLoadStatus, Reading } from "../src/lib/stores/hostLoadStore";

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
        shed: { background: false, reactive: false },
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
    },
    procs: null,
    tick,
    problems: [],
  };
}
