import { describe, expect, it } from "vitest";
import { hostLoadStatusFixture } from "../../../harness/hostLoadFixture";
import { pulseOf, type HostLoadPulse } from "../stores/hostLoadStore";
import {
  bottomBarMetrics,
  popoverDetailModel,
  sparklineGeometry,
} from "./hostLoadMetrics";

function calmPulse(overrides: Partial<HostLoadPulse> = {}): HostLoadPulse {
  return {
    state: "fresh",
    pressure: "normal",
    level: "normal",
    age_ms: 400,
    enabled: true,
    compressor_thrash: false,
    role: "writer",
    governor: {
      admit: true,
      shed_background: false,
      shed_reactive: false,
      binding: null,
      refusing_for_s: null,
      notice: false,
      overridden: false,
      largest_foreign: null,
    },
    power: "ac",
    thermal: "nominal",
    load_per_core: 0.42,
    cpu_pct: 24.5,
    gpu_pct: 12.0,
    mem_used_bytes: 18 * 2 ** 30,
    mem_total_bytes: 36 * 2 ** 30,
    swap_used_bytes: 512 * 2 ** 20,
    thermal_temp_c: 54.2,
    disk_free_bytes: 180 * 2 ** 30,
    disk_total_bytes: 926 * 2 ** 30,
    disk_write_rate: 0,
    disk_fill_rate: 0,
    ...overrides,
  };
}

describe("bottomBarMetrics", () => {
  it("hides when no pulse has arrived", () => {
    const model = bottomBarMetrics(null, "full");
    expect(model.visible).toBe(false);
    expect(model.items).toHaveLength(0);
    expect(model.alertChip).toBeNull();
  });

  it("renders all five indicators in full mode on macOS", () => {
    const model = bottomBarMetrics(calmPulse(), "full");
    expect(model.visible).toBe(true);
    expect(model.alertChip).toBeNull();
    expect(model.severity).toBe("normal");
    expect(model.items.map((item) => item.kind)).toEqual([
      "cpu",
      "gpu",
      "memory",
      "thermal",
      "storage",
    ]);
    expect(model.items.map((item) => item.label)).toEqual([
      "25%",
      "12%",
      "18G",
      "54°",
      "180G",
    ]);
  });

  it("omits unavailable metrics without inventing zeros on Linux/partial hosts", () => {
    const model = bottomBarMetrics(
      calmPulse({
        gpu_pct: null,
        thermal: null,
        thermal_temp_c: null,
        cpu_pct: null,
        load_per_core: 1.5,
      }),
      "full",
    );
    expect(model.items.map((item) => item.kind)).toEqual(["cpu", "memory", "storage"]);
    expect(model.items[0].label).toBe("1.5x");
  });

  it("uses compact memory percentage in compact mode", () => {
    const model = bottomBarMetrics(calmPulse(), "compact");
    const mem = model.items.find((item) => item.kind === "memory");
    expect(mem?.label).toBe("50%");
  });

  it("hides calm metrics in minimal mode, but surfaces alert chips", () => {
    expect(bottomBarMetrics(calmPulse(), "minimal").visible).toBe(false);

    const warnPulse = calmPulse({
      level: "critical",
      pressure: "critical",
      governor: {
        admit: false,
        shed_background: true,
        shed_reactive: true,
        binding: "memory_pressure",
        refusing_for_s: 30,
        notice: true,
        overridden: false,
        largest_foreign: null,
      },
    });
    const minimal = bottomBarMetrics(warnPulse, "minimal");
    expect(minimal.visible).toBe(true);
    expect(minimal.severity).toBe("critical");
    expect(minimal.alertChip?.tone).toBe("danger");
    expect(minimal.items).toHaveLength(0);
  });

  it("never shows live scalar numbers when dead, stale_old, or disabled and clarifies unallocated storage", () => {
    const full = bottomBarMetrics(calmPulse(), "full");
    const storageItem = full.items.find((i) => i.kind === "storage");
    expect(storageItem?.title).toContain("immediate unallocated space, excluding purgeable");

    for (const nonLive of [
      calmPulse({ state: "dead" }),
      calmPulse({ state: "stale_old" }),
    ]) {
      const m = bottomBarMetrics(nonLive, "full");
      expect(m.visible).toBe(true);
      expect(m.alertChip?.text).toBe("Load sensor: no sampler");
      expect(m.items).toHaveLength(0);
    }

    const off = bottomBarMetrics(calmPulse({ enabled: false }), "full");
    expect(off.visible).toBe(false);
    expect(off.items).toHaveLength(0);
  });
});

describe("sparklineGeometry", () => {
  it("returns empty paths when all samples are missing", () => {
    const empty = sparklineGeometry([null, undefined, Number.NaN], 120, 32, 60);
    expect(empty).toEqual({
      linePath: "",
      areaPath: "",
      lastPoint: null,
      peak: null,
      latest: null,
    });
  });

  it("right-aligns single sample without fabricating prior points", () => {
    const single = sparklineGeometry([50], 118, 28, 60, 100);
    expect(single.linePath).toBe("M 118 14");
    expect(single.areaPath).toBe("");
    expect(single.lastPoint).toEqual({ x: 118, y: 14 });
    expect(single.peak).toBe(50);
    expect(single.latest).toBe(50);
  });

  it("breaks paths across null gaps instead of bridging missing samples", () => {
    const gapped = sparklineGeometry([10, 20, null, 40, 80], 100, 24, 5, 100);
    // Two separate M segments for [10, 20] at slots 0..1 and [40, 80] at slots 3..4.
    expect(gapped.linePath.match(/\bM\b/g)).toHaveLength(2);
    expect(gapped.areaPath.match(/\bZ\b/g)).toHaveLength(2);
    expect(gapped.peak).toBe(80);
    expect(gapped.latest).toBe(80);
  });
});

describe("popoverDetailModel", () => {
  it("builds full hardware breakdown and live series from snapshot and history", () => {
    const status = hostLoadStatusFixture();
    const p1 = pulseOf(status);
    const p2 = { ...p1, cpu_pct: 48.0, gpu_pct: 22.0, thermal_temp_c: 61.5 };
    const model = popoverDetailModel(p2, status, [p1, p2]);

    expect(model.powerSource).toBe("ac");
    expect(model.topologyLabel).toContain("12 cores");
    expect(model.cpu.available).toBe(true);
    expect(model.cpu.series).toEqual([24.5, 48.0]);
    expect(model.gpu.available).toBe(true);
    expect(model.gpu.stats.map((s) => s.label)).toEqual(["Memory in use", "Allocated"]);
    expect(model.memory.segments.map((seg) => seg.key)).toEqual([
      "app",
      "wired",
      "compressed",
      "cached",
    ]);
    expect(model.thermal.primaryText).toBe("61.5 °C");
    expect(model.thermal.stepIndex).toBe(0);
    expect(model.storage.available).toBe(true);
    expect(model.storage.usedPct).toBeCloseTo(80.6, 1);
  });

  it("reports explicit unavailable reasons when a sensor is unsupported", () => {
    const status = hostLoadStatusFixture();
    status.snapshot!.host.gpu = { status: "unavailable", value: "unsupported" };
    status.snapshot!.host.thermal_temp_c = { status: "unavailable", value: "unsupported" };
    const pulse = calmPulse({ gpu_pct: null, thermal_temp_c: null });
    const model = popoverDetailModel(pulse, status, [pulse]);

    expect(model.gpu.available).toBe(false);
    expect(model.gpu.primaryText).toBe("not measured here");
    expect(model.thermal.primaryText).toBe("Nominal");
    expect(model.thermal.subtext).toContain("unavailable");
  });

  it("omits Linux Cached-only stacked bar segments when app_bytes is null and hides stale snapshot readings when dead", () => {
    const linuxStatus = hostLoadStatusFixture();
    linuxStatus.snapshot!.host.memory = {
      status: "value",
      value: {
        used_bytes: 12 * 2 ** 30,
        app_bytes: null,
        wired_bytes: null,
        compressed_bytes: null,
        cached_bytes: 4 * 2 ** 30,
      },
    };
    const linuxPulse = pulseOf(linuxStatus);
    const linuxModel = popoverDetailModel(linuxPulse, linuxStatus, [linuxPulse]);
    expect(linuxModel.memory.segments).toEqual([]);
    expect(linuxModel.memory.usedPct).toBeCloseTo(33.3, 1);
    expect(linuxModel.memory.subtext).toContain("Cached");

    const staleModel = popoverDetailModel(
      calmPulse({ state: "stale_old", age_ms: 90_000 }),
      linuxStatus,
      [
        linuxPulse,
        calmPulse({
          state: "dead",
          cpu_pct: 91.2,
          gpu_pct: 84.0,
          mem_used_bytes: 28 * 2 ** 30,
          mem_total_bytes: 36 * 2 ** 30,
          thermal_temp_c: 93.0,
        }),
      ],
    );
    expect(staleModel.cpu.available).toBe(false);
    expect(staleModel.gpu.available).toBe(false);
    expect(staleModel.memory.available).toBe(false);
    expect(staleModel.thermal.available).toBe(false);
    expect(staleModel.storage.available).toBe(false);
    expect(staleModel.cpu.primaryText).toContain("stale");
    expect(staleModel.memory.segments).toEqual([]);
    expect(staleModel.governorLine).toBe("Launches admitted (sensor inactive)");
    // Unsanitized non-live pulses in `history` must map to null gaps, never flat-line.
    expect(staleModel.cpu.series).toEqual([linuxPulse.cpu_pct, null]);
    expect(staleModel.gpu.series).toEqual([linuxPulse.gpu_pct, null]);
    expect(staleModel.memory.series[1]).toBeNull();
    expect(staleModel.thermal.series).toEqual([linuxPulse.thermal_temp_c, null]);

    // Linux with `cached_bytes: 0` still displays `Cached 0 B` cleanly rather than dropping 0 as falsy.
    const zeroCachedLinux = hostLoadStatusFixture();
    zeroCachedLinux.snapshot!.host.memory = {
      status: "value",
      value: {
        used_bytes: 12 * 2 ** 30,
        app_bytes: null,
        wired_bytes: null,
        compressed_bytes: null,
        cached_bytes: 0,
      },
    };
    const zeroCachedModel = popoverDetailModel(pulseOf(zeroCachedLinux), zeroCachedLinux, []);
    expect(zeroCachedModel.memory.segments).toEqual([]);
    expect(zeroCachedModel.memory.subtext).toContain("Cached 0 B");

    const disabledModel = popoverDetailModel(
      calmPulse({ enabled: false, state: "fresh" }),
      linuxStatus,
      [],
    );
    expect(disabledModel.governorLine).toBe("Launches admitted (sensor off)");
  });
});
