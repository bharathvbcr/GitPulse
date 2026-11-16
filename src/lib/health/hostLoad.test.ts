import { describe, expect, it } from "vitest";
import type {
  AgentSession,
  GovernorReport,
  HostLoadPulse,
  MemguardHealth,
  Reading,
  Snapshot,
} from "../stores/hostLoadStore";
import {
  FASTEST_WRITER_MIN_RATE,
  admissionLine,
  fastestWriter,
  bytes,
  bindingLabel,
  describeReading,
  diskFillLine,
  fillRateLabel,
  fillRateLine,
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
    shed: { background: false, reactive: false, cause: null },
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
    expect(describeReading(null, bytes)).toBe("couldn't read");
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

  it("shows low disk and cache errors as advice beside an admitted launch, never as a refusal", () => {
    const low = report(
      {},
      {
        advisory: {
          reason: "low_disk",
          cause: "below_floor",
          free_bytes: 1 << 30,
          floor_bytes: 10 * 2 ** 30,
          cap_bytes: null,
          skipped: ["node_modules"],
        },
      },
    );
    expect(admissionLine(low)).toMatch(/^Agent launches are admitted\. Low disk advice: .*node_modules.*launches are not affected\.$/);
    const failed = report({}, { advisory: { reason: "cache_error", message: "node_modules: boom" } });
    expect(admissionLine(failed)).toMatch(/^Agent launches are admitted\. Advice: .*node_modules: boom/);
    expect(admissionLine(report({}, { advisory: null }))).toBe("Agent launches are admitted.");
  });

  it("names an unreadable volume and carries every advisory of one launch", () => {
    const several = report(
      {},
      {
        advisory: {
          reason: "several",
          items: [
            { reason: "disk_unknown" },
            { reason: "cache_error", message: ".turbo: boom" },
          ],
        },
      },
    );
    const line = admissionLine(several);
    expect(line).toMatch(/free space couldn't be read/);
    expect(line).toMatch(/\.turbo: boom/);
  });

  // adversarial W4-L3: the advice must not contradict a pressure refusal.
  it("adv: advice beside a pressure refusal does not claim launches are unaffected", () => {
    const low = {
      reason: "low_disk" as const,
      cause: "below_floor" as const,
      free_bytes: 1 << 30,
      floor_bytes: 10 * 2 ** 30,
      cap_bytes: null,
      skipped: ["node_modules"],
    };
    const refusing = report(
      { admit: false, reason: "pressure", message: "memory pressure critical (kernel level 4): agent launches are refused until it clears" },
      { refusing_for_s: 125, advisory: low },
    );
    expect(admissionLine(refusing)).not.toMatch(/launches are not affected/);
  });

  it("adv: an unreadable volume is named unknown, not omitted or shown as a number", () => {
    const unknown = report(
      {},
      {
        advisory: {
          reason: "low_disk",
          cause: "below_floor",
          free_bytes: null,
          floor_bytes: 10 * 2 ** 30,
          cap_bytes: null,
          skipped: ["node_modules"],
        },
      },
    );
    expect(admissionLine(unknown)).toMatch(/unknown free/);
    expect(admissionLine(unknown)).not.toMatch(/\b0 B\b/);
  });

  it("names the classes shed and the reason, with the stale warning", () => {
    const shedding = report({
      reason: "elevated",
      tier: "elevated",
      shed: { background: true, reactive: false, cause: null },
      binding: "memory_pressure",
      message: "memory pressure warn (kernel level 2): background work is shed, launches are admitted",
      warnings: ["stale 45s: governing on the last-known level"],
    });
    expect(governorLine(shedding)).toBe(
      "Shedding background work: memory pressure warn (kernel level 2): background work is shed, launches are admitted (stale 45s: governing on the last-known level).",
    );
    expect(governorLine(report({ shed: { background: true, reactive: true, cause: null }, reason: "pressure" }))).toMatch(
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

describe("the disk fill line", () => {
  const GIB = 2 ** 30;
  const fill = (over: Partial<Parameters<typeof diskFillLine>[0]> = {}) => ({
    rate_bytes_per_min: 2.7 * GIB,
    eta_min: 10.7,
    free_bytes: 45 * GIB,
    floor_bytes: 16 * GIB,
    tier: "elevated" as const,
    ...over,
  });

  it("says the rate and the minutes until the floor", () => {
    expect(diskFillLine(fill())).toBe("Disk filling: 2.70 GB/min, about 11 min until the 16.0 GB floor.");
    expect(fillRateLabel(0.5 * GIB)).toBe("512 MB/min");
  });

  it("says under a minute, and when the floor is already reached", () => {
    expect(diskFillLine(fill({ eta_min: 0.4 }))).toContain("under 1 min until");
    expect(diskFillLine(fill({ eta_min: 0 }))).toBe("Disk filling: 2.70 GB/min, free space is at the 16.0 GB floor.");
  });

  it("names the binding in the status words", () => {
    expect(bindingLabel("disk_filling")).toBe("disk filling");
    expect(bindingLabel("low_disk")).toBe("low disk");
  });
});

describe("the fill rate line tells not measured from not falling", () => {
  const GIB = 2 ** 30;
  // Only the one field the line reads: the rest of a snapshot is not its business.
  const snapshot = (rate?: Reading<number>) => ({ disk_fill_rate: rate }) as unknown as Snapshot;
  const projection = {
    rate_bytes_per_min: 2.7 * GIB,
    eta_min: 10.7,
    free_bytes: 45 * GIB,
    floor_bytes: 16 * GIB,
    tier: "elevated" as const,
  };

  it("says warming up while the writer has too little history", () => {
    expect(fillRateLine(snapshot({ status: "unavailable", value: "not_yet" }), null)).toBe("Fill rate: warming up");
  });

  it("says the writing app is an older build only for an older record or an absent field", () => {
    const older = "Fill rate: not available (the app writing the snapshot is an older build)";
    expect(fillRateLine(snapshot({ status: "unavailable", value: "older" }), null)).toBe(older);
    expect(fillRateLine(snapshot(undefined), null)).toBe(older);
  });

  it("says a switched-off sensor is off, and an unsupported platform is unsupported", () => {
    expect(fillRateLine(snapshot({ status: "unavailable", value: "disabled" }), null)).toBe(
      "Fill rate: off (host load measurement is switched off)",
    );
    expect(fillRateLine(snapshot({ status: "unavailable", value: "unsupported" }), null)).toBe(
      "Fill rate: not available on this platform",
    );
  });

  it("says the disk can't be read when it can't, and not falling only when it measured that", () => {
    expect(fillRateLine(snapshot({ status: "unavailable", value: "failed" }), null)).toBe(
      "Fill rate: not available (free disk space can't be read)",
    );
    expect(fillRateLine(snapshot({ status: "value", value: 0 }), null)).toBe("Fill rate: not falling");
    expect(fillRateLine(snapshot({ status: "value", value: 0.5 * GIB }), null)).toBe("Fill rate: 512 MB/min");
  });

  it("shows the sustained rate when it binds and the quick line reads lower", () => {
    const both = (quick: number, sustained: Reading<number>) =>
      ({
        disk_fill_rate: { status: "value", value: quick },
        disk_fill_sustained: sustained,
      }) as unknown as Snapshot;
    expect(fillRateLine(both(0, { status: "value", value: 2 * GIB }), null)).toBe(`Fill rate: ${fillRateLabel(2 * GIB)}`);
    expect(fillRateLine(both(3 * GIB, { status: "value", value: 2 * GIB }), null)).toBe(`Fill rate: ${fillRateLabel(3 * GIB)}`);
    expect(fillRateLine(both(0.5 * GIB, { status: "unavailable", value: "not_yet" }), null)).toBe(
      "Fill rate: 512 MB/min",
    );
  });

  it("gives the projection when the disk is heading for the floor, and nothing without a snapshot", () => {
    expect(fillRateLine(snapshot({ status: "value", value: 2.7 * GIB }), projection)).toBe(diskFillLine(projection));
    expect(fillRateLine(null, null)).toBeNull();
  });
});

describe("fastestWriter", () => {
  // humanBytes counts in 1024s and labels them MB.
  const MB = 1_048_576;
  const session = (
    pid: number,
    write_rate: Reading<number>,
    over: Partial<AgentSession> = {},
  ): AgentSession => ({
    kind: "claude",
    pid,
    start_us: 1,
    cwd: `/work/${pid}`,
    tree: null,
    run_id: null,
    parent_pid: null,
    footprint: { status: "value", value: 1 },
    write_rate,
    members: 1,
    unreadable_members: 0,
    ...over,
  });
  const rate = (value: number): Reading<number> => ({ status: "value", value });
  const notYet: Reading<number> = { status: "unavailable", value: "not_yet" };

  it("says nothing without a baseline or without any session", () => {
    expect(fastestWriter([], false)).toBeNull();
    expect(fastestWriter([session(1, notYet), session(2, { status: "unavailable", value: "unsupported" })], false)).toBeNull();
  });

  it("says nothing when no session writes above the threshold, zero included", () => {
    expect(fastestWriter([session(1, rate(0)), session(2, rate(FASTEST_WRITER_MIN_RATE - 1))], false)).toBeNull();
  });

  it("names the fastest session, its kind, pid and directory, as a floor about its processes", () => {
    const found = fastestWriter(
      [session(1, rate(2 * MB)), session(2, rate(85 * MB), { kind: "codex", cwd: "/Users/me/app" }), session(3, notYet)],
      false,
    );
    expect(found?.session.pid).toBe(2);
    expect(found?.rate).toBe(85 * MB);
    expect(found?.line).toBe("Writing fastest: Codex session 2 in /Users/me/app, at least 85.0 MB/s");
    expect(found?.line).not.toMatch(/folder|growing/i);
  });

  it("breaks a tie toward the lower pid whatever the order", () => {
    const a = session(9, rate(5 * MB));
    const b = session(4, rate(5 * MB));
    expect(fastestWriter([a, b], false)?.session.pid).toBe(4);
    expect(fastestWriter([b, a], false)?.session.pid).toBe(4);
  });

  it("limits the claim to the sessions shown when the list was cut", () => {
    const found = fastestWriter([session(7, rate(10 * MB))], true);
    expect(found?.line).toBe("Writing fastest of the sessions shown: Claude session 7 in /work/7, at least 10.0 MB/s");
  });

  it("treats a launched and an observed session alike, and admits an unknown directory", () => {
    const launched = session(10, rate(3 * MB), { tree: "run:r1", run_id: "r1" });
    const observed = session(11, rate(6 * MB), { cwd: null });
    expect(fastestWriter([launched, observed], false)?.line).toBe(
      "Writing fastest: Claude session 11 in an unknown directory, at least 6.00 MB/s",
    );
    expect(fastestWriter([launched], false)?.session.run_id).toBe("r1");
  });
});
