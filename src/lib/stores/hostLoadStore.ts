import { derived, writable, type Readable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";

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

export type Why = "unsupported" | "unreadable" | "failed" | "not_yet";
/** `sysload::Reading<T>`: a value, or why there is none. */
export type Reading<T> = { status: "value"; value: T } | { status: "unavailable"; value: Why };
export type Level = "normal" | "warn" | "critical";
export type SensorState = "fresh" | "stale_recent" | "stale_old" | "dead";
export type PressureInput = Level | "unavailable";
export type Role = "writer" | "reader" | "not_sampling" | "unsupported";
export type Platform = "governed" | "advise" | "unavailable";
export type Thermal = "nominal" | "fair" | "serious" | "critical";
export type Power = "ac" | "battery";
export type AgentKind = "claude" | "codex";

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

export interface Cores {
  logical: number;
  levels: number;
  perf0: number;
  perf1: number;
}

export interface HostSample {
  pressure: Reading<Level>;
  swap_used: Reading<number>;
  swap_total: Reading<number>;
  compressor: Reading<Compressor>;
  compressed_bytes: Reading<number>;
  load: Reading<[number, number, number]>;
  cores: Reading<Cores>;
  memsize: Reading<number>;
  open_files: Reading<number>;
  max_files: Reading<number>;
  free_disk: Reading<number>;
  thermal: Reading<Thermal>;
  power: Reading<Power>;
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
}

export interface AgentSession {
  kind: AgentKind;
  pid: number;
  start_us: number;
  cwd: string | null;
  tree: string | null;
  run_id: string | null;
  footprint: Reading<number>;
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

export interface HostLoadStatus {
  platform: Platform;
  role: Role;
  sensor: SensorReport;
  pressure: PressureInput;
  snapshot_path: string | null;
  writer: WriterIdentity | null;
  writer_is_self: boolean;
  snapshot: Snapshot | null;
  procs: ProcView | null;
  tick: TickStats;
  problems: string[];
}

export interface HostLoadPulse {
  state: SensorState;
  pressure: PressureInput;
  level: Level | null;
  age_ms: number | null;
  enabled: boolean;
  compressor_thrash: boolean;
  role: Role;
}

export interface HostLoadSettings {
  enabled: boolean;
}

/** The event the app's sampler emits after every tick. */
export const HOST_LOAD_PULSE_EVENT = "host-load-pulse";

/** Mirrors `sysload::pulse_of`, so the first status read stands in for a pulse. */
export function pulseOf(status: HostLoadStatus): HostLoadPulse {
  return {
    state: status.sensor.state,
    pressure: status.pressure,
    level: status.sensor.level,
    age_ms: status.sensor.age_ms,
    enabled: status.sensor.enabled ?? false,
    compressor_thrash: status.snapshot?.compressor_thrash ?? false,
    role: status.role,
  };
}

const pulseStore = writable<HostLoadPulse | null>(null);
const statusStore = writable<HostLoadStatus | null>(null);

let listening: Promise<UnlistenFn | null> | null = null;

/**
 * Starts listening for the sampler's pulse, once per page, and reads the
 * status so the first value does not wait for a tick. Safe to call from
 * every component that needs the pressure.
 */
export function startHostLoad(): void {
  if (listening || !isTauri()) return;
  listening = listen<HostLoadPulse>(HOST_LOAD_PULSE_EVENT, (event) => {
    pulseStore.set(event.payload);
  }).catch(() => null);
  void refreshHostLoad().catch(() => undefined);
}

/** Reads the full status (Health); the pulse follows it. */
export async function refreshHostLoad(): Promise<HostLoadStatus | null> {
  if (!isTauri()) return null;
  const status = await invoke<HostLoadStatus>("cmd_host_load_status");
  statusStore.set(status);
  pulseStore.set(pulseOf(status));
  return status;
}

/** Terminates a foreign orphan Health listed; the backend re-checks it first. */
export async function terminateOrphan(pid: number, startUs: number): Promise<string> {
  const message = await invoke<string>("cmd_host_load_terminate_orphan", {
    pid,
    startUs,
  });
  void refreshHostLoad().catch(() => undefined);
  return message;
}

/** Mirrors `HostLoadSettings::default` in Rust: on. */
export const DEFAULT_HOST_LOAD_SETTINGS: HostLoadSettings = { enabled: true };

export async function loadHostLoadSettings(): Promise<HostLoadSettings> {
  if (!isTauri()) return DEFAULT_HOST_LOAD_SETTINGS;
  return invoke<HostLoadSettings>("cmd_host_load_settings");
}

export async function saveHostLoadSettings(settings: HostLoadSettings): Promise<HostLoadSettings> {
  if (!isTauri()) return settings;
  return invoke<HostLoadSettings>("cmd_host_load_settings_save", { settings });
}

export const hostLoadPulse: Readable<HostLoadPulse | null> = { subscribe: pulseStore.subscribe };
export const hostLoadStatus: Readable<HostLoadStatus | null> = {
  subscribe: statusStore.subscribe,
};

/**
 * The memory-pressure input for features that defer background work under
 * load (card 1c): a level only while the sensor may be acted on.
 */
export const hostPressure: Readable<PressureInput> = derived(
  pulseStore,
  (pulse) => pulse?.pressure ?? "unavailable",
);

/** Test seam: resets the module state between tests. */
export function resetHostLoadForTests(): void {
  listening = null;
  pulseStore.set(null);
  statusStore.set(null);
}
