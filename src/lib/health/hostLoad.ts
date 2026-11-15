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
import type {
  Binding,
  GovernorPulse,
  Advisory,
  GovernorReport,
  HostLoadPulse,
  Level,
  MemguardHealth,
  PreviousBoot,
  Reading,
  TraceRecord,
  SensorState,
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
export function statusChip(pulse: HostLoadPulse | null): StatusChip | null {
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
    case "thermal":
      return "heat";
    case "cpu_load":
      return "heavy CPU load";
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
    case "thermal":
      return "heat";
    case "cpu_load":
      return "CPU load";
    case "battery":
      return "on battery";
    case "memory_pressure":
    case null:
      return "memory";
  }
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
