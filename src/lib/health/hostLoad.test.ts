import { describe, expect, it } from "vitest";
import type { GovernorReport, HostLoadPulse, MemguardHealth } from "../stores/hostLoadStore";
import {
  admissionLine,
  bytes,
  describeReading,
  formatAge,
  governorLine,
  memguardLine,
  refusalNotice,
  sensorStateLabel,
  statusChip,
  valueOf,
} from "./hostLoad";

const pulse = (over: Partial<HostLoadPulse> = {}): HostLoadPulse => ({
  state: "fresh",
  pressure: "normal",
  level: "normal",
  age_ms: 1_000,
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
  ...over,
});

const report = (over: Partial<GovernorReport["verdict"]> = {}, rest: Partial<GovernorReport> = {}): GovernorReport => ({
  verdict: {
    admit: true,
    shed: { background: false, reactive: false },
    reason: "healthy",
    tier: "clear",
    binding: null,
    sensor: "fresh",
    age_ms: 1_000,
    message: "memory pressure normal (kernel level 1)",
    warnings: [],
    ...over,
  },
  refusing_for_s: null,
  notice: false,
  largest_foreign: null,
  override_remaining_s: null,
  ...rest,
});

describe("host-load wording", () => {
  it("never shows a reading the OS refused as zero", () => {
    expect(describeReading({ status: "value", value: 0 }, bytes)).toBe("0 B");
    expect(describeReading({ status: "unavailable", value: "unreadable" }, bytes)).toBe("unreadable");
    expect(describeReading({ status: "unavailable", value: "not_yet" }, bytes)).toBe("pending");
    expect(describeReading({ status: "unavailable", value: "unsupported" }, bytes)).toBe("not measured here");
    expect(describeReading(null, bytes)).toBe("could not read");
    expect(valueOf({ status: "unavailable", value: "failed" })).toBeNull();
    expect(valueOf({ status: "value", value: 7 })).toBe(7);
  });

  it("names the sensor state and its age", () => {
    expect(sensorStateLabel("fresh", 1_000)).toBe("live");
    expect(sensorStateLabel("stale_recent", 45_000)).toBe("stale 45 s");
    expect(sensorStateLabel("stale_old", 300_000)).toBe("stale 5 min");
    expect(sensorStateLabel("dead", null)).toBe("no sampler");
    expect(formatAge(7_200_000)).toBe("2 h");
  });
});

describe("the status-bar chip", () => {
  it("stays quiet while the machine is fine, switched off, or has no sensor", () => {
    expect(statusChip(null)).toBeNull();
    expect(statusChip(pulse())).toBeNull();
    expect(statusChip(pulse({ enabled: false, level: "critical", pressure: "unavailable" }))).toBeNull();
    expect(statusChip(pulse({ role: "unsupported", state: "dead" }))).toBeNull();
    expect(statusChip(pulse({ role: "not_sampling", state: "dead" }))).toBeNull();
  });

  it("speaks up for pressure, with the danger tone for critical and the age when stale", () => {
    expect(statusChip(pulse({ level: "warn", pressure: "warn" }))).toMatchObject({
      text: "Memory: warning",
      tone: "warn",
    });
    expect(
      statusChip(pulse({ level: "critical", pressure: "critical", compressor_thrash: true })),
    ).toMatchObject({ text: "Memory: critical · thrashing", tone: "danger" });
    expect(
      statusChip(pulse({ state: "stale_recent", age_ms: 30_000, level: "warn", pressure: "warn" }))?.text,
    ).toBe("Memory: warning · stale 30 s");
    expect(statusChip(pulse({ compressor_thrash: true }))?.text).toBe("Memory: compressor thrashing");
  });

  it("says when the sampler stopped, rather than showing a stale level as current", () => {
    const stopped = statusChip(pulse({ state: "dead", age_ms: null, level: "critical", pressure: "unavailable" }));
    expect(stopped).toMatchObject({ text: "Load sensor: no sampler", tone: "muted" });
    expect(stopped?.title).toMatch(/unknown/);
    const old = statusChip(pulse({ state: "stale_old", age_ms: 400_000, level: "warn", pressure: "unavailable" }));
    expect(old?.text).toBe("Load sensor: no sampler");
    expect(old?.title).toMatch(/7 min ago/);
    // A dead sampler is reported even while switched off: "off" still writes
    // a heartbeat, so dead means the app is not running a sampler at all.
    expect(statusChip(pulse({ state: "dead", enabled: false, age_ms: null }))?.text).toBe(
      "Load sensor: no sampler",
    );
  });
});

describe("memguard line", () => {
  const healthy: MemguardHealth = {
    present: true,
    pid: 68214,
    start_us: 1,
    exec: "/Users/me/.local/bin/memguard",
    flags: ["-cap-gib", "32"],
    log_path: { status: "value", value: "/Users/me/Library/Logs/memguard/memguard.log" },
    heartbeat_age_s: { status: "value", value: 120 },
    heartbeat_interval_s: 3_600,
    warn: false,
    warnings: [],
  };

  it("names the pid, the log and how long since it last wrote", () => {
    expect(memguardLine(healthy)).toBe(
      "memguard running (pid 68214) · log /Users/me/Library/Logs/memguard/memguard.log · last written 2 min ago",
    );
    expect(memguardLine({ ...healthy, log_path: { status: "unavailable", value: "unreadable" } })).toContain(
      "log unreadable",
    );
    expect(memguardLine({ ...healthy, present: false })).toBe("memguard is not running");
  });
});

describe("the governor's lines", () => {
  it("says nothing is shed while the machine is fine", () => {
    expect(governorLine(report())).toBe("Nothing is shed. Memory pressure normal (kernel level 1).");
    expect(admissionLine(report())).toBe("Agent launches are admitted.");
  });

  it("names the classes shed and the reason, with the stale warning", () => {
    const shedding = report({
      reason: "elevated",
      tier: "elevated",
      shed: { background: true, reactive: false },
      binding: "memory_pressure",
      message: "memory pressure warn (kernel level 2): background work is shed, launches are admitted",
      warnings: ["stale 45s: governing on the last-known level"],
    });
    expect(governorLine(shedding)).toBe(
      "Shedding background work: memory pressure warn (kernel level 2): background work is shed, launches are admitted (stale 45s: governing on the last-known level).",
    );
    expect(governorLine(report({ shed: { background: true, reactive: true }, reason: "pressure" }))).toMatch(
      /^Shedding background and reactive work/,
    );
  });

  it("says a dead or stale-old sensor means no extra shedding, rather than looking like governing", () => {
    const blind = report({
      reason: "sensor_unavailable",
      sensor: "dead",
      message: "the load sensor is not running: memory pressure is unknown, so nothing is shed or refused",
      warnings: ["the load sensor is not running"],
    });
    expect(governorLine(blind)).toMatch(/^Ungoverned: the load sensor is not running/);
    expect(admissionLine(blind)).toMatch(/^Agent launches are admitted \(the load sensor is not running/);
    expect(governorLine(report({ reason: "switched_off", message: "host-load measurement is switched off" }))).toBe(
      "Not governing: host-load measurement is switched off.",
    );
  });

  it("reports refusal with its duration, and a person's override", () => {
    const refusing = report({ admit: false, reason: "pressure", message: "memory pressure critical (kernel level 4): agent launches are refused until it clears" }, { refusing_for_s: 125 });
    expect(admissionLine(refusing)).toBe(
      "Agent launches are refused for 2 min: memory pressure critical (kernel level 4): agent launches are refused until it clears.",
    );
    expect(admissionLine({ ...refusing, override_remaining_s: 90 })).toBe(
      "Launches are admitted for 90 s more, on your say-so.",
    );
  });

  it("words the 30-minute notice with the binding resource and the largest foreign footprint", () => {
    const gov = pulse().governor;
    expect(refusalNotice({ ...gov, binding: "compressor_thrash", refusing_for_s: 2_700, largest_foreign: 3 * 1024 ** 3 })).toBe(
      "Refusing agent launches for 45 min under a thrashing memory compressor; largest foreign footprint 3.00 GB.",
    );
    expect(refusalNotice({ ...gov, binding: "memory_pressure", refusing_for_s: null, largest_foreign: null })).toBe(
      "Refusing agent launches for 30 min under memory pressure; largest foreign footprint unknown.",
    );
  });

  it("adds refused launches and shedding to the status chip, refused first", () => {
    const gov = pulse().governor;
    expect(
      statusChip(pulse({ level: "critical", pressure: "critical", governor: { ...gov, admit: false, shed_background: true, shed_reactive: true } }))?.text,
    ).toBe("Memory: critical · launches refused");
    expect(
      statusChip(pulse({ level: "warn", pressure: "warn", governor: { ...gov, shed_background: true } }))?.text,
    ).toBe("Memory: warning · shedding");
    // A person's override stops the chip claiming launches are refused.
    expect(
      statusChip(pulse({ level: "critical", pressure: "critical", governor: { ...gov, admit: false, overridden: true, shed_background: true, shed_reactive: true } }))?.text,
    ).toBe("Memory: critical · shedding");
  });
});
