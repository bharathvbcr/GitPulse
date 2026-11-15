import { describe, expect, it } from "vitest";
import { hostLoadStatusFixture } from "../../../harness/hostLoadFixture";
import { bindingLabel, governorLine, refusalNotice, statusChip } from "./hostLoad";
import { INPUT_SPECS, MODE_LABELS, liveReading, ruleProblem, specOf } from "./hostLoadInputs";
import { DEFAULT_THRESHOLDS, type Binding, type GovernorPulse, type HostLoadPulse } from "../stores/hostLoadStore";

const GIB = 2 ** 30;

const governor = (over: Partial<GovernorPulse> = {}): GovernorPulse => ({
  admit: true,
  shed_background: false,
  shed_reactive: false,
  binding: null,
  refusing_for_s: null,
  notice: false,
  overridden: false,
  largest_foreign: null,
  ...over,
});

const pulse = (over: Partial<GovernorPulse>): HostLoadPulse => ({
  state: "fresh",
  pressure: "normal",
  level: "normal",
  age_ms: 500,
  enabled: true,
  compressor_thrash: false,
  role: "writer",
  governor: governor(over),
});

describe("the status bar speaks for every binding, not only kernel pressure", () => {
  it("says nothing while nothing is acting", () => {
    expect(statusChip(pulse({}))).toBeNull();
  });

  it("names the resource when launches are refused at normal pressure", () => {
    const chip = statusChip(pulse({ admit: false, shed_background: true, shed_reactive: true, binding: "swap" }));
    expect(chip).toMatchObject({ tone: "danger" });
    expect(chip?.text).toBe("Host load: swap · launches refused");
  });

  it("says it is shedding when only background work is slowed", () => {
    const chip = statusChip(pulse({ shed_background: true, binding: "cpu_load" }));
    expect(chip).toMatchObject({ tone: "warn", text: "Host load: CPU load · shedding" });
  });

  it("stays quiet about a refusal the person overrode and nothing else", () => {
    expect(statusChip(pulse({ admit: false, overridden: true, binding: "low_disk" }))).toBeNull();
  });
});

describe("every binding has words", () => {
  const all: Binding[] = [
    "memory_pressure",
    "compressor_thrash",
    "compressor_segments",
    "swap",
    "low_disk",
    "thermal",
    "cpu_load",
    "battery",
  ];
  it("labels each binding and names it in the refusal notice", () => {
    const causes = new Set<string>();
    for (const binding of all) {
      expect(bindingLabel(binding).length).toBeGreaterThan(0);
      const notice = refusalNotice(governor({ binding, refusing_for_s: 1900, largest_foreign: 2 * GIB }));
      expect(notice).toContain("Refusing agent launches for 31 min under ");
      causes.add(notice);
    }
    expect(causes.size).toBe(all.length - 0);
    expect(refusalNotice(governor({ binding: "swap" }))).toContain("heavy swap use");
    expect(refusalNotice(governor({ binding: "low_disk" }))).toContain("low disk space");
  });

  it("shows the governor's own sentence for a swap refusal", () => {
    const report = hostLoadStatusFixture().governor;
    report.verdict = {
      ...report.verdict,
      admit: false,
      reason: "pressure",
      tier: "severe",
      binding: "swap",
      shed: { background: true, reactive: true },
      message: "swap used is 0.83x physical memory (53.4 GiB of 64.0 GiB), at or above 0.75x: agent launches are refused until it clears",
    };
    expect(governorLine(report)).toContain("swap used is 0.83x physical memory");
  });
});

describe("the input rows", () => {
  it("has one row per governor input, each findable by its own settings id", () => {
    expect(INPUT_SPECS.map((spec) => spec.id)).toEqual(Object.keys(DEFAULT_THRESHOLDS));
    expect(new Set(INPUT_SPECS.map((spec) => spec.settingId)).size).toBe(INPUT_SPECS.length);
    expect(MODE_LABELS).toEqual({
      off: "Off",
      shed: "Slow GitPulse's background work",
      refuse: "Also refuse agent launches",
    });
    expect(specOf("battery").canRefuse).toBe(false);
  });

  it("refuses the same bad thresholds the backend does", () => {
    const swap = specOf("swap");
    expect(ruleProblem(swap, DEFAULT_THRESHOLDS.swap)).toBeNull();
    expect(ruleProblem(swap, { ...DEFAULT_THRESHOLDS.swap, severe_at: 0.4 })).toMatch(/must not be below/);
    expect(ruleProblem(swap, { ...DEFAULT_THRESHOLDS.swap, elevated_at: Number.NaN })).toMatch(/numbers/);
    expect(ruleProblem(swap, { ...DEFAULT_THRESHOLDS.swap, elevated_at: 0 })).toMatch(/above 0/);
    const disk = specOf("disk");
    expect(ruleProblem(disk, DEFAULT_THRESHOLDS.disk)).toBeNull();
    expect(ruleProblem(disk, { ...DEFAULT_THRESHOLDS.disk, severe_at: 30 })).toMatch(/less free space is worse/);
    expect(ruleProblem(specOf("thermal"), { mode: "shed", elevated_at: 0, severe_at: 0 })).toBeNull();
  });

  it("shows the live reading in the row's unit, or why there is none, never a zero", () => {
    const snapshot = hostLoadStatusFixture().snapshot;
    expect(liveReading("swap", snapshot)).toBe("now 0.01x");
    expect(liveReading("disk", snapshot)).toBe("now 180.0 GiB");
    expect(liveReading("load", snapshot)).toBe("now 0.26");
    expect(liveReading("compressor_segments", snapshot)).toBe("now 0.15");
    expect(liveReading("thermal", snapshot)).toBe("now nominal");
    expect(liveReading("battery", snapshot)).toBe("now on mains power");
    expect(liveReading("swap", null)).toBeNull();
    const broken = structuredClone(snapshot)!;
    broken.host.swap_used = { status: "unavailable", value: "unreadable" };
    broken.host.thermal = { status: "unavailable", value: "unsupported" };
    broken.host.compressor_segments = { status: "unavailable", value: "not_yet" };
    expect(liveReading("swap", broken)).toBe("unreadable");
    expect(liveReading("thermal", broken)).toBe("not measured here");
    expect(liveReading("compressor_segments", broken)).toBe("pending");
    broken.host.memsize = { status: "value", value: 0 };
    broken.host.swap_used = { status: "value", value: 5 };
    expect(liveReading("swap", broken)).toBe("unreadable");
  });
});
