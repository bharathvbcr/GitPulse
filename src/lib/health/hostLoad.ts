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
  if (pulse.level === "critical" || pulse.level === "warn") {
    return {
      text: `Memory: ${levelLabel(pulse.level)}${thrash}${stale}`,
      tone: pulse.level === "critical" ? "danger" : "warn",
      title:
        "The kernel reports memory pressure. Heavy work started now competes for memory; open Health for what is using it.",
    };
  }
  if (pulse.compressor_thrash) {
    return {
      text: `Memory: compressor thrashing${stale}`,
      tone: "warn",
      title: "The memory compressor has been thrashing for three samples in a row.",
    };
  }
  return null;
}

/** memguard in one line, for Health. */
export function memguardLine(memguard: MemguardHealth): string {
  if (!memguard.present) return "memguard is not running";
  const pid = memguard.pid === null ? "" : ` (pid ${memguard.pid})`;
  const log = describeReading(memguard.log_path, (path) => path);
  const beat = describeReading(memguard.heartbeat_age_s, (s) => `${formatAge(s * 1000)} ago`);
  return `memguard running${pid} · log ${log} · last written ${beat}`;
}
