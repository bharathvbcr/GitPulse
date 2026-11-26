import { derived, get, writable, type Readable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { refusalNotice } from "../health/hostLoad";
import { toastStore } from "./toastStore";
import { diagnostics } from "../diagnostics/diagnostics";
import type { ResourceBudgets } from "../resourceBudgets";

/**
 * Host load as the app's sampler publishes it (`src-tauri/src/sysload`,
 * `docs/LOAD_GOVERNOR.md`).
 *
 * The sampler ticks every 2 s in the app process and emits a small pulse
 * (`host-load-pulse`); Health asks for the full status when it is open. The
 * one value other features consume is {@link hostPressure}: the memory
 * pressure level when the sensor may be acted on, otherwise `"unavailable"`
 * — never a guessed `"normal"`. That is the input card 1c's background gate
 * reads.
 */

export type Why = "unsupported" | "unreadable" | "failed" | "not_yet" | "disabled" | "older";
/** `sysload::Reading<T>`: a value, or why there is none. */
export type Reading<T> = { status: "value"; value: T } | { status: "unavailable"; value: Why };
export type Level = "normal" | "warn" | "critical";
export type SensorState = "fresh" | "stale_recent" | "stale_old" | "dead";
export type PressureInput = Level | "unavailable";
export type Role = "writer" | "reader" | "not_sampling" | "unsupported";
export type Platform = "governed" | "advise" | "unavailable";
export type Thermal = "nominal" | "fair" | "serious" | "critical";
export type Power = "ac" | "battery";
export type AgentKind = "claude" | "codex" | "antigravity" | "dev_council" | "manvi";

export interface SensorReport {
  state: SensorState;
  age_ms: number | null;
  level: Level | null;
  enabled: boolean | null;
  reason: string | null;
}

export interface Clock {
  wall_ms: number;
  mono_ns: number;
  boot_id: number;
}

export interface WriterIdentity {
  pid: number;
  start_us: number;
  version: string;
  path: string;
}

export interface TickStats {
  ticks: number;
  overruns: number;
  missed: number;
  last_us: number;
  max_us: number;
  cpu_ns: number;
}

export interface Compressor {
  thrashing_detected: number;
  free_below_reserved: number;
  swapouts_pressure: number;
  major_compaction_segments_freed: number;
}

export interface CompressorDelta {
  thrashing_detected: number;
  free_below_reserved: number;
  swapouts_pressure: number;
  major_compaction_segments_freed: number;
}

/** `sysload::CompressorSegments`: segments in use against the compressor's limit. */
export interface CompressorSegments {
  total: number;
  limit: number;
}

export interface Cores {
  logical: number;
  levels: number;
  perf0: number;
  perf1: number;
}

/** `sysload::CpuUsage`: system-wide CPU utilization deltas (% across logical CPUs). */
export interface CpuUsage {
  total_pct: number;
  user_pct: number;
  system_pct: number;
}

/** `sysload::GpuSample`: IOKit `IOAccelerator` GPU utilization and unified memory. */
export interface GpuSample {
  utilization_pct: number;
  renderer_pct: number | null;
  tiler_pct: number | null;
  memory_in_use: number | null;
  memory_allocated: number | null;
}

/** `sysload::MemorySample`: physical memory used and Activity Monitor category breakdown. */
export interface MemorySample {
  used_bytes: number;
  app_bytes: number | null;
  wired_bytes: number | null;
  compressed_bytes: number | null;
  cached_bytes: number | null;
}

export interface HostSample {
  pressure: Reading<Level>;
  swap_used: Reading<number>;
  swap_total: Reading<number>;
  compressor: Reading<Compressor>;
  compressed_bytes: Reading<number>;
  compressor_segments: Reading<CompressorSegments>;
  load: Reading<[number, number, number]>;
  cores: Reading<Cores>;
  memsize: Reading<number>;
  open_files: Reading<number>;
  max_files: Reading<number>;
  free_disk: Reading<number>;
  thermal: Reading<Thermal>;
  power: Reading<Power>;
  cpu_usage?: Reading<CpuUsage>;
  gpu?: Reading<GpuSample>;
  memory?: Reading<MemorySample>;
  total_disk?: Reading<number>;
  thermal_temp_c?: Reading<number>;
}

export interface SessionCount {
  launched: number;
  observed: number;
}

export interface Snapshot {
  version: number;
  length: number;
  seq: number;
  written: Clock;
  enabled: boolean;
  platform: Platform;
  cadence_ms: number;
  writer: WriterIdentity;
  host: HostSample;
  compressor_delta: Reading<CompressorDelta>;
  compressor_thrash: boolean;
  processes: Reading<number>;
  unreadable_processes: number;
  claude: Reading<SessionCount>;
  codex: Reading<SessionCount>;
  launched_footprint: Reading<number>;
  observed_footprint: Reading<number>;
  largest_foreign: Reading<number>;
  launched_write_rate: Reading<number>;
  max_tree_write_rate: Reading<number>;
  memguard_present: boolean;
  memguard_warn: boolean;
  memguard_pid: number;
  orphans: number;
  tick: TickStats;
  /**
   * Bytes of free disk lost per minute, as the writer measured it (the quick
   * rate, which Elevated decides on). Optional only for a record an older build
   * wrote: this backend always emits it, and says why when there is no value.
   */
  disk_fill_rate?: Reading<number>;
  /** The sustained rate, which Severe decides on: one burst cannot fake it. */
  disk_fill_sustained?: Reading<number>;
  /**
   * The app's held governor verdict, for readers with no history (the hook,
   * `gitpulse-mcp`). One tick behind the readings: the sampler publishes, then
   * counts the sample. Pending on a writer's first tick; unsupported in a
   * record an older build wrote.
   */
  verdict: Reading<PublishedVerdict>;
}

/** `sysload::PublishedVerdict`. */
export interface PublishedVerdict {
  tier: Tier;
  binding: Binding | null;
  admit: boolean;
  shed_background: boolean;
  shed_reactive: boolean;
}

export interface AgentSession {
  kind: AgentKind;
  pid: number;
  start_us: number;
  cwd: string | null;
  tree: string | null;
  run_id: string | null;
  /**
   * The nearest agent session above this one in the process tree. Null is "no
   * session above it that the scan could see", not "started by a person".
   */
  parent_pid: number | null;
  /** CPU nanoseconds per second since the previous tick; a full core is 1e9. */
  cpu_rate: Reading<number>;
  footprint: Reading<number>;
  /**
   * Bytes per second the session's own process and its attached descendants
   * wrote since the previous tick. A floor, never the whole: a process that
   * started and exited between two ticks is never read. It names who wrote,
   * not which folder the bytes went to.
   */
  write_rate: Reading<number>;
  members: number;
  unreadable_members: number;
}

export interface LaunchedTree {
  tree: string;
  run_id: string | null;
  session_id: string | null;
  members: number;
  unreadable_members: number;
  footprint: Reading<number>;
  write_rate: Reading<number>;
  cpu_rate: Reading<number>;
}

export interface ForeignProcess {
  pid: number;
  name: string;
  footprint: number;
  session: AgentKind | null;
}

export interface Orphan {
  pid: number;
  start_us: number;
  name: string;
  exec: string | null;
  cwd: string;
  checkout: string;
  age_s: number;
  cpu_ratio: number;
  footprint: Reading<number>;
}

export interface MemguardHealth {
  present: boolean;
  pid: number | null;
  start_us: number | null;
  exec: string | null;
  flags: string[];
  log_path: Reading<string>;
  heartbeat_age_s: Reading<number>;
  heartbeat_interval_s: number | null;
  warn: boolean;
  warnings: string[];
}

export interface ProcView {
  ok: boolean;
  error: string | null;
  processes: number;
  other_users: number;
  unreadable: number;
  claude: SessionCount;
  codex: SessionCount;
  /** Antigravity, DevCouncil and Manvi sessions, so the counts add up to `sessions`. */
  other: SessionCount;
  sessions: AgentSession[];
  sessions_truncated: boolean;
  trees: LaunchedTree[];
  launched_footprint: Reading<number>;
  observed_footprint: Reading<number>;
  foreign_top: ForeignProcess[];
  largest_foreign: Reading<number>;
  launched_write_rate: Reading<number>;
  max_tree_write_rate: Reading<number>;
  memguard: MemguardHealth;
  orphans: Orphan[];
}

/** What the governor says is binding (`sysload::Binding`). */
export type Binding =
  | "memory_pressure"
  | "compressor_thrash"
  | "compressor_segments"
  | "swap"
  | "low_disk"
  | "disk_filling"
  | "thermal"
  | "cpu_load"
  | "exec_latency"
  | "battery";
export type Tier = "clear" | "elevated" | "severe";
export type Reason =
  | "healthy"
  | "elevated"
  | "pressure"
  | "launch_gate_off"
  | "sensor_unavailable"
  | "switched_off"
  | "not_governed";

/** Which of the spawn gate's classes are shed; a user action never is. */
export interface Shed {
  background: boolean;
  reactive: boolean;
  /** What put the host in the tier that sheds; null when nothing is shed. */
  cause: Binding | null;
}

export interface GovernorVerdict {
  admit: boolean;
  shed: Shed;
  reason: Reason;
  tier: Tier;
  binding: Binding | null;
  sensor: SensorState;
  age_ms: number | null;
  message: string;
  warnings: string[];
}

/** `sysload::LowDiskCause`: what bound the cache copy. */
export type LowDiskCause = "below_floor" | "byte_cap" | "headroom_reserved";

/** `sysload::LowDisk`: why caches were not copied for want of disk. */
export interface LowDisk {
  cause: LowDiskCause;
  free_bytes: number | null;
  floor_bytes: number;
  cap_bytes: number | null;
  skipped: string[];
}

/**
 * `sysload::Advisory`: what the latest worktree build advised. Advice only:
 * it never refuses a launch, a checkout or a worktree.
 */
export type Advisory =
  | {
      reason: "low_disk";
      cause: LowDiskCause;
      free_bytes: number | null;
      floor_bytes: number;
      cap_bytes: number | null;
      skipped: string[];
    }
  | { reason: "cache_error"; message: string }
  | { reason: "disk_unknown" }
  | { reason: "several"; items: Advisory[] };

/** `sysload::DiskFill`: where the disk is heading. */
export interface DiskFill {
  /** Bytes of free space lost per minute. */
  rate_bytes_per_min: number;
  /** Minutes until free space reaches the severe floor at that rate; 0 when it is there. */
  eta_min: number;
  free_bytes: number;
  floor_bytes: number;
  /** The tier the projection alone reaches, before the rule's mode caps it. */
  tier: Tier;
}

/**
 * `sysload::ExecLatency`: how long a trivial child takes to start, measured by
 * the app's own probe. The median of the last `n` probes, and whether the
 * latch (slow from 500 ms, clear below 300 ms) calls that slow.
 */
export interface ExecLatency {
  median_ms: number;
  n: number;
  slow: boolean;
}

/** `sysload::GovernorReport`: the verdict, how long refusal has run, a live override. */
export interface GovernorReport {
  verdict: GovernorVerdict;
  /**
   * The start-up time the governor last read, beside the memory pressure it
   * reports. Absent or null when no probe runs in the process or it could not
   * run: unknown, never fast.
   */
  exec_latency?: ExecLatency | null;
  /** Present while the fill-rate rule is on and the projection is at least elevated. */
  disk_fill?: DiskFill | null;
  refusing_for_s: number | null;
  notice: boolean;
  largest_foreign: number | null;
  override_remaining_s: number | null;
  advisory?: Advisory | null;
}

/** `sysload::GovernorPulse`: the part of the report each tick carries. */
export interface GovernorPulse {
  admit: boolean;
  shed_background: boolean;
  shed_reactive: boolean;
  binding: Binding | null;
  refusing_for_s: number | null;
  notice: boolean;
  overridden: boolean;
  largest_foreign: number | null;
}

/** `sysload::ProfileTier`: how much headroom the machine has. */
export type ProfileTier = "compact" | "standard" | "roomy";
/** `host_load_profile`: `auto` derives the tier from memory, the others pin it. */
export type ProfileChoice = "auto" | ProfileTier;
/**
 * `sysload::ProfileBasis`: how the tier was decided. `memory_unreadable` is a
 * default (`standard`), not a derived answer.
 */
export type ProfileBasis = "memory" | "override" | "memory_unreadable";

/** `sysload::ResourceProfile`: the capacity budgets this machine allows. */
export interface ResourceProfile {
  tier: ProfileTier;
  basis: ProfileBasis;
  memory_bytes: number | null;
  logical_cores: number | null;
  budgets: ResourceBudgets;
}

/** The error code a refused agent launch carries (`sysload::REFUSAL_CODE`). */
export const HOST_PRESSURE_REFUSAL = "host_pressure";

/** `sysload::TraceRecord`: one sample of the pre-crash ring; null is unreadable, never zero. */
export interface TraceRecord {
  wall_s: number;
  mono_ms: number;
  boot_id: number;
  pressure: Level | null;
  tier: Tier | null;
  binding: Binding | null;
  admit: boolean | null;
  swap_used: number | null;
  compressed_bytes: number | null;
  load_1m: number | null;
  free_disk: number | null;
  /** Bytes of free space lost per minute, or null when it was not measured. */
  disk_fill_rate?: number | null;
  /** The sustained rate Severe decides on, or null when it was not measured. */
  disk_fill_sustained?: number | null;
  launched_sessions: number | null;
  observed_sessions: number | null;
  largest_foreign: number | null;
  launched_footprint: number | null;
  thermal: Thermal | null;
  power: Power | null;
  segment_fraction: number | null;
  /** Readings lost while the trace write was blocked or failing, since the previous record that landed. */
  dropped_before: number;
  /** The same count since the trace feed started. */
  dropped_total: number;
}

/** `sysload::TracePeaks`: the worst of each reading over a window; null when none could be read. */
export interface TracePeaks {
  records: number;
  max_tier: Tier | null;
  refused: boolean;
  max_swap_used: number | null;
  max_compressed_bytes: number | null;
  max_load_1m: number | null;
  min_free_disk: number | null;
  max_launched_footprint: number | null;
  max_largest_foreign: number | null;
  max_segment_fraction: number | null;
  worst_thermal: Thermal | null;
  max_pressure: Level | null;
}

/** `sysload::PreviousBoot`: what the last boot's trace ended with. */
export interface PreviousBoot {
  ended_wall_s: number | null;
  records: TraceRecord[];
  peaks: TracePeaks;
  /** Readings dropped while a trace write was blocked, summed over every record the ring still holds. */
  dropped: number;
  error: string | null;
}

export interface HostLoadStatus {
  platform: Platform;
  role: Role;
  sensor: SensorReport;
  pressure: PressureInput;
  governor: GovernorReport;
  profile: ResourceProfile;
  snapshot_path: string | null;
  writer: WriterIdentity | null;
  writer_is_self: boolean;
  snapshot: Snapshot | null;
  procs: ProcView | null;
  tick: TickStats;
  problems: string[];
  /** The tail of the previous boot's trace; null when there is none. */
  previous_boot: PreviousBoot | null;
}

export interface HostLoadPulse {
  state: SensorState;
  pressure: PressureInput;
  level: Level | null;
  age_ms: number | null;
  enabled: boolean;
  compressor_thrash: boolean;
  role: Role;
  governor: GovernorPulse;
  /**
   * Host room from the last snapshot (`sysload::HostLoadPulse`). Optional and
   * nullable: an older backend or a fixture that leaves them out reads as
   * unknown, never as mains power or a quiet machine.
   */
  power?: Power | null;
  thermal?: Thermal | null;
  /** One-minute load average over the logical core count. */
  load_per_core?: number | null;
  cpu_pct?: number | null;
  gpu_pct?: number | null;
  mem_used_bytes?: number | null;
  mem_total_bytes?: number | null;
  swap_used_bytes?: number | null;
  thermal_temp_c?: number | null;
  disk_free_bytes?: number | null;
  disk_total_bytes?: number | null;
  disk_write_rate?: number | null;
  disk_fill_rate?: number | null;
  snapshot_seq?: number;
}

/** Mirrors `sysload::room_of`: each reading only when it was read. */
export function hostRoomOf(host: HostSample | undefined): Pick<HostLoadPulse, "power" | "thermal" | "load_per_core"> {
  if (!host) return { power: null, thermal: null, load_per_core: null };
  const load = host.load.status === "value" ? host.load.value[0] : null;
  const logical = host.cores.status === "value" ? host.cores.value.logical : 0;
  return {
    power: host.power.status === "value" ? host.power.value : null,
    thermal: host.thermal.status === "value" ? host.thermal.value : null,
    load_per_core:
      load !== null && Number.isFinite(load) && load >= 0 && logical > 0 ? Math.round((load / logical) * 100) / 100 : null,
  };
}

export type HostPulseMetrics = Required<
  Pick<
    HostLoadPulse,
    | "cpu_pct"
    | "gpu_pct"
    | "mem_used_bytes"
    | "mem_total_bytes"
    | "swap_used_bytes"
    | "thermal_temp_c"
    | "disk_free_bytes"
    | "disk_total_bytes"
    | "disk_write_rate"
    | "disk_fill_rate"
  >
>;

/** Extracts live CPU/GPU/Memory/Thermal/Disk pulse metrics from a snapshot. */
export function hostMetricsOf(snapshot: Snapshot | null | undefined): HostPulseMetrics {
  if (!snapshot || snapshot.enabled === false) {
    return {
      cpu_pct: null,
      gpu_pct: null,
      mem_used_bytes: null,
      mem_total_bytes: null,
      swap_used_bytes: null,
      thermal_temp_c: null,
      disk_free_bytes: null,
      disk_total_bytes: null,
      disk_write_rate: null,
      disk_fill_rate: null,
    };
  }
  const h = snapshot.host;
  const cpuPct =
    h.cpu_usage?.status === "value" && Number.isFinite(h.cpu_usage.value.total_pct) && h.cpu_usage.value.total_pct >= 0
      ? h.cpu_usage.value.total_pct
      : null;
  const gpuPct =
    h.gpu?.status === "value" && Number.isFinite(h.gpu.value.utilization_pct) && h.gpu.value.utilization_pct >= 0
      ? h.gpu.value.utilization_pct
      : null;
  const tempC =
    h.thermal_temp_c?.status === "value" && Number.isFinite(h.thermal_temp_c.value)
      ? h.thermal_temp_c.value
      : null;
  return {
    cpu_pct: cpuPct,
    gpu_pct: gpuPct,
    mem_used_bytes: h.memory?.status === "value" ? h.memory.value.used_bytes : null,
    mem_total_bytes: h.memsize.status === "value" ? h.memsize.value : null,
    swap_used_bytes: h.swap_used.status === "value" ? h.swap_used.value : null,
    thermal_temp_c: tempC,
    disk_free_bytes: h.free_disk.status === "value" ? h.free_disk.value : null,
    disk_total_bytes: h.total_disk?.status === "value" ? h.total_disk.value : null,
    disk_write_rate: snapshot.launched_write_rate.status === "value" ? snapshot.launched_write_rate.value : null,
    disk_fill_rate: snapshot.disk_fill_rate?.status === "value" ? snapshot.disk_fill_rate.value : null,
  };
}

/** `sysload::InputMode`: what one input may do to the governor's verdict. */
export type InputMode = "off" | "shed" | "refuse";

/** `sysload::Rule`: a mode and two thresholds (disk is GiB free, lower is worse). */
export interface Rule {
  mode: InputMode;
  elevated_at: number;
  severe_at: number;
}

/** `sysload::ModeRule`: an input whose states are fixed. */
export interface ModeRule {
  mode: InputMode;
}

/** `sysload::HorizonRule`: how soon the disk is projected to reach its severe floor. */
export interface HorizonRule {
  mode: InputMode;
  elevated_within_min: number;
  severe_within_min: number;
}

/** `sysload::HeadroomRule`: the free space a coverage build needs before it starts. */
export interface HeadroomRule {
  mode: InputMode;
  headroom_gib: number;
}

/** `sysload::DiskGuard`: the disk guards beyond the level. */
export interface DiskGuard {
  fill: HorizonRule;
  coverage: HeadroomRule;
}

/** `sysload::Thresholds`: every input the governor decides on besides pressure. */
export interface Thresholds {
  swap: Rule;
  compressor_segments: Rule;
  load: Rule;
  thermal: ModeRule;
  battery: ModeRule;
  /** While `disk_auto`, the numbers are the automatic floors shown, not stored. */
  disk: Rule;
  /** Whether the disk floors follow physical memory (swap shares the free-space pool). */
  disk_auto: boolean;
  disk_guard: DiskGuard;
}

/** What the command gate does with a heavy command while the host is overloaded (`HostLoadHookMode`). */
export type HostLoadHookMode = "enforce" | "advise" | "off";

/** `tool_config::LeaseMode`. */
export type LeaseMode = "on" | "off";

/** `tool_config::HostLoadLeases`: how heavy builds are leased. Stored as `host_load_leases`. */
export interface HostLoadLeases {
  mode: LeaseMode;
  slots: number;
  wait_max_s: number;
  wait_hard_s: number;
  incremental_off: boolean;
}

/** `tool_config::HostLoadIdle`: when a session counts as idle and when Health offers Park. Stored as `host_load_idle`. */
export interface HostLoadIdle {
  idle_after_s: number;
  park_after_s: number;
}

export interface HostLoadSettings {
  enabled: boolean;
  /** Whether severe pressure refuses agent launches (`host_load.govern_launches`). */
  govern_launches: boolean;
  /** Stored as the top-level `host_load_hook_mode`, never inside `host_load`. */
  hook_mode: HostLoadHookMode;
  thresholds: Thresholds;
  leases: HostLoadLeases;
  idle: HostLoadIdle;
  /**
   * Which resource profile sizes GitPulse's caches. Stored as the top-level
   * `host_load_profile`, never inside `host_load`.
   */
  profile: ProfileChoice;
  /** What the memory-pressure supervisor may do. Stored as the top-level `host_load_supervisor`. */
  supervisor: SupervisorMode;
  /**
   * Rung C, opt-in: a paused build still paused at the 10-minute cap is ended
   * instead of resumed. Stored as the top-level `host_load_supervisor_terminate`.
   */
  supervisor_terminate: boolean;
}

/** Off does nothing, Advise only says what it would pause, Act pauses. */
export type SupervisorMode = "off" | "advise" | "act";

/** Mirrors `HostLoadLeases::default` in Rust. */
export const DEFAULT_LEASES: HostLoadLeases = {
  mode: "on",
  slots: 4,
  wait_max_s: 1800,
  wait_hard_s: 14400,
  incremental_off: false,
};

/** Mirrors `HostLoadIdle::default` in Rust. */
export const DEFAULT_IDLE: HostLoadIdle = { idle_after_s: 900, park_after_s: 3600 };

/**
 * Mirrors `Thresholds::default` in Rust. Provisional: each default was set
 * from the 2026-11-14 incident alone (see `sysload/thresholds.rs`).
 */
export const DEFAULT_THRESHOLDS: Thresholds = {
  swap: { mode: "refuse", elevated_at: 0.5, severe_at: 0.75 },
  compressor_segments: { mode: "refuse", elevated_at: 0.6, severe_at: 0.8 },
  load: { mode: "shed", elevated_at: 3, severe_at: 6 },
  thermal: { mode: "shed" },
  battery: { mode: "off" },
  disk: { mode: "refuse", elevated_at: 20, severe_at: 10 },
  disk_auto: true,
  disk_guard: {
    fill: { mode: "refuse", elevated_within_min: 30, severe_within_min: 10 },
    coverage: { mode: "refuse", headroom_gib: 60 },
  },
};

/**
 * Whether the settings equal the defaults, as the backend stores them. The
 * automatic disk floors are shown as numbers for this computer's memory, so
 * while they are automatic only the disk mode is compared, never its numbers.
 */
export function thresholdsAreDefault(thresholds: Thresholds): boolean {
  const base = DEFAULT_THRESHOLDS;
  const disk = (value: Thresholds) =>
    value.disk_auto ? { auto: true, mode: value.disk.mode } : { auto: false, ...value.disk };
  return (
    JSON.stringify({ ...thresholds, disk: disk(thresholds) }) ===
    JSON.stringify({ ...base, disk: disk(base) })
  );
}

/** The event the app's sampler emits after every tick. */
export const HOST_LOAD_PULSE_EVENT = "host-load-pulse";

/** Whether a pulse represents an enabled, governing sensor reading (`fresh` or `stale_recent`). */
export function isLiveHostLoadPulse(pulse: HostLoadPulse): boolean {
  return pulse.enabled && (pulse.state === "fresh" || pulse.state === "stale_recent");
}

function sanitizePulse(pulse: HostLoadPulse): HostLoadPulse {
  if (isLiveHostLoadPulse(pulse)) return pulse;
  return {
    ...pulse,
    pressure: "unavailable",
    compressor_thrash: false,
    power: null,
    thermal: null,
    load_per_core: null,
    cpu_pct: null,
    gpu_pct: null,
    mem_used_bytes: null,
    mem_total_bytes: null,
    swap_used_bytes: null,
    thermal_temp_c: null,
    disk_free_bytes: null,
    disk_total_bytes: null,
    disk_write_rate: null,
    disk_fill_rate: null,
  };
}

/** Mirrors `sysload::pulse_of`, so the first status read stands in for a pulse. */
export function pulseOf(status: HostLoadStatus): HostLoadPulse {
  const enabled = status.sensor.enabled ?? false;
  const live =
    enabled &&
    (status.sensor.state === "fresh" || status.sensor.state === "stale_recent") &&
    status.snapshot?.enabled !== false;
  const liveSnapshot = live ? status.snapshot : null;
  return {
    state: status.sensor.state,
    pressure: live ? status.pressure : "unavailable",
    level: status.sensor.level,
    age_ms: status.sensor.age_ms,
    enabled,
    compressor_thrash: liveSnapshot?.compressor_thrash ?? false,
    role: status.role,
    governor: {
      admit: status.governor.verdict.admit,
      shed_background: status.governor.verdict.shed.background,
      shed_reactive: status.governor.verdict.shed.reactive,
      binding: status.governor.verdict.binding,
      refusing_for_s: status.governor.refusing_for_s,
      notice: status.governor.notice,
      overridden: status.governor.override_remaining_s !== null,
      largest_foreign: status.governor.largest_foreign,
    },
    ...hostRoomOf(liveSnapshot?.host),
    ...hostMetricsOf(liveSnapshot),
    ...(status.snapshot && typeof status.snapshot.seq === "number"
      ? { snapshot_seq: status.snapshot.seq }
      : {}),
  };
}

export const MAX_HOST_LOAD_HISTORY = 60;

const pulseStore = writable<HostLoadPulse | null>(null);
const statusStore = writable<HostLoadStatus | null>(null);
const historyStore = writable<readonly HostLoadPulse[]>([]);
let lastReaderSeq: number | null = null;

function appendHistory(rawPulse: HostLoadPulse, seedOnly = false): void {
  const pulse = sanitizePulse(rawPulse);
  const live = isLiveHostLoadPulse(pulse);
  const seq =
    pulse.role === "reader" && typeof pulse.snapshot_seq === "number"
      ? pulse.snapshot_seq
      : null;

  historyStore.update((items) => {
    // Polls only seed empty history when live, so live sampler events aren't duplicated;
    // when the sensor transitions from live to non-live (e.g. dead sampler stopped emitting),
    // still allow one null-metrics gap entry to break the chart series.
    if (seedOnly && items.length > 0 && live) return items;

    if (live) {
      if (seq !== null && seq === lastReaderSeq) {
        return items;
      }
      if (seq !== null) lastReaderSeq = seq;
      const next = [...items, pulse];
      return next.length > MAX_HOST_LOAD_HISTORY ? next.slice(next.length - MAX_HOST_LOAD_HISTORY) : next;
    }

    // Clear the tracked reader sequence on non-live gaps so a restarted writer
    // whose new sequence happens to match an old seq is not deduplicated.
    lastReaderSeq = null;
    const prev = items[items.length - 1];
    if (prev && !isLiveHostLoadPulse(prev)) {
      return items;
    }
    const next = [...items, pulse];
    return next.length > MAX_HOST_LOAD_HISTORY ? next.slice(next.length - MAX_HOST_LOAD_HISTORY) : next;
  });
}

let listening: Promise<UnlistenFn | null> | null = null;
let noticeShown = false;

/**
 * Tells the person, once per run, that launches have been refused for 30
 * minutes, with the one click that stops it. The refusal itself never lifts
 * on its own: a deny that quietly became an allow would be the failure this
 * notice exists to prevent, so the flip is the person's.
 */
function announceRefusal(pulse: HostLoadPulse): void {
  if (!pulse.governor.notice) {
    noticeShown = false;
    return;
  }
  if (noticeShown) return;
  noticeShown = true;
  toastStore.add({
    kind: "warning",
    message: refusalNotice(pulse.governor),
    duration: 0,
    action: {
      label: "Stop refusing launches",
      onClick: async () => {
        const settings = await loadHostLoadSettings();
        await saveHostLoadSettings({ ...settings, govern_launches: false });
      },
    },
  });
}

/**
 * Starts listening for the sampler's pulse, once per page, and reads the
 * status so the first value does not wait for a tick. Safe to call from
 * every component that needs the pressure.
 *
 * A refused listen caches nothing, so the next call tries again: a rejected
 * promise kept as "already listening" would leave the bar without pulses for
 * the rest of the run while looking attached.
 */
export function startHostLoad(): void {
  if (listening || !isTauri()) return;
  const attempt: Promise<UnlistenFn | null> = listen<HostLoadPulse>(HOST_LOAD_PULSE_EVENT, (event) => {
    const pulse = sanitizePulse(event.payload);
    pulseStore.set(pulse);
    appendHistory(pulse);
    announceRefusal(pulse);
    rereadProfileIfUnsettled();
  }).catch((error) => {
    if (listening === attempt) listening = null;
    diagnostics.warn("hostLoad", `couldn't listen for the host-load pulse; will retry: ${String(error)}`);
    return null;
  });
  listening = attempt;
  profileRetryNotBefore = Date.now() + PROFILE_RETRY_BASE_MS;
  void refreshHostLoad().catch(() => undefined);
}

/** First wait before the profile is reread; each further wait doubles, up to the cap. */
export const PROFILE_RETRY_BASE_MS = 2_000;
export const PROFILE_RETRY_CAP_MS = 60_000;

let profileRetries = 0;
let profileRetryNotBefore = 0;

/**
 * The pulse does not carry the resource profile, so a start-up read that failed,
 * or that said the computer's memory could not be read, would leave every
 * consumer on a guessed tier until a restart. While the profile is unsettled
 * (no status yet, or `memory_unreadable`) a pulse rereads it, but on a doubling
 * wait that levels off at `PROFILE_RETRY_CAP_MS`: at most one read a minute for
 * a read that never succeeds, never one per two-second tick, and never a stop
 * that would strand the tier. A change of setting starts the waits over.
 */
function rereadProfileIfUnsettled(): void {
  const held = get(statusStore);
  if (held !== null && held.profile.basis !== "memory_unreadable") return;
  const now = Date.now();
  if (now < profileRetryNotBefore) return;
  profileRetryNotBefore = now + Math.min(PROFILE_RETRY_CAP_MS, PROFILE_RETRY_BASE_MS * 2 ** profileRetries);
  profileRetries += 1;
  void refreshHostLoad().catch(() => undefined);
}

/** The one status read in flight; a caller that arrives during it shares it. */
let refreshing: Promise<HostLoadStatus | null> | null = null;

/**
 * Reads the full status (Health); the pulse follows it. Single-flight: Health
 * asks every few seconds, and a slow read must not stack another behind
 * itself, so a call made while one is running gets that one's answer.
 */
export function refreshHostLoad(): Promise<HostLoadStatus | null> {
  if (!isTauri()) return Promise.resolve(null);
  if (refreshing) return refreshing;
  const flight: Promise<HostLoadStatus | null> = (async () => {
    const status = await invoke<HostLoadStatus>("cmd_host_load_status");
    statusStore.set(status);
    const pulse = pulseOf(status);
    pulseStore.set(pulse);
    appendHistory(pulse, true);
    return status;
  })().finally(() => {
    if (refreshing === flight) refreshing = null;
  });
  refreshing = flight;
  return flight;
}

/**
 * Rereads after a change this window made. A read already running began
 * before the change and may not show it, so this waits for that one and then
 * reads again instead of joining it.
 */
function refreshAfterChange(): void {
  profileRetries = 0;
  profileRetryNotBefore = 0;
  void (refreshing ?? Promise.resolve())
    .catch(() => undefined)
    .then(() => refreshHostLoad())
    .catch(() => undefined);
}

/** Terminates a foreign orphan Health listed; the backend re-checks it first. */
export async function terminateOrphan(pid: number, startUs: number): Promise<string> {
  const message = await invoke<string>("cmd_host_load_terminate_orphan", {
    pid,
    startUs,
  });
  refreshAfterChange();
  return message;
}

/**
 * A person's "Admit anyway" on a refused agent launch. The backend asks in a
 * native dialog that page script cannot answer, then admits launches for two
 * minutes; a No rejects, so a caller must not retry on a rejection.
 */
export async function admitAnyway(): Promise<string> {
  const message = await invoke<string>("cmd_host_load_admit_anyway");
  refreshAfterChange();
  return message;
}

/** `sysload::lease::Record`: what a slot or a ticket says about its build. */
/** `sysload::lease::Priority`: terminals first, then task runs, then sessions only observed. */
export type Priority = "terminal" | "run" | "observed";

export interface LeaseRecord {
  pid: number;
  start_us: number;
  session_key: string;
  priority: Priority;
  terminal_session: string | null;
  run_id: string | null;
  cwd: string;
  label: string;
  enqueued_mono_ns: number;
  granted_mono_ns: number | null;
  tier_at_grant: Tier | null;
  verdict_unavailable_reason: string | null;
  overflow: boolean;
}

export interface LeaseHolder {
  slot: number;
  pid: number;
  /** Null is a lock with no readable record of whose it is. */
  record: LeaseRecord | null;
}

export interface LeaseWaiter {
  name: string;
  record: LeaseRecord;
}

/** `sysload::lease::Capacity`. */
export interface LeaseCapacity {
  configured: number;
  slots: number;
  refuses: boolean;
  tier: Tier | null;
  note: string | null;
}

/** `sysload::lease::Status`: every field that could not be read says why instead of reading zero. */
export interface LeaseStatus {
  on: boolean;
  configured: number;
  capacity: LeaseCapacity;
  holders: LeaseHolder[];
  queue: LeaseWaiter[];
  unreadable: string[];
  error: string | null;
  links_missing: string | null;
  misses: number | null;
  /** The marker directory held more than one read looks at: `misses` is a floor. */
  misses_truncated: boolean;
  misses_error: string | null;
  unleased_running: number | null;
  unleased_error: string | null;
  now_mono_ns: number;
}

/**
 * Reads the build leases; never writes. `queueOnly` skips the process-table
 * read and the miss count (what the terminal pill needs is the queue), and
 * the fields it skipped say so instead of reading zero.
 */
export function loadLeaseStatus(queueOnly = false): Promise<LeaseStatus | null> {
  if (!isTauri()) return Promise.resolve(null);
  return invoke<LeaseStatus>("cmd_host_load_leases", { queueOnly });
}

/** `sysload::activity::SessionActivity`. */
export type SessionActivity =
  | { state: "busy"; why: string }
  | { state: "idle"; since_ms: number; why: string }
  | { state: "unknown"; why: string };

export interface IdleSession {
  terminal_session: string | null;
  kind: AgentKind;
  pid: number;
  start_us: number;
  cwd: string | null;
  footprint: Reading<number>;
  activity: SessionActivity;
  /** Why it cannot be parked, or null when it can. */
  park_blocked: string | null;
}

/** `sysload::activity::Report`. */
export interface IdleReport {
  sessions: IdleSession[];
  idle: number;
  unknown: number;
  idle_after_s: number;
  park_after_s: number;
  error: string | null;
}

export function loadIdleReport(): Promise<IdleReport | null> {
  if (!isTauri()) return Promise.resolve(null);
  return invoke<IdleReport>("cmd_host_load_idle");
}

/**
 * A person's "Park" on one idle session. The backend checks it again, asks in a
 * native dialog that page script cannot answer, and checks once more; a No or
 * a changed session rejects, so a caller must not retry on a rejection.
 */
export async function parkSession(terminalSession: string): Promise<string> {
  const message = await invoke<string>("cmd_host_load_park", { sessionId: terminalSession });
  refreshAfterChange();
  return message;
}

/** Mirrors `HostLoadSettings::default` in Rust: on, profile derived. */
export const DEFAULT_HOST_LOAD_SETTINGS: HostLoadSettings = {
  enabled: true,
  govern_launches: true,
  hook_mode: "enforce",
  thresholds: DEFAULT_THRESHOLDS,
  leases: DEFAULT_LEASES,
  idle: DEFAULT_IDLE,
  profile: "auto",
  supervisor: "act",
  supervisor_terminate: false,
};

/**
 * The settings as last read or saved, shared by every Settings row that edits
 * them. Each row saves through {@link updateHostLoadSettings}, which merges
 * onto this and not onto a component's older copy, so a toggle never saves
 * stale thresholds over a row's edit. Null until a read has succeeded.
 */
const settingsStore = writable<HostLoadSettings | null>(null);
export const hostLoadSettings: Readable<HostLoadSettings | null> = { subscribe: settingsStore.subscribe };

export async function loadHostLoadSettings(): Promise<HostLoadSettings> {
  if (!isTauri()) return DEFAULT_HOST_LOAD_SETTINGS;
  const settings = await invoke<HostLoadSettings>("cmd_host_load_settings");
  settingsStore.set(settings);
  return settings;
}

export async function saveHostLoadSettings(settings: HostLoadSettings): Promise<HostLoadSettings> {
  if (!isTauri()) {
    settingsStore.set(settings);
    return settings;
  }
  const saved = await invoke<HostLoadSettings>("cmd_host_load_settings_save", { settings });
  settingsStore.set(saved);
  // The profile is part of the status: reread it so a change of tier reaches
  // every consumer now, not on a restart.
  refreshAfterChange();
  return saved;
}

let loadingSettings: Promise<HostLoadSettings> | null = null;

/**
 * The settings, read once however many Settings rows ask at the same time.
 * A row that mounts after a read has succeeded gets the stored copy.
 */
export function ensureHostLoadSettings(): Promise<HostLoadSettings> {
  let have: HostLoadSettings | null = null;
  settingsStore.subscribe((value) => (have = value))();
  if (have) return Promise.resolve(have);
  if (!loadingSettings) {
    const flight = loadHostLoadSettings().finally(() => {
      if (loadingSettings === flight) loadingSettings = null;
    });
    loadingSettings = flight;
  }
  return loadingSettings;
}

/** Saves `change` on top of the latest settings read or saved anywhere. */
export function updateHostLoadSettings(change: Partial<HostLoadSettings>): Promise<HostLoadSettings> {
  let latest: HostLoadSettings = DEFAULT_HOST_LOAD_SETTINGS;
  settingsStore.subscribe((value) => {
    if (value) latest = value;
  })();
  return saveHostLoadSettings({ ...latest, ...change });
}

export const hostLoadPulse: Readable<HostLoadPulse | null> = { subscribe: pulseStore.subscribe };
export const hostLoadStatus: Readable<HostLoadStatus | null> = {
  subscribe: statusStore.subscribe,
};
export const hostLoadHistory: Readable<readonly HostLoadPulse[]> = {
  subscribe: historyStore.subscribe,
};

/**
 * The resource profile the backend derived, or null until the first status
 * arrives. A consumer sizing itself from it uses `COMPACT_BUDGETS` while it
 * is null, never a roomy default (`resourceBudgets.ts`).
 */
export const resourceProfile: Readable<ResourceProfile | null> = derived(
  statusStore,
  (status) => status?.profile ?? null,
);

/**
 * The memory-pressure input for features that defer background work under
 * load (card 1c): a level only while the sensor may be acted on.
 */
export const hostPressure: Readable<PressureInput> = derived(
  pulseStore,
  (pulse) => pulse?.pressure ?? "unavailable",
);

/**
 * Test seam: publishes `pulse` as if the sampler had just ticked, for a page
 * with no backend (the browser harness) to drive what reads the pulse.
 */
export function publishHostLoadPulseForTests(pulse: HostLoadPulse | null): void {
  const sanitized = pulse === null ? null : sanitizePulse({ ...pulse });
  pulseStore.set(sanitized);
  if (sanitized !== null) appendHistory(sanitized);
}

/** Test seam: resets the module state between tests. */
export function resetHostLoadForTests(): void {
  listening = null;
  refreshing = null;
  noticeShown = false;
  lastReaderSeq = null;
  profileRetries = 0;
  profileRetryNotBefore = 0;
  pulseStore.set(null);
  statusStore.set(null);
  historyStore.set([]);
  settingsStore.set(null);
}
