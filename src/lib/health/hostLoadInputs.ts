/**
 * The governor's inputs as Settings shows them: what each one is called, what
 * it measures, the unit its thresholds are in, and the live reading beside it.
 * The rules mirror `sysload/thresholds.rs` (the backend refuses a bad value
 * too; this only says so before the save).
 */
import type { DiskGuard, InputMode, Rule, Snapshot, Thresholds } from "../stores/hostLoadStore";
import { valueOf } from "./hostLoad";

/** The inputs that have a row of their own; the disk guards have theirs below. */
export type InputId = Exclude<keyof Thresholds, "disk_auto" | "disk_guard">;

export interface InputSpec {
  id: InputId;
  /** The Settings catalog id of this row. */
  settingId: string;
  label: string;
  /** What it measures and what the thresholds mean, as plain sentences. */
  description: string;
  /** Null for an input whose states are fixed (thermal, battery). */
  rule: null | {
    unit: string;
    step: number;
    max: number;
    /** Lower is worse: disk free space. */
    lowerIsWorse: boolean;
    elevatedLabel: string;
    severeLabel: string;
  };
  /** Whether "Also refuse agent launches" is offered (battery never refuses). */
  canRefuse: boolean;
}

export const INPUT_SPECS: readonly InputSpec[] = [
  {
    id: "swap",
    settingId: "host-load-swap",
    label: "Swap used",
    description:
      "How much of this computer's memory has spilled to swap on disk, as a multiple of its physical memory. A machine this far into swap slows to a crawl and can hang, even while the system still reports normal memory pressure.",
    rule: { unit: "x memory", step: 0.05, max: 8, lowerIsWorse: false, elevatedLabel: "Slow background work at", severeLabel: "Refuse launches at" },
    canRefuse: true,
  },
  {
    id: "compressor_segments",
    settingId: "host-load-compressor",
    label: "Memory compressor",
    description:
      "How full the memory compressor is, as a fraction of its segment limit. The 2026-10-07 panic happened with it full.",
    rule: { unit: "of limit", step: 0.05, max: 1, lowerIsWorse: false, elevatedLabel: "Slow background work at", severeLabel: "Refuse launches at" },
    canRefuse: true,
  },
  {
    id: "load",
    settingId: "host-load-cpu",
    label: "CPU load",
    description:
      "The 1-minute load average per logical CPU. A load of 1.0 per CPU keeps every core busy; far above that, work is queuing.",
    rule: { unit: "per CPU", step: 0.5, max: 256, lowerIsWorse: false, elevatedLabel: "Slow background work at", severeLabel: "Refuse launches at" },
    canRefuse: true,
  },
  {
    id: "thermal",
    settingId: "host-load-thermal",
    label: "Heat",
    description:
      "The system's thermal state. Serious counts as elevated, critical as severe (only when launches may be refused).",
    rule: null,
    canRefuse: true,
  },
  {
    id: "battery",
    settingId: "host-load-battery",
    label: "Battery power",
    description: "Running on battery counts as elevated, so background work is slowed to save power. It never refuses a launch.",
    rule: null,
    canRefuse: false,
  },
  {
    id: "disk",
    settingId: "host-load-disk",
    label: "Free disk space",
    description:
      "Free space on the volume GitPulse keeps its state on. Lower is worse: below the first number background work is slowed, below the second launches may be refused. Swap files share this space, so by default both numbers follow this computer's memory: half of it and a quarter of it, never under 20 and 10 GiB. A number you set is kept as you set it.",
    rule: { unit: "GiB free", step: 1, max: 1_048_576, lowerIsWorse: true, elevatedLabel: "Slow background work below", severeLabel: "Refuse launches below" },
    canRefuse: true,
  },
];

export const MODE_LABELS: Record<InputMode, string> = {
  off: "Off",
  shed: "Slow GitPulse's background work",
  refuse: "Also refuse agent launches",
};

export function specOf(id: InputId): InputSpec {
  const spec = INPUT_SPECS.find((entry) => entry.id === id);
  if (!spec) throw new Error(`unknown host-load input ${id}`);
  return spec;
}

/** Why a rule cannot be saved, or null. Mirrors `rule_problem` in Rust. */
export function ruleProblem(spec: InputSpec, rule: Rule): string | null {
  if (!spec.rule) return null;
  const { max, lowerIsWorse, unit } = spec.rule;
  const { elevated_at: elevated, severe_at: severe } = rule;
  if (!Number.isFinite(elevated) || !Number.isFinite(severe)) {
    return `${spec.label}: both thresholds must be numbers.`;
  }
  if (elevated <= 0 || severe <= 0 || elevated > max || severe > max) {
    return `${spec.label}: thresholds must be above 0 and at most ${max} ${unit}.`;
  }
  if (lowerIsWorse && severe > elevated) {
    return `${spec.label}: the refuse level must not be above the slow-down level, since less free space is worse.`;
  }
  if (!lowerIsWorse && severe < elevated) {
    return `${spec.label}: the refuse level must not be below the slow-down level.`;
  }
  return null;
}

const GIB = 2 ** 30;

/**
 * The input's live reading in its threshold unit, or a sentence for why there
 * is none ("not measured here", "unreadable"). Never a zero for a missing
 * reading.
 */
export function liveReading(id: InputId, snapshot: Snapshot | null): string | null {
  if (!snapshot) return null;
  const host = snapshot.host;
  const gap = (reading: { status: string; value: unknown } | undefined): string => {
    const why = reading && reading.status === "unavailable" ? reading.value : "failed";
    return why === "unsupported" ? "not measured here" : why === "not_yet" ? "pending" : "unreadable";
  };
  switch (id) {
    case "swap": {
      const used = valueOf(host.swap_used);
      const memory = valueOf(host.memsize);
      if (used === null || memory === null || memory === 0) return gap(used === null ? host.swap_used : host.memsize);
      return `now ${(used / memory).toFixed(2)}x`;
    }
    case "compressor_segments": {
      const segments = valueOf(host.compressor_segments);
      if (segments === null) return gap(host.compressor_segments);
      if (segments.limit === 0) return "unreadable";
      return `now ${(segments.total / segments.limit).toFixed(2)}`;
    }
    case "load": {
      const load = valueOf(host.load);
      const cores = valueOf(host.cores);
      if (load === null || cores === null || cores.logical === 0) return gap(load === null ? host.load : host.cores);
      return `now ${(load[0] / cores.logical).toFixed(2)}`;
    }
    case "thermal": {
      const state = valueOf(host.thermal);
      return state === null ? gap(host.thermal) : `now ${state}`;
    }
    case "battery": {
      const power = valueOf(host.power);
      return power === null ? gap(host.power) : power === "battery" ? "now on battery" : "now on mains power";
    }
    case "disk": {
      const free = valueOf(host.free_disk);
      return free === null ? gap(host.free_disk) : `now ${(free / GIB).toFixed(1)} GiB`;
    }
  }
}

/** The disk guards beyond the level, as Settings rows: their own mode and numbers. */
export type GuardId = keyof DiskGuard;

export const GUARD_SPECS: Record<GuardId, { settingId: string; label: string; description: string }> = {
  fill: {
    settingId: "host-load-disk-fill",
    label: "Disk filling fast",
    description:
      "Watches how fast free space is falling and how long that leaves before the refuse level above. A coverage build once took 145 GiB in about twenty minutes; the level alone says nothing until little is left. Slow background work when the projection is under the first number of minutes, refuse launches under the second.",
  },
  coverage: {
    settingId: "host-load-disk-coverage",
    label: "Coverage build headroom",
    description:
      "Free space a coverage build (cargo llvm-cov) needs before an agent may start it. Below it the command gate refuses the build, or only says so when set to slow. Cleaning coverage output is never refused.",
  },
};

const MAX_HORIZON_MIN = 10_080;

/** Why the fill rule cannot be saved, or null. Mirrors `fill_problem` in Rust. */
export function fillProblem(rule: DiskGuard["fill"]): string | null {
  const { elevated_within_min: elevated, severe_within_min: severe } = rule;
  if (!Number.isFinite(elevated) || !Number.isFinite(severe)) return "Disk filling: both horizons must be numbers.";
  if (elevated <= 0 || severe <= 0 || elevated > MAX_HORIZON_MIN || severe > MAX_HORIZON_MIN) {
    return `Disk filling: horizons must be above 0 and at most ${MAX_HORIZON_MIN} minutes.`;
  }
  if (severe > elevated) return "Disk filling: the refuse horizon must not be longer than the slow-down one.";
  return null;
}

/** Why the coverage headroom cannot be saved, or null. Mirrors `coverage_problem` in Rust. */
export function coverageProblem(rule: DiskGuard["coverage"]): string | null {
  const gib = rule.headroom_gib;
  if (!Number.isFinite(gib) || gib <= 0 || gib > 1_048_576) {
    return "Coverage headroom: must be above 0 and at most 1048576 GiB.";
  }
  return null;
}
