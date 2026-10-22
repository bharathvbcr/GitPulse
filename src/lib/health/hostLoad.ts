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
  GovernorPulse,
  Advisory,
  GovernorReport,
  HostLoadPulse,
  Level,
  MemguardHealth,
  Reading,
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
  failed: "could not read",
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
      return ["Advice: free space could not be read, so no build cache was skipped on its account."];
    case "low_disk": {
      const names = advisory.skipped.join(", ");
      if (advisory.cause === "byte_cap") {
        const cap = advisory.cap_bytes === null ? "the byte cap" : `the ${bytes(advisory.cap_bytes)} byte cap`;
        return [`Low disk advice: ${names} was not copied into the last worktree (over ${cap}).`];
      }
      const free = advisory.free_bytes === null ? "unknown" : bytes(advisory.free_bytes);
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
  const cause =
    governor.binding === "compressor_thrash" ? "a thrashing memory compressor" : "memory pressure";
  return `Refusing agent launches for ${minutes} min under ${cause}; ${largest}.`;
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
