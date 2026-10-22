import { derived, writable, type Readable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { refusalNotice } from "../health/hostLoad";
import { toastStore } from "./toastStore";

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

/** What the governor says is binding (`sysload::Binding`). */
export type Binding = "memory_pressure" | "compressor_thrash";
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

/** `sysload::GovernorReport`: the verdict, how long refusal has run, a live override. */
export interface GovernorReport {
  verdict: GovernorVerdict;
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

/** The error code a refused agent launch carries (`sysload::REFUSAL_CODE`). */
export const HOST_PRESSURE_REFUSAL = "host_pressure";

export interface HostLoadStatus {
  platform: Platform;
  role: Role;
  sensor: SensorReport;
  pressure: PressureInput;
  governor: GovernorReport;
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
  governor: GovernorPulse;
}

export interface HostLoadSettings {
  enabled: boolean;
  /** Whether severe pressure refuses agent launches (`host_load.govern_launches`). */
  govern_launches: boolean;
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
  };
}

const pulseStore = writable<HostLoadPulse | null>(null);
const statusStore = writable<HostLoadStatus | null>(null);

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
 */
export function startHostLoad(): void {
  if (listening || !isTauri()) return;
  listening = listen<HostLoadPulse>(HOST_LOAD_PULSE_EVENT, (event) => {
    pulseStore.set(event.payload);
    announceRefusal(event.payload);
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

/**
 * A person's "Admit anyway" on a refused agent launch. The backend asks in a
 * native dialog that page script cannot answer, then admits launches for two
 * minutes; a No rejects, so a caller must not retry on a rejection.
 */
export async function admitAnyway(): Promise<string> {
  const message = await invoke<string>("cmd_host_load_admit_anyway");
  void refreshHostLoad().catch(() => undefined);
  return message;
}

/** Mirrors `HostLoadSettings::default` in Rust: on. */
export const DEFAULT_HOST_LOAD_SETTINGS: HostLoadSettings = {
  enabled: true,
  govern_launches: true,
};

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
  noticeShown = false;
  pulseStore.set(null);
  statusStore.set(null);
}
