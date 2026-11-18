import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, describe, expect, it, vi } from "vitest";
import { render } from "svelte/server";
import { invoke } from "@tauri-apps/api/core";
import HostLoadSegment from "./HostLoadSegment.svelte";
import { stripMarkupComments } from "../dom/markupText";
import { hostLoadStatusFixture } from "../../../harness/hostLoadFixture";
import {
  publishHostLoadPulseForTests,
  pulseOf,
  refreshHostLoad,
  resetHostLoadForTests,
  type HostLoadPulse,
} from "../stores/hostLoadStore";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../platform", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../platform")>();
  return { ...actual, isTauri: () => true };
});

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, "HostLoadSegment.svelte"), "utf8");

let nextSeq = 240;

function samplePulse(overrides: Partial<HostLoadPulse> = {}): HostLoadPulse {
  return {
    ...pulseOf(hostLoadStatusFixture()),
    snapshot_seq: nextSeq++,
    ...overrides,
  };
}

afterEach(() => {
  nextSeq = 240;
  vi.mocked(invoke).mockReset();
  resetHostLoadForTests();
});

describe("HostLoadSegment", () => {
  it("draws nothing until a host-load pulse arrives", () => {
    const visible = stripMarkupComments(render(HostLoadSegment).body).trim();
    expect(visible).toBe("");
  });

  it("renders compact CPU, GPU, Memory, Thermal, and Storage metrics on macOS", () => {
    publishHostLoadPulseForTests(samplePulse());
    const body = render(HostLoadSegment, { props: { detail: "full" } }).body;
    expect(body).toContain('data-testid="status-host-load"');
    expect(body).toContain('data-metric-kind="cpu"');
    expect(body).toContain('data-metric-kind="gpu"');
    expect(body).toContain('data-metric-kind="memory"');
    expect(body).toContain('data-metric-kind="thermal"');
    expect(body).toContain('data-metric-kind="storage"');
    expect(body).toContain("25%");
    expect(body).toContain("12%");
    expect(body).toContain("18G");
    expect(body).toContain("54°");
    expect(body).toContain("180G");
  });

  it("omits unavailable GPU indicators on hosts without GPU telemetry", () => {
    publishHostLoadPulseForTests(samplePulse({ gpu_pct: null, thermal: null, thermal_temp_c: null }));
    const body = render(HostLoadSegment, { props: { detail: "full" } }).body;
    expect(body).toContain('data-metric-kind="cpu"');
    expect(body).toContain('data-metric-kind="memory"');
    expect(body).toContain('data-metric-kind="storage"');
    expect(body).not.toContain('data-metric-kind="gpu"');
    expect(body).not.toContain('data-metric-kind="thermal"');
  });

  it("stays quiet in minimal mode unless an alert is active", () => {
    publishHostLoadPulseForTests(samplePulse());
    expect(
      stripMarkupComments(render(HostLoadSegment, { props: { detail: "minimal" } }).body).trim(),
    ).toBe("");

    publishHostLoadPulseForTests(samplePulse({ level: "critical", pressure: "critical" }));
    const alerting = render(HostLoadSegment, { props: { detail: "minimal" } }).body;
    expect(alerting).toContain('data-testid="status-host-load"');
    expect(alerting).toContain("Memory: critical");
  });

  it("renders live SVG graphs, Activity Monitor breakdown, and Health jump inside the popover", () => {
    publishHostLoadPulseForTests(samplePulse({ cpu_pct: 22, gpu_pct: 10, thermal_temp_c: 55 }));
    publishHostLoadPulseForTests(samplePulse({ cpu_pct: 38, gpu_pct: 18, thermal_temp_c: 59 }));

    const body = render(HostLoadSegment, { props: { detail: "full", open: true } }).body;
    expect(body).toContain('data-testid="host-load-popover"');
    expect(body).toContain('data-testid="host-load-open-health"');
    expect(body).toContain("System load");
    expect(body).toContain("<svg");
    expect(body).toContain("38.0%");
    expect(body).toContain("Peak 38.0%");
    expect(body).toContain("18.0%");
    expect(body).toContain("Peak 18.0%");
    expect(body).toContain("59.0 °C");
    expect(body).toContain("Peak 59.0 °C");
    expect(body).toContain("2 min window");
    expect(body).toContain("sr-only");
    expect(body).toContain("Immediate unallocated space (excludes purgeable caches).");
    expect(body).toContain("Live window covers up to 60 samples");
  });

  it("renders a single memory progress bar on Linux (without Cached-only stacked segments) and hides popover when bar is hidden", async () => {
    const linuxStatus = hostLoadStatusFixture();
    linuxStatus.snapshot!.host.gpu = { status: "unavailable", value: "unsupported" };
    linuxStatus.snapshot!.host.thermal = { status: "unavailable", value: "unsupported" };
    linuxStatus.snapshot!.host.memory = {
      status: "value",
      value: {
        used_bytes: 18 * 2 ** 30,
        app_bytes: null,
        wired_bytes: null,
        compressed_bytes: null,
        cached_bytes: 4 * 2 ** 30,
      },
    };
    vi.mocked(invoke).mockResolvedValueOnce(linuxStatus);
    await refreshHostLoad();

    const openBody = render(HostLoadSegment, { props: { detail: "full", open: true } }).body;
    expect(openBody).toContain('style="width: 50%"');
    expect(openBody).toContain("Cached");
    expect(openBody).not.toContain("App ");

    // Even if `open` was true while switching to calm minimal mode (bar.visible === false),
    // the popover does not render unanchored into the page.
    const hiddenBody = stripMarkupComments(
      render(HostLoadSegment, { props: { detail: "minimal", open: true } }).body,
    ).trim();
    expect(hiddenBody).toBe("");
  });

  it("hides Thermal sparkline Peak and Storage unallocated footnote when sensor goes dead after live samples", () => {
    publishHostLoadPulseForTests(samplePulse({ cpu_pct: 25, gpu_pct: 12, thermal_temp_c: 62.0 }));
    const liveBody = render(HostLoadSegment, { props: { detail: "full", open: true } }).body;
    expect(liveBody).toContain("Peak 62.0 °C");
    expect(liveBody).toContain("Immediate unallocated space (excludes purgeable caches).");

    publishHostLoadPulseForTests(
      samplePulse({
        state: "dead",
        age_ms: 90_000,
        cpu_pct: 99,
        gpu_pct: 99,
        thermal_temp_c: 99,
        disk_free_bytes: 50 * 2 ** 30,
      }),
    );
    const deadBody = render(HostLoadSegment, { props: { detail: "full", open: true } }).body;
    expect(deadBody).not.toContain("Peak 62.0 °C");
    expect(deadBody).not.toContain("99.0");
    expect(deadBody).not.toContain("Immediate unallocated space (excludes purgeable caches).");
    expect(deadBody).not.toContain("2 min window");
    expect(deadBody).toContain("Sampler inactive");
    expect(deadBody).toContain("Launches admitted (sensor inactive)");
  });

  it("wires upward popover positioning, revision tracking, pollHostLoad, dialog focus, exit transition, and Health navigation", () => {
    expect(source).toContain('use:portal={"body"}');
    expect(source).toContain("use:popover={popoverConfig}");
    expect(source).toContain("revision: panelModel");
    expect(source).toContain("out:popoverExit");
    expect(source).toContain('place: "above" as const');
    expect(source).toContain('align: "end" as const');
    expect(source).toContain('repoStore.setActiveTab("insights", "health")');
    expect(source).toContain("pollHostLoad()");
    expect(source).toContain("openHealthBtnEl?.focus()");
    expect(source).toContain("{#if bar.visible && open && panelModel}");
    expect(source).toContain("{#if panelModel.thermal.available && panelModel.thermal.series.some");
  });
});
