import { describe, expect, it } from "vitest";
import type { HostLoadPulse, MemguardHealth } from "../stores/hostLoadStore";
import {
  bytes,
  describeReading,
  formatAge,
  memguardLine,
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
  ...over,
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
