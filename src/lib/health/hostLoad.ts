/**
 * Wording for host load: what the status bar and the Health section say
 * about the sampler's readings (`stores/hostLoadStore.ts`).
 *
 * Kept out of the components so the decisions — when the status bar speaks
 * up, how an unreadable reading reads — are tested once and said the same
 * way in both places. The rule they share: a reading the OS refused says so,
 * and is never shown as zero.
 */
import { humanBytes } from "../storage/format";
import { NO_RELIEF_CHIP, type ReliefChipInput } from "./hostRelief";
import type {
  AgentKind,
  AgentSession,
  Binding,
  DiskFill,
  GovernorPulse,
  Advisory,
  GovernorReport,
  HostLoadPulse,
  Level,
  MemguardHealth,
  PreviousBoot,
  ProfileChoice,
  Reading,
  ResourceProfile,
  TraceRecord,
  SensorState,
  Snapshot,
  Why,
} from "../stores/hostLoadStore";

/** The value of a reading, or `null` for any reason there is none. */
export function valueOf<T>(reading: Reading<T> | null | undefined): T | null {
  return reading && reading.status === "value" ? reading.value : null;
}

const WHY_LABEL: Record<Why, string> = {
  unsupported: "not measured here",
  unreadable: "unreadable",
  failed: "couldn't read",
  not_yet: "pending",
  disabled: "switched off",
  older: "not published by this writer",
};

/** A reading in words: its value formatted, or why there is none. */
export function describeReading<T>(
  reading: Reading<T> | null | undefined,
  format: (value: T) => string,
): string {
  if (!reading) return WHY_LABEL.failed;
  return reading.status === "value" ? format(reading.value) : WHY_LABEL[reading.value];
}

export const bytes = (value: number): string => humanBytes(value);
export const bytesPerSecond = (value: number): string => `${humanBytes(value)}/s`;

/** "12 s", "4 min", "2 h". */
export function formatAge(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 90) return `${s} s`;
  const min = Math.round(s / 60);
  if (min < 90) return `${min} min`;
  return `${Math.round(min / 60)} h`;
}

/** The sensor state as a reader should take it. */
export function sensorStateLabel(state: SensorState, ageMs: number | null): string {
  switch (state) {
    case "fresh":
      return "live";
    case "stale_recent":
    case "stale_old":
      return ageMs === null ? "stale" : `stale ${formatAge(ageMs)}`;
    case "dead":
      return "no sampler";
  }
}

const LEVEL_LABEL: Record<Level, string> = {
  normal: "normal",
  warn: "warning",
  critical: "critical",
};

export function levelLabel(level: Level | null): string {
  return level ? LEVEL_LABEL[level] : "unknown";
}

export type ChipTone = "warn" | "danger" | "muted";

export interface StatusChip {
  text: string;
  tone: ChipTone;
  title: string;
}

/**
 * What the status bar says about host load, or `null` to say nothing.
 *
 * Quiet while the machine is fine and the sampler is live — the bar is not
 * where "all well" belongs. It speaks when memory pressure is up, when the
 * compressor is thrashing, or when the sensor stopped on a platform where it
 * is meant to run. A sensor the user switched off is their choice, and
 * Settings already says so.
 */
export function statusChip(
  pulse: HostLoadPulse | null,
  relief: ReliefChipInput = NO_RELIEF_CHIP,
): StatusChip | null {
  const base = pressureChip(pulse);
  if (relief.paused === 0 && !relief.unverified) return base;
  // The relief supervisor's word, appended to whatever the bar already says,
  // or said alone when the bar would have been quiet. A status that was
  // stale or unreadable says so: it is never "nothing paused".
  const note =
    relief.paused > 0
      ? ` · ${relief.paused} paused${relief.unverified ? " (unverified)" : ""}`
      : " · pause status unverified";
  const title =
    relief.paused > 0
      ? "GitPulse's memory protection paused a build or test under an agent. It resumes automatically. Open Health, Host load, to resume it now."
      : "GitPulse couldn't verify whether its memory protection has paused anything. Open Health, Host load.";
  if (base) return { ...base, text: base.text + note, title: `${base.title} ${title}` };
  return {
    text: `Memory protection${note}`,
    tone: relief.paused > 0 ? "warn" : "muted",
    title,
  };
}

function pressureChip(pulse: HostLoadPulse | null): StatusChip | null {
  if (!pulse || pulse.role === "unsupported" || pulse.role === "not_sampling") return null;
  if (!pulse.enabled && pulse.state !== "dead") return null;
  if (pulse.state === "dead" || pulse.state === "stale_old") {
    return {
      text: "Load sensor: no sampler",
      tone: "muted",
      title:
        pulse.age_ms === null
          ? "The host-load sampler has not written a snapshot. Memory pressure is unknown."
          : `The host-load sampler last wrote ${formatAge(pulse.age_ms)} ago. Memory pressure is unknown.`,
    };
  }
  const stale = pulse.state === "stale_recent" && pulse.age_ms !== null ? ` · stale ${formatAge(pulse.age_ms)}` : "";
  const thrash = pulse.compressor_thrash ? " · thrashing" : "";
  const governing = governorSuffix(pulse.governor);
  if (pulse.level === "critical" || pulse.level === "warn") {
    return {
      text: `Memory: ${levelLabel(pulse.level)}${thrash}${governing}${stale}`,
      tone: pulse.level === "critical" ? "danger" : "warn",
      title:
        "The kernel reports memory pressure. Heavy work started now competes for memory; open Health for what is using it and what GitPulse is shedding.",
    };
  }
  if (pulse.compressor_thrash) {
    return {
      text: `Memory: compressor thrashing${governing}${stale}`,
      tone: "warn",
      title: "The memory compressor has been thrashing for three samples in a row.",
    };
  }
  // A resource other than the kernel's pressure level can bind (swap, disk,
  // load, ...): the bar must not stay silent while the governor is acting.
  const refused = !pulse.governor.admit && !pulse.governor.overridden;
  if (refused || pulse.governor.shed_background || pulse.governor.shed_reactive) {
    return {
      text: `Host load: ${bindingLabel(pulse.governor.binding)}${governing}${stale}`,
      tone: refused ? "danger" : "warn",
      title:
        "GitPulse is acting on a host reading other than memory pressure. Open Health for which one, its value and the threshold it crossed. Thresholds are in Settings, Agents, Host load.",
    };
  }
  return null;
}

/**
 * What the governor is doing with GitPulse's own work, in one line for
 * Health: which classes are shed and why, or that nothing is. A sensor that
 * is stale-old or dead says so and says that nothing extra is shed — the
 * governor fails open, and Health must not look like it is governing then.
 */
export function governorLine(report: GovernorReport): string {
  const { verdict } = report;
  switch (verdict.reason) {
    case "not_governed":
    case "switched_off":
      return `Not governing: ${verdict.message}.`;
    case "sensor_unavailable":
      return `Ungoverned: ${verdict.message}.`;
    case "healthy":
      return `Nothing is shed. ${capitalise(verdict.message)}.`;
    default: {
      const shed = [
        verdict.shed.background ? "background" : null,
        verdict.shed.reactive ? "reactive" : null,
      ].filter((name): name is string => name !== null);
      const stale = verdict.warnings.length > 0 ? ` (${verdict.warnings.join("; ")})` : "";
      return `Shedding ${shed.join(" and ")} work: ${verdict.message}${stale}.`;
    }
  }
}

function capitalise(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** Admission in a line: whether agent launches are being refused, for how long. */
export function admissionLine(report: GovernorReport): string {
  const line = admissionVerdictLine(report);
  const advice = advisoryLine(report);
  return advice === null ? line : `${line} ${advice}`;
}

/**
 * The latest worktree build's advice. It never changes whether launches are
 * admitted, and says "launches are not affected" only when they are not being
 * refused for another reason.
 */
export function advisoryLine(report: GovernorReport): string | null {
  const advisory = report.advisory;
  if (!advisory) return null;
  const refused = !report.verdict.admit && report.override_remaining_s === null;
  return advice(advisory, refused).join(" ");
}

function advice(advisory: Advisory, refused: boolean): string[] {
  switch (advisory.reason) {
    case "several":
      return advisory.items.flatMap((item) => advice(item, refused));
    case "cache_error":
      return [`Advice: the last worktree was made without its build caches (${advisory.message}).`];
    case "disk_unknown":
      return ["Advice: free space couldn't be read, so no build cache was skipped on its account."];
    case "low_disk": {
      const names = advisory.skipped.join(", ");
      if (advisory.cause === "byte_cap") {
        const cap = advisory.cap_bytes === null ? "the byte cap" : `the ${bytes(advisory.cap_bytes)} byte cap`;
        return [`Low disk advice: ${names} was not copied into the last worktree (over ${cap}).`];
      }
      const free = advisory.free_bytes === null ? "unknown" : bytes(advisory.free_bytes);
      if (advisory.cause === "headroom_reserved") {
        return [
          `Low disk advice: ${free} free, but the headroom is reserved by concurrent copies, so ${names} was not copied into the last worktree.`,
        ];
      }
      const unaffected = refused ? "." : "; launches are not affected.";
      return [`Low disk advice: ${free} free, so ${names} was not copied into the last worktree${unaffected}`];
    }
  }
}

function admissionVerdictLine(report: GovernorReport): string {
  const { verdict } = report;
  if (report.override_remaining_s !== null && !verdict.admit) {
    return `Launches are admitted for ${report.override_remaining_s} s more, on your say-so.`;
  }
  if (!verdict.admit) {
    const since =
      report.refusing_for_s === null ? "" : ` for ${formatAge(report.refusing_for_s * 1000)}`;
    return `Agent launches are refused${since}: ${verdict.message}.`;
  }
  if (verdict.reason === "launch_gate_off") return "Agent launches are admitted: refusal is switched off in Settings.";
  return verdict.warnings.length > 0
    ? `Agent launches are admitted (${verdict.warnings.join("; ")}).`
    : "Agent launches are admitted.";
}

/** The toast text once refusal has run past the notice threshold. */
export function refusalNotice(governor: GovernorPulse): string {
  const minutes = Math.max(30, Math.floor((governor.refusing_for_s ?? 0) / 60));
  const largest =
    governor.largest_foreign === null
      ? "largest foreign footprint unknown"
      : `largest foreign footprint ${bytes(governor.largest_foreign)}`;
  const cause = bindingCause(governor.binding);
  return `Refusing agent launches for ${minutes} min under ${cause}; ${largest}.`;
}

/**
 * The slowest session write rate worth naming, in bytes a second. Below it
 * every session is ordinary background churn and a "fastest" line would only
 * point at noise.
 */
export const FASTEST_WRITER_MIN_RATE = 1_048_576;

const AGENT_LABEL: Record<AgentKind, string> = {
  claude: "Claude",
  codex: "Codex",
  antigravity: "Antigravity",
  dev_council: "DevCouncil",
  manvi: "Manvi",
};

export interface FastestWriter {
  session: AgentSession;
  /** Bytes a second, a floor. */
  rate: number;
  line: string;
}

/**
 * The session whose processes wrote fastest over the last tick, and the
 * sentence that says so; null when there is nothing honest to say: no reading
 * yet (no baseline), no session at or above the threshold, or no sessions.
 *
 * It is a statement about which session's processes are writing, never about
 * which folder grows: the counters are per process, not per path. The rate is
 * a floor (a process that started and exited between two ticks is never
 * read), so the sentence says "at least". On a truncated list the claim is
 * only about the sessions shown. A tie goes to the lower pid, so the answer
 * does not flip between ticks.
 */
export function fastestWriter(sessions: AgentSession[], truncated: boolean): FastestWriter | null {
  let best: { session: AgentSession; rate: number } | null = null;
  for (const session of sessions) {
    const rate = valueOf(session.write_rate);
    if (rate === null || rate < FASTEST_WRITER_MIN_RATE) continue;
    if (best === null || rate > best.rate || (rate === best.rate && session.pid < best.session.pid)) {
      best = { session, rate };
    }
  }
  if (best === null) return null;
  const { session, rate } = best;
  const where = session.cwd ? `in ${session.cwd}` : "in an unknown directory";
  const lead = truncated ? "Writing fastest of the sessions shown" : "Writing fastest";
  return {
    session,
    rate,
    line: `${lead}: ${AGENT_LABEL[session.kind]} session ${session.pid} ${where}, at least ${bytesPerSecond(rate)}`,
  };
}

/** A fill rate in bytes a minute: "2.7 GB/min". */
export function fillRateLabel(bytesPerMin: number): string {
  return `${humanBytes(bytesPerMin)}/min`;
}

/**
 * Where the disk is heading, for Health: "Disk filling: 2.7 GB/min, about
 * 11 min until the 16 GB floor." Says so when the floor is already reached.
 */
export function diskFillLine(fill: DiskFill): string {
  const rate = fillRateLabel(fill.rate_bytes_per_min);
  const floor = humanBytes(fill.floor_bytes);
  if (fill.eta_min <= 0) {
    return `Disk filling: ${rate}, free space is at the ${floor} floor.`;
  }
  const minutes = fill.eta_min < 1 ? "under 1" : `about ${Math.round(fill.eta_min)}`;
  return `Disk filling: ${rate}, ${minutes} min until the ${floor} floor.`;
}

/**
 * What the fill-rate rule is doing, for Health and the settings row: the
 * projection when the disk is heading for the floor, otherwise the state of
 * the measurement. A rule that has nothing to say because it has not measured
 * yet must not read like one that measured a calm disk, so the reasons are
 * said: warming up, the writing app is an older build, or the disk can't be
 * read. Null only when there is no snapshot at all.
 */
export function fillRateLine(snapshot: Snapshot | null, fill: DiskFill | null | undefined): string | null {
  if (fill) return diskFillLine(fill);
  if (!snapshot) return null;
  const reading = snapshot.disk_fill_rate;
  // This backend always emits the field, so a missing one is only ever a
  // record an older build wrote and something re-serialised without it.
  if (!reading) return FILL_OLDER_BUILD;
  if (reading.status === "value") {
    // The sustained rate is what the severe rule binds on; when it is the
    // higher of the two the line shows it, so the number on screen is the one
    // deciding. Unavailable sustained readings leave the quick one as it was.
    const sustained = snapshot.disk_fill_sustained;
    const rate = sustained?.status === "value" ? Math.max(reading.value, sustained.value) : reading.value;
    return rate > 0 ? `Fill rate: ${fillRateLabel(rate)}` : "Fill rate: not falling";
  }
  // Each reason is decided in Rust (`diskrate`, `sampler`, `snapshot`); this
  // only words it.
  switch (reading.value) {
    case "not_yet":
      return "Fill rate: warming up";
    case "disabled":
      return "Fill rate: off (host load measurement is switched off)";
    case "older":
      return FILL_OLDER_BUILD;
    case "unsupported":
      return "Fill rate: not available on this platform";
    default:
      return "Fill rate: not available (free disk space can't be read)";
  }
}

const FILL_OLDER_BUILD = "Fill rate: not available (the app writing the snapshot is an older build)";

/** The binding resource as a phrase after "under", for the refusal notice. */
function bindingCause(binding: Binding | null): string {
  switch (binding) {
    case "compressor_thrash":
      return "a thrashing memory compressor";
    case "compressor_segments":
      return "a nearly full memory compressor";
    case "swap":
      return "heavy swap use";
    case "low_disk":
      return "low disk space";
    case "disk_filling":
      return "disk space filling fast";
    case "thermal":
      return "heat";
    case "cpu_load":
      return "heavy CPU load";
    case "exec_latency":
      return "slow process start-up";
    case "battery":
      return "battery power";
    case "memory_pressure":
    case null:
      return "memory pressure";
  }
}

/** What the binding resource is, in two or three words, for the status chip. */
export function bindingLabel(binding: Binding | null): string {
  switch (binding) {
    case "compressor_thrash":
      return "compressor thrashing";
    case "compressor_segments":
      return "compressor full";
    case "swap":
      return "swap";
    case "low_disk":
      return "low disk";
    case "disk_filling":
      return "disk filling";
    case "thermal":
      return "heat";
    case "cpu_load":
      return "CPU load";
    case "exec_latency":
      return "slow start-up";
    case "battery":
      return "on battery";
    case "memory_pressure":
    case null:
      return "memory";
  }
}

/**
 * Process start-up time in one line, to sit beside memory pressure in Health
 * and in a diagnostics export. A probe that has not produced a reading says so
 * rather than reading as fast.
 */
export function execLatencyLine(report: GovernorReport): string {
  const latency = report.exec_latency;
  if (latency === null || latency === undefined) {
    return "Process start-up time: unknown (no probe reading).";
  }
  const took =
    latency.median_ms < 1000 ? `${latency.median_ms} ms` : `${(latency.median_ms / 1000).toFixed(1)} s`;
  return `Process start-up time: median ${took} over ${latency.n} probe${latency.n === 1 ? "" : "s"}${latency.slow ? " (slow: background work is shed)" : ""}.`;
}

/** What the governor adds to the status chip: refused launches outrank shedding. */
function governorSuffix(governor: GovernorPulse): string {
  if (!governor.admit && !governor.overridden) return " · launches refused";
  if (governor.shed_background || governor.shed_reactive) return " · shedding";
  return "";
}

/** memguard in one line, for Health. */
export function memguardLine(memguard: MemguardHealth): string {
  if (!memguard.present) return "memguard is not running";
  const pid = memguard.pid === null ? "" : ` (pid ${memguard.pid})`;
  const log = describeReading(memguard.log_path, (path) => path);
  const beat = describeReading(memguard.heartbeat_age_s, (s) => `${formatAge(s * 1000)} ago`);
  return `memguard running${pid} · log ${log} · last written ${beat}`;
}

/** Seconds since the epoch as a local date and time, for "ended at". */
export function wallTime(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString();
}

const readOrUnknown = <T>(value: T | null, format: (value: T) => string): string =>
  value === null ? "unreadable" : format(value);

/**
 * The previous boot's trace in sentences, for "Before the last restart": when
 * it ended, then the worst of each reading over its last 30 records. A reading
 * that could not be taken says so; it is never shown as zero.
 */
export function previousBootLines(previous: PreviousBoot): string[] {
  if (previous.error !== null) {
    return [`The last boot's trace couldn't be read: ${previous.error}.`];
  }
  if (previous.records.length === 0 || previous.ended_wall_s === null) {
    return ["The last boot's trace holds no readings."];
  }
  const p = previous.peaks;
  const first = previous.records[0].mono_ms;
  const last = previous.records[previous.records.length - 1].mono_ms;
  const spanMs = Math.max(0, last - first);
  const gap =
    previous.dropped > 0
      ? [
          `${previous.dropped} reading${previous.dropped === 1 ? " was" : "s were"} dropped while the trace write was blocked or failing, so this history has gaps. Anything dropped after the last row below is not counted.`,
        ]
      : [];
  return [
    ...gap,
    `Ended ${wallTime(previous.ended_wall_s)}; its last ${previous.records.length} readings span ${formatAge(spanMs)}.`,
    `Highest memory pressure: ${p.max_pressure === null ? "unreadable" : levelLabel(p.max_pressure)}. Governor: ${p.max_tier ?? "unknown"}${p.refused ? ", launches were refused" : ", launches were admitted"}.`,
    `Swap used, at most ${readOrUnknown(p.max_swap_used, bytes)}. Compressor segments, at most ${readOrUnknown(p.max_segment_fraction, (f) => f.toFixed(2))} of the limit.`,
    `1-minute load, at most ${readOrUnknown(p.max_load_1m, (l) => l.toFixed(1))}. Free disk, at least ${readOrUnknown(p.min_free_disk, bytes)}.`,
    `GitPulse's launched trees, at most ${readOrUnknown(p.max_launched_footprint, bytes)}. Largest other process, at most ${readOrUnknown(p.max_largest_foreign, bytes)}.`,
    `Thermal state, worst ${p.worst_thermal ?? "unreadable"}.`,
  ];
}

/** One trace record as a row: time, pressure, tier and binding, then the readings. */
export function traceRow(record: TraceRecord): string[] {
  const tier =
    record.tier === null
      ? "unknown"
      : record.binding === null
        ? record.tier
        : `${record.tier}: ${bindingLabel(record.binding)}`;
  return [
    new Date(record.wall_s * 1000).toLocaleTimeString(),
    record.pressure === null ? "unreadable" : levelLabel(record.pressure),
    record.admit === false ? `${tier} (refusing)` : tier,
    readOrUnknown(record.swap_used, bytes),
    readOrUnknown(record.load_1m, (l) => l.toFixed(1)),
    readOrUnknown(record.free_disk, bytes),
    readOrUnknown(record.launched_sessions, String),
  ];
}

/** The memory profile select's options (`host_load_profile`), in the order shown. */
export const PROFILE_CHOICES: readonly { value: ProfileChoice; label: string }[] = [
  { value: "auto", label: "Automatic (from this computer's memory)" },
  { value: "compact", label: "Compact (8 GB of memory or less)" },
  { value: "standard", label: "Standard (up to 16 GB)" },
  { value: "roomy", label: "Roomy (more than 16 GB)" },
];

/** A select's value as a choice; anything that is not one reads as automatic. */
export function asProfileChoice(value: string): ProfileChoice {
  return PROFILE_CHOICES.find((choice) => choice.value === value)?.value ?? "auto";
}

/**
 * Which tier is in use and why. An unreadable memory size is said so rather
 * than shown as a derived answer.
 */
export function profileNote(profile: ResourceProfile): string {
  switch (profile.basis) {
    case "override":
      return `In use: ${profile.tier}, as you chose.`;
    case "memory_unreadable":
      return `In use: ${profile.tier}, because this computer's memory couldn't be read.`;
    case "memory":
      return profile.memory_bytes === null
        ? `In use: ${profile.tier}.`
        : `In use: ${profile.tier}, from ${bytes(profile.memory_bytes)} of memory.`;
  }
}
