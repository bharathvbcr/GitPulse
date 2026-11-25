/**
 * Presentation models and live sparkline geometry for the status bar host-load
 * strip and its expandable live metrics popover.
 *
 * Keeps macOS/Linux formatting and unavailable-reason honesty in pure,
 * framework-free TypeScript so every metric card can be unit-tested without
 * mounting Svelte.
 */
import {
  isLiveHostLoadPulse,
  type HostLoadPulse,
  type HostLoadStatus,
  type Snapshot,
  type Thermal,
} from "../stores/hostLoadStore";
import {
  bytes,
  bytesPerSecond,
  describeReading,
  levelLabel,
  sensorStateLabel,
  statusChip,
  valueOf,
  type StatusChip,
} from "./hostLoad";
import { NO_RELIEF_CHIP, type ReliefChipInput } from "./hostRelief";

export type MetricSeverity = "normal" | "warn" | "critical";
export type MetricKind = "cpu" | "gpu" | "memory" | "thermal" | "storage";
export type BottomBarDetail = "full" | "compact" | "minimal";

export interface BottomBarMetricItem {
  kind: MetricKind;
  label: string;
  title: string;
  severity: MetricSeverity;
}

export interface BottomBarMetricsModel {
  visible: boolean;
  alertChip: StatusChip | null;
  items: readonly BottomBarMetricItem[];
  severity: MetricSeverity;
  summaryLabel: string;
}

export interface SparklineGeometry {
  linePath: string;
  areaPath: string;
  lastPoint: { x: number; y: number } | null;
  peak: number | null;
  latest: number | null;
}

export interface MetricStat {
  label: string;
  value: string;
}

export interface MemorySegment {
  key: "app" | "wired" | "compressed" | "cached";
  label: string;
  bytes: number;
  pctOfTotal: number;
}

export interface PopoverDetailModel {
  sensorLabel: string;
  powerSource: "ac" | "battery" | "unknown";
  powerLabel: string;
  topologyLabel: string | null;
  governorLine: string;
  cpu: {
    available: boolean;
    title: string;
    primaryText: string;
    subtext: string;
    severity: MetricSeverity;
    series: readonly (number | null)[];
    domainMax?: number;
    seriesUnit: "%" | "x";
    stats: readonly MetricStat[];
  };
  gpu: {
    available: boolean;
    primaryText: string;
    subtext: string;
    severity: MetricSeverity;
    series: readonly (number | null)[];
    stats: readonly MetricStat[];
  };
  memory: {
    available: boolean;
    primaryText: string;
    subtext: string;
    usedPct: number | null;
    severity: MetricSeverity;
    series: readonly (number | null)[];
    segments: readonly MemorySegment[];
    stats: readonly MetricStat[];
  };
  thermal: {
    available: boolean;
    state: Thermal | null;
    stepIndex: number;
    primaryText: string;
    subtext: string;
    severity: MetricSeverity;
    series: readonly (number | null)[];
  };
  storage: {
    available: boolean;
    primaryText: string;
    subtext: string;
    usedPct: number | null;
    severity: MetricSeverity;
    stats: readonly MetricStat[];
  };
}

const SEVERITY_RANK: Record<MetricSeverity, number> = {
  normal: 0,
  warn: 1,
  critical: 2,
};

function maxSeverity(...levels: MetricSeverity[]): MetricSeverity {
  let worst: MetricSeverity = "normal";
  for (const level of levels) {
    if (SEVERITY_RANK[level] > SEVERITY_RANK[worst]) worst = level;
  }
  return worst;
}

function compactSize(value: number): string {
  const gib = 2 ** 30;
  const tib = 2 ** 40;
  if (value >= tib) return `${(value / tib).toFixed(1)}T`;
  if (value >= 10 * gib) return `${Math.round(value / gib)}G`;
  if (value >= gib) return `${(value / gib).toFixed(1)}G`;
  const mib = 2 ** 20;
  if (value >= mib) return `${Math.round(value / mib)}M`;
  return bytes(value);
}

const THERMAL_LABEL: Record<Thermal, string> = {
  nominal: "Nominal",
  fair: "Fair",
  serious: "Serious",
  critical: "Critical",
};

const THERMAL_STEP: Record<Thermal, number> = {
  nominal: 0,
  fair: 1,
  serious: 2,
  critical: 3,
};

function cpuSeverity(cpuPct: number | null | undefined, loadPerCore: number | null | undefined): MetricSeverity {
  if (cpuPct !== null && cpuPct !== undefined && Number.isFinite(cpuPct)) {
    if (cpuPct >= 90) return "critical";
    if (cpuPct >= 75) return "warn";
    return "normal";
  }
  if (loadPerCore !== null && loadPerCore !== undefined && Number.isFinite(loadPerCore)) {
    if (loadPerCore >= 4) return "critical";
    if (loadPerCore >= 2) return "warn";
  }
  return "normal";
}

function gpuSeverity(gpuPct: number | null | undefined): MetricSeverity {
  if (gpuPct === null || gpuPct === undefined || !Number.isFinite(gpuPct)) return "normal";
  if (gpuPct >= 92) return "critical";
  if (gpuPct >= 80) return "warn";
  return "normal";
}

function memorySeverity(
  pulse: HostLoadPulse,
  used: number | null | undefined,
  total: number | null | undefined,
): MetricSeverity {
  if (pulse.level === "critical") return "critical";
  if (pulse.level === "warn" || pulse.compressor_thrash) return "warn";
  if (used !== null && used !== undefined && total && total > 0) {
    const ratio = used / total;
    if (ratio >= 0.92) return "critical";
    if (ratio >= 0.82) return "warn";
  }
  return "normal";
}

function thermalSeverity(thermal: Thermal | null | undefined, tempC: number | null | undefined): MetricSeverity {
  if (thermal === "critical") return "critical";
  if (thermal === "serious") return "warn";
  if (tempC !== null && tempC !== undefined && Number.isFinite(tempC)) {
    if (tempC >= 98) return "critical";
    if (tempC >= 88) return "warn";
  }
  return "normal";
}

function storageSeverity(pulse: HostLoadPulse, free: number | null | undefined, total: number | null | undefined): MetricSeverity {
  if (pulse.governor.binding === "low_disk" || pulse.governor.binding === "disk_filling") {
    if (!pulse.governor.admit && !pulse.governor.overridden) return "critical";
    return "warn";
  }
  if (free !== null && free !== undefined && total && total > 0) {
    const freeRatio = free / total;
    if (freeRatio < 0.08) return "critical";
    if (freeRatio < 0.15) return "warn";
  }
  return "normal";
}

/**
 * Computes the compact bottom-bar indicator items and alert badge from the
 * latest pulse and the current StatusBar density mode.
 */
export function bottomBarMetrics(
  pulse: HostLoadPulse | null,
  detail: BottomBarDetail = "full",
  relief: ReliefChipInput = NO_RELIEF_CHIP,
): BottomBarMetricsModel {
  if (!pulse) {
    return {
      visible: false,
      alertChip: null,
      items: [],
      severity: "normal",
      summaryLabel: "Host metrics unavailable",
    };
  }

  const alertChip = statusChip(pulse, relief);
  const alertSeverity: MetricSeverity =
    alertChip === null ? "normal" : alertChip.tone === "danger" ? "critical" : "warn";

  if (detail === "minimal" || !isLiveHostLoadPulse(pulse)) {
    return {
      visible: alertChip !== null,
      alertChip,
      items: [],
      severity: alertSeverity,
      summaryLabel: alertChip
        ? alertChip.text
        : !pulse.enabled
          ? "Host sensor switched off"
          : "Host metrics unavailable",
    };
  }

  const items: BottomBarMetricItem[] = [];

  if (pulse.cpu_pct !== null && pulse.cpu_pct !== undefined && Number.isFinite(pulse.cpu_pct)) {
    items.push({
      kind: "cpu",
      label: `${Math.round(pulse.cpu_pct)}%`,
      title: `CPU usage: ${pulse.cpu_pct.toFixed(1)}%${pulse.load_per_core != null ? ` · load ${pulse.load_per_core.toFixed(2)}/core` : ""}`,
      severity: cpuSeverity(pulse.cpu_pct, pulse.load_per_core),
    });
  } else if (pulse.load_per_core !== null && pulse.load_per_core !== undefined && Number.isFinite(pulse.load_per_core)) {
    items.push({
      kind: "cpu",
      label: `${pulse.load_per_core.toFixed(1)}x`,
      title: `CPU load per core: ${pulse.load_per_core.toFixed(2)}`,
      severity: cpuSeverity(null, pulse.load_per_core),
    });
  }

  if (pulse.gpu_pct !== null && pulse.gpu_pct !== undefined && Number.isFinite(pulse.gpu_pct)) {
    items.push({
      kind: "gpu",
      label: `${Math.round(pulse.gpu_pct)}%`,
      title: `GPU utilization: ${pulse.gpu_pct.toFixed(1)}%`,
      severity: gpuSeverity(pulse.gpu_pct),
    });
  }

  if (pulse.mem_used_bytes !== null && pulse.mem_used_bytes !== undefined) {
    const total = pulse.mem_total_bytes ?? null;
    const pct = total && total > 0 ? Math.round((pulse.mem_used_bytes / total) * 100) : null;
    const label =
      detail === "compact" && pct !== null ? `${pct}%` : compactSize(pulse.mem_used_bytes);
    items.push({
      kind: "memory",
      label,
      title:
        total && total > 0
          ? `Memory used: ${bytes(pulse.mem_used_bytes)} of ${bytes(total)} (${pct}%)`
          : `Memory used: ${bytes(pulse.mem_used_bytes)}`,
      severity: memorySeverity(pulse, pulse.mem_used_bytes, total),
    });
  }

  const hasTemp =
    pulse.thermal_temp_c !== null &&
    pulse.thermal_temp_c !== undefined &&
    Number.isFinite(pulse.thermal_temp_c);
  if (hasTemp || pulse.thermal != null) {
    const thermalName = pulse.thermal ? THERMAL_LABEL[pulse.thermal] : "Unknown";
    const tempText = hasTemp ? `${pulse.thermal_temp_c!.toFixed(1)} °C` : null;
    const label = hasTemp
      ? `${Math.round(pulse.thermal_temp_c!)}°`
      : detail === "full"
        ? thermalName
        : thermalName.slice(0, 4);
    items.push({
      kind: "thermal",
      label,
      title: tempText ? `Thermal: ${thermalName.toLowerCase()} · ${tempText}` : `Thermal state: ${thermalName.toLowerCase()}`,
      severity: thermalSeverity(pulse.thermal, pulse.thermal_temp_c),
    });
  }

  if (pulse.disk_free_bytes !== null && pulse.disk_free_bytes !== undefined) {
    const total = pulse.disk_total_bytes ?? null;
    items.push({
      kind: "storage",
      label: compactSize(pulse.disk_free_bytes),
      title:
        total && total > 0
          ? `Storage free (immediate unallocated space, excluding purgeable): ${bytes(pulse.disk_free_bytes)} of ${bytes(total)}`
          : `Storage free (immediate unallocated space, excluding purgeable): ${bytes(pulse.disk_free_bytes)}`,
      severity: storageSeverity(pulse, pulse.disk_free_bytes, total),
    });
  }

  const severity = maxSeverity(alertSeverity, ...items.map((item) => item.severity));
  const summaryParts = items.map((item) => item.title);
  const summaryLabel = alertChip
    ? `${alertChip.text}. ${summaryParts.join(". ")}`
    : summaryParts.length > 0
      ? `Host metrics — ${summaryParts.join(". ")}`
      : "Host metrics";

  return {
    visible: alertChip !== null || items.length > 0,
    alertChip,
    items,
    severity,
    summaryLabel,
  };
}

/**
 * Converts a sequence of optional samples into right-aligned SVG `<path>`
 * strings without bridging across missing (`null` / non-finite) gaps.
 */
export function sparklineGeometry(
  values: readonly (number | null | undefined)[],
  width: number,
  height: number,
  maxSlots = 60,
  domainMax?: number,
): SparklineGeometry {
  const slots = Math.max(1, Math.floor(maxSlots));
  const slice = values.length > slots ? values.slice(values.length - slots) : values;
  const finiteValues = slice.filter(
    (v): v is number => v !== null && v !== undefined && Number.isFinite(v),
  );
  if (finiteValues.length === 0 || width <= 0 || height <= 0) {
    return {
      linePath: "",
      areaPath: "",
      lastPoint: null,
      peak: null,
      latest: null,
    };
  }

  const peak = Math.max(...finiteValues);
  const rawLatest = slice[slice.length - 1];
  const latest =
    rawLatest !== null && rawLatest !== undefined && Number.isFinite(rawLatest)
      ? rawLatest
      : null;

  const padY = Math.min(2, height / 6);
  const usableH = Math.max(1, height - padY * 2);
  const baselineY = Number((height - padY).toFixed(2));
  const effectiveMax =
    domainMax !== undefined && Number.isFinite(domainMax) && domainMax > 0
      ? Math.max(domainMax, peak)
      : Math.max(peak, 1);

  const step = slots > 1 ? width / (slots - 1) : 0;
  const offset = slots - slice.length;
  const pointAt = (index: number, value: number) => {
    const x = slots === 1 ? width / 2 : (offset + index) * step;
    const clamped = Math.max(0, value);
    const y = padY + usableH * (1 - clamped / effectiveMax);
    return {
      x: Number(x.toFixed(2)),
      y: Number(y.toFixed(2)),
    };
  };

  const runs: Array<Array<{ x: number; y: number }>> = [];
  let current: Array<{ x: number; y: number }> = [];
  for (let i = 0; i < slice.length; i++) {
    const v = slice[i];
    if (v !== null && v !== undefined && Number.isFinite(v)) {
      current.push(pointAt(i, v));
    } else if (current.length > 0) {
      runs.push(current);
      current = [];
    }
  }
  if (current.length > 0) runs.push(current);

  const lineParts: string[] = [];
  const areaParts: string[] = [];
  let lastPoint: { x: number; y: number } | null = null;

  for (const run of runs) {
    const first = run[0];
    const last = run[run.length - 1];
    lastPoint = last;
    if (run.length === 1) {
      lineParts.push(`M ${first.x} ${first.y}`);
    } else {
      const segs = run.map((pt, idx) => `${idx === 0 ? "M" : "L"} ${pt.x} ${pt.y}`).join(" ");
      lineParts.push(segs);
      areaParts.push(
        `M ${first.x} ${baselineY} ${segs.replace(/^M/, "L")} L ${last.x} ${baselineY} Z`,
      );
    }
  }

  return {
    linePath: lineParts.join(" "),
    areaPath: areaParts.join(" "),
    lastPoint,
    peak,
    latest,
  };
}

function powerSourceOf(
  pulse: HostLoadPulse,
  host: Snapshot["host"] | undefined,
  live: boolean,
): { source: "ac" | "battery" | "unknown"; label: string } {
  if (!live) {
    return {
      source: "unknown",
      label: !pulse.enabled ? "Switched off" : sensorStateLabel(pulse.state, pulse.age_ms),
    };
  }
  const power = pulse.power ?? valueOf(host?.power);
  if (power === "ac") return { source: "ac", label: "AC power" };
  if (power === "battery") return { source: "battery", label: "Battery" };
  return { source: "unknown", label: describeReading(host?.power, (p) => (p === "ac" ? "AC power" : "Battery")) };
}

/** Builds the detailed live-graph model for the bottom-bar host metrics popover. */
export function popoverDetailModel(
  pulse: HostLoadPulse,
  status: HostLoadStatus | null,
  history: readonly HostLoadPulse[],
): PopoverDetailModel {
  const live = isLiveHostLoadPulse(pulse);
  const snapshot = live && status?.snapshot?.enabled !== false ? (status?.snapshot ?? null) : null;
  const host = snapshot?.host;
  const unavailableLabel = !pulse.enabled ? "switched off" : sensorStateLabel(pulse.state, pulse.age_ms);
  const inactiveSubtext = !pulse.enabled ? "Host sensor is off" : "Sampler inactive";

  const powerInfo = powerSourceOf(pulse, host, live);
  const coresVal = valueOf(host?.cores);
  const topologyLabel = coresVal
    ? coresVal.levels === 2 && coresVal.perf0 > 0 && coresVal.perf1 > 0
      ? `${coresVal.logical} cores (${coresVal.perf0} Super + ${coresVal.perf1} Performance)`
      : `${coresVal.logical} logical cores`
    : null;

  // CPU card
  const cpuVal = valueOf(host?.cpu_usage);
  const loadVal = valueOf(host?.load);
  const hasCpuPctHistory = history.some(
    (item) => isLiveHostLoadPulse(item) && item.cpu_pct !== null && item.cpu_pct !== undefined,
  );
  const cpuPctNow = live ? (pulse.cpu_pct ?? cpuVal?.total_pct ?? null) : null;
  const loadPerCoreNow = live ? (pulse.load_per_core ?? null) : null;
  const usePctSeries = hasCpuPctHistory || cpuPctNow !== null;
  const cpuSeries = history.map((item) =>
    isLiveHostLoadPulse(item) ? (usePctSeries ? (item.cpu_pct ?? null) : (item.load_per_core ?? null)) : null,
  );
  const cpuStats: MetricStat[] = [];
  if (loadVal) {
    cpuStats.push({
      label: "Load avg",
      value: `${loadVal[0].toFixed(2)} · ${loadVal[1].toFixed(2)} · ${loadVal[2].toFixed(2)}`,
    });
  } else if (loadPerCoreNow !== null && loadPerCoreNow !== undefined) {
    cpuStats.push({ label: "Load/core", value: `${loadPerCoreNow.toFixed(2)}x` });
  }
  if (coresVal) {
    cpuStats.push({ label: "Cores", value: String(coresVal.logical) });
  }

  // GPU card
  const gpuVal = valueOf(host?.gpu);
  const gpuPctNow = live ? (pulse.gpu_pct ?? gpuVal?.utilization_pct ?? null) : null;
  const gpuAvailable = live && gpuPctNow !== null;
  const gpuSubParts: string[] = [];
  if (gpuVal?.renderer_pct !== null && gpuVal?.renderer_pct !== undefined) {
    gpuSubParts.push(`Renderer ${gpuVal.renderer_pct.toFixed(1)}%`);
  }
  if (gpuVal?.tiler_pct !== null && gpuVal?.tiler_pct !== undefined) {
    gpuSubParts.push(`Tiler ${gpuVal.tiler_pct.toFixed(1)}%`);
  }
  const gpuStats: MetricStat[] = [];
  if (gpuVal?.memory_in_use !== null && gpuVal?.memory_in_use !== undefined) {
    gpuStats.push({ label: "Memory in use", value: bytes(gpuVal.memory_in_use) });
  }
  if (gpuVal?.memory_allocated !== null && gpuVal?.memory_allocated !== undefined) {
    gpuStats.push({ label: "Allocated", value: bytes(gpuVal.memory_allocated) });
  }

  // Memory card
  const memSample = valueOf(host?.memory);
  const memUsed = live ? (pulse.mem_used_bytes ?? memSample?.used_bytes ?? null) : null;
  const memTotal = live ? (pulse.mem_total_bytes ?? valueOf(host?.memsize) ?? null) : null;
  const memUsedPct =
    memUsed !== null && memTotal && memTotal > 0
      ? Math.max(0, Math.min(100, Math.round((memUsed / memTotal) * 1000) / 10))
      : null;
  const memSegments: MemorySegment[] = [];
  // Only emit the 4-part Activity Monitor stacked bar when App memory is
  // published (macOS); Linux only measures Cached inside the breakdown and
  // renders a single overall used-percentage bar instead.
  if (memSample && memSample.app_bytes !== null && memTotal && memTotal > 0) {
    const entries: Array<[MemorySegment["key"], string, number | null]> = [
      ["app", "App", memSample.app_bytes],
      ["wired", "Wired", memSample.wired_bytes],
      ["compressed", "Compressed", memSample.compressed_bytes],
      ["cached", "Cached", memSample.cached_bytes],
    ];
    for (const [key, label, val] of entries) {
      if (val !== null && val > 0) {
        memSegments.push({
          key,
          label,
          bytes: val,
          pctOfTotal: Math.min(100, Math.round((val / memTotal) * 1000) / 10),
        });
      }
    }
  }
  const swapUsed = live ? (pulse.swap_used_bytes ?? valueOf(host?.swap_used) ?? null) : null;
  const swapTotal = valueOf(host?.swap_total) ?? null;
  const memStats: MetricStat[] = [
    {
      label: "Pressure",
      value: `${levelLabel(live ? pulse.level : null)}${live && pulse.compressor_thrash ? " · thrashing" : ""}`,
    },
  ];
  if (swapUsed !== null) {
    memStats.push({
      label: "Swap",
      value: swapTotal !== null ? `${bytes(swapUsed)} of ${bytes(swapTotal)}` : bytes(swapUsed),
    });
  } else if (host?.swap_used) {
    memStats.push({ label: "Swap", value: describeReading(host.swap_used, bytes) });
  }

  // Thermal card
  const thermalState = live ? (pulse.thermal ?? valueOf(host?.thermal) ?? null) : null;
  const tempNow = live ? (pulse.thermal_temp_c ?? valueOf(host?.thermal_temp_c) ?? null) : null;
  const thermalAvailable = live && (thermalState !== null || tempNow !== null);
  const thermalStateText = thermalState ? THERMAL_LABEL[thermalState] : "Unknown";

  // Storage card
  const diskFree = live ? (pulse.disk_free_bytes ?? valueOf(host?.free_disk) ?? null) : null;
  const diskTotal = live ? (pulse.disk_total_bytes ?? valueOf(host?.total_disk) ?? null) : null;
  const diskUsedPct =
    diskFree !== null && diskTotal && diskTotal > 0
      ? Math.max(0, Math.min(100, Math.round(((diskTotal - diskFree) / diskTotal) * 1000) / 10))
      : null;
  const storageStats: MetricStat[] = [];
  const writeRate = live ? (pulse.disk_write_rate ?? valueOf(snapshot?.launched_write_rate) ?? null) : null;
  if (writeRate !== null) {
    storageStats.push({ label: "Agent writes", value: bytesPerSecond(writeRate) });
  }
  const fillRate = live ? (pulse.disk_fill_rate ?? valueOf(snapshot?.disk_fill_rate) ?? null) : null;
  if (fillRate !== null) {
    storageStats.push({ label: "Fill rate", value: `${bytes(fillRate)}/min` });
  }

  const statusIsLive =
    status !== null &&
    (status.sensor.enabled ?? false) &&
    (status.sensor.state === "fresh" || status.sensor.state === "stale_recent");
  const statusMatchesPulse =
    status !== null &&
    status.sensor.state === pulse.state &&
    (status.sensor.enabled ?? false) === pulse.enabled;

  return {
    sensorLabel: !pulse.enabled ? "switched off" : sensorStateLabel(pulse.state, pulse.age_ms),
    powerSource: powerInfo.source,
    powerLabel: powerInfo.label,
    topologyLabel,
    governorLine:
      statusMatchesPulse || (live && statusIsLive)
        ? status!.governor.verdict.message
        : !pulse.enabled
          ? "Launches admitted (sensor off)"
          : !live
            ? pulse.governor.admit
              ? "Launches admitted (sensor inactive)"
              : "Agent launches refused"
            : pulse.governor.admit
              ? "Launches admitted"
              : "Agent launches refused",
    cpu: {
      available: live && (cpuPctNow !== null || loadPerCoreNow !== null),
      title: usePctSeries ? "CPU utilization" : "CPU load",
      primaryText:
        cpuPctNow !== null
          ? `${cpuPctNow.toFixed(1)}%`
          : loadPerCoreNow !== null && loadPerCoreNow !== undefined
            ? `${loadPerCoreNow.toFixed(2)}x`
            : !live
              ? unavailableLabel
              : describeReading(host?.cpu_usage, (c) => `${c.total_pct.toFixed(1)}%`),
      subtext: cpuVal
        ? `User ${cpuVal.user_pct.toFixed(1)}% · System ${cpuVal.system_pct.toFixed(1)}%`
        : !live
          ? inactiveSubtext
          : host?.cpu_usage
            ? describeReading(host.cpu_usage, (c) => `${c.total_pct.toFixed(1)}%`)
            : "User/system split pending next sample",
      severity: live ? cpuSeverity(cpuPctNow, loadPerCoreNow) : "normal",
      series: cpuSeries,
      domainMax: usePctSeries ? 100 : undefined,
      seriesUnit: usePctSeries ? "%" : "x",
      stats: cpuStats,
    },
    gpu: {
      available: gpuAvailable,
      primaryText: gpuPctNow !== null
        ? `${gpuPctNow.toFixed(1)}%`
        : !live
          ? unavailableLabel
          : describeReading(host?.gpu, (g) => `${g.utilization_pct.toFixed(1)}%`),
      subtext: gpuAvailable
        ? gpuSubParts.length > 0
          ? gpuSubParts.join(" · ")
          : "Device utilization"
        : !live
          ? inactiveSubtext
          : host?.gpu
            ? describeReading(host.gpu, (g) => `${g.utilization_pct.toFixed(1)}%`)
            : "Not measured on this host",
      severity: live ? gpuSeverity(gpuPctNow) : "normal",
      series: history.map((item) => (isLiveHostLoadPulse(item) ? (item.gpu_pct ?? null) : null)),
      stats: gpuStats,
    },
    memory: {
      available: live && memUsed !== null,
      primaryText:
        memUsed !== null
          ? memTotal
            ? `${bytes(memUsed)} of ${bytes(memTotal)}`
            : bytes(memUsed)
          : !live
            ? unavailableLabel
            : describeReading(host?.memory, (m) => bytes(m.used_bytes)),
      subtext:
        memUsedPct !== null
          ? memSegments.length === 0 && memSample?.cached_bytes != null
            ? `${memUsedPct}% used · Cached ${bytes(memSample.cached_bytes)}`
            : `${memUsedPct}% used`
          : !live
            ? inactiveSubtext
            : "Memory breakdown unavailable",
      usedPct: memUsedPct,
      severity: live ? memorySeverity(pulse, memUsed, memTotal) : "normal",
      series: history.map((item) =>
        isLiveHostLoadPulse(item) && item.mem_used_bytes != null && item.mem_total_bytes && item.mem_total_bytes > 0
          ? (item.mem_used_bytes / item.mem_total_bytes) * 100
          : null,
      ),
      segments: memSegments,
      stats: memStats,
    },
    thermal: {
      available: thermalAvailable,
      state: thermalState,
      stepIndex: thermalState ? THERMAL_STEP[thermalState] : -1,
      primaryText:
        tempNow !== null && Number.isFinite(tempNow)
          ? `${tempNow.toFixed(1)} °C`
          : thermalState !== null
            ? thermalStateText
            : !live
              ? unavailableLabel
              : describeReading(host?.thermal_temp_c, (t) => `${t.toFixed(1)} °C`),
      subtext:
        tempNow !== null
          ? `${thermalStateText} · SoC die`
          : thermalState !== null
            ? "Thermal state only (die sensor unavailable)"
            : !live
              ? inactiveSubtext
              : describeReading(host?.thermal_temp_c, (t) => `${t.toFixed(1)} °C`),
      severity: live ? thermalSeverity(thermalState, tempNow) : "normal",
      series: history.map((item) =>
        isLiveHostLoadPulse(item) ? (item.thermal_temp_c ?? null) : null,
      ),
    },
    storage: {
      available: live && diskFree !== null,
      primaryText:
        diskFree !== null
          ? `${bytes(diskFree)} free`
          : !live
            ? unavailableLabel
            : describeReading(host?.free_disk, bytes),
      subtext:
        diskFree !== null && diskTotal && diskTotal > 0
          ? `${bytes(Math.max(0, diskTotal - diskFree))} used of ${bytes(diskTotal)} (${diskUsedPct}%)`
          : !live
            ? inactiveSubtext
            : host?.total_disk?.status === "value"
              ? `Total ${bytes(host.total_disk.value)}`
              : "Volume total unavailable",
      usedPct: diskUsedPct,
      severity: live ? storageSeverity(pulse, diskFree, diskTotal) : "normal",
      stats: storageStats,
    },
  };
}
