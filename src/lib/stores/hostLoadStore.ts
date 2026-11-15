import { derived, writable, type Readable } from "svelte/store";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";
import { refusalNotice } from "../health/hostLoad";
import { toastStore } from "./toastStore";
import { diagnostics } from "../diagnostics/diagnostics";

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
  | "thermal"
  | "cpu_load"
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

/** `sysload::Thresholds`: every input the governor decides on besides pressure. */
export interface Thresholds {
  swap: Rule;
  compressor_segments: Rule;
  load: Rule;
  thermal: ModeRule;
  battery: ModeRule;
  disk: Rule;
}

export interface HostLoadSettings {
  enabled: boolean;
  /** Whether severe pressure refuses agent launches (`host_load.govern_launches`). */
  govern_launches: boolean;
  thresholds: Thresholds;
}

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
};

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
 *
 * A refused listen caches nothing, so the next call tries again: a rejected
 * promise kept as "already listening" would leave the bar without pulses for
 * the rest of the run while looking attached.
 */
export function startHostLoad(): void {
  if (listening || !isTauri()) return;
  const attempt: Promise<UnlistenFn | null> = listen<HostLoadPulse>(HOST_LOAD_PULSE_EVENT, (event) => {
    pulseStore.set(event.payload);
    announceRefusal(event.payload);
  }).catch((error) => {
    if (listening === attempt) listening = null;
    diagnostics.warn("hostLoad", `couldn't listen for the host-load pulse; will retry: ${String(error)}`);
    return null;
  });
  listening = attempt;
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
    pulseStore.set(pulseOf(status));
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

/** Mirrors `HostLoadSettings::default` in Rust: on. */
export const DEFAULT_HOST_LOAD_SETTINGS: HostLoadSettings = {
  enabled: true,
  govern_launches: true,
  thresholds: DEFAULT_THRESHOLDS,
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
  refreshing = null;
  noticeShown = false;
  pulseStore.set(null);
  statusStore.set(null);
  settingsStore.set(null);
}
