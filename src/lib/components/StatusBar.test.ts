import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { afterEach, beforeAll, describe, expect, it } from "vitest";
import { render } from "svelte/server";
import StatusBar from "./StatusBar.svelte";
import { preloadMountForTests } from "./LazyMount.svelte";
import { loadHostLoadSegment } from "./lazyLoaders";
import { interfaceStore } from "../stores/interfaceStore";
import { hostLoadStatusFixture } from "../../../harness/hostLoadFixture";
import {
  publishHostLoadPulseForTests,
  pulseOf,
  resetHostLoadForTests,
} from "../stores/hostLoadStore";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "StatusBar.svelte"),
  "utf8",
);

const badgeSource = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "HarnessBadge.svelte"),
  "utf8",
);

const bar = () => render(StatusBar).body;

// The host-load strip is loaded, not imported (it is kept out of the entry
// chunk), and a server render never runs the effect that loads it. Preloading
// puts the bar in the state the app reaches once the chunk lands.
beforeAll(() => preloadMountForTests(loadHostLoadSegment));

describe("StatusBar", () => {
  it("loads the host-load strip through LazyMount, only on the repository surface", () => {
    expect(source).not.toMatch(/import HostLoadSegment from/);
    const repoOnly = source.slice(source.indexOf('{#if $interfaceStore.globalSurface === "repository"}'));
    expect(repoOnly).toContain("load={loadHostLoadSegment}");
  });
  it("opens uncommitted files rather than retaining a previous commit diff", () => {
    const changes = source.slice(source.indexOf("<!-- Working Tree Changes"), source.indexOf("<!-- Conflicts Indicator"));
    expect(changes).toContain("repoStore.previewUncommitted()");
  });
  afterEach(() => {
    resetHostLoadForTests();
    interfaceStore.setStatusBarMode("full");
  });

  it("renders status bar role and shortcut indicators", () => {
    const body = bar();
    expect(body).toContain('role="contentinfo"');
    expect(body).toContain('aria-label="Repository status bar"');
    expect(body).toContain("Palette");
    expect(body).toContain("Shortcuts");
  });

  it("keeps the branch but drops the ambient readouts in compact mode", () => {
    interfaceStore.setStatusBarMode("minimal");
    const body = bar();
    expect(body).toContain('aria-label="Repository status bar"');
    expect(body).toContain("HEAD");
    // "Clean" and the shortcut hints say nothing is wrong, which is exactly
    // the noise a compact bar exists to lose.
    expect(body).not.toContain("Clean");
    expect(body).not.toContain("Palette");
    expect(body).not.toContain("Shortcuts");
  });

  it("carries the full Manvi pill, in the minimal bar too, and opens its menu upward", () => {
    for (const mode of ["full", "minimal"] as const) {
      interfaceStore.setStatusBarMode(mode);
      const body = bar();
      expect(body, mode).toContain('data-variant="full"');
      expect(body, mode).toContain("data-manvi-chip");
    }
    expect(source).toContain('<HarnessBadge variant="full" />');
    expect(badgeSource).toContain('place: variant === "full" ? ("above" as const)');
  });

  it("leaves the pill to the title bar off the repository surface, where this bar is zero high but mounted", () => {
    for (const surface of ["fleet", "agents", "tasks"] as const) {
      interfaceStore.setGlobalSurface(surface);
      try {
        expect(bar(), surface).not.toContain("data-manvi-chip");
      } finally {
        interfaceStore.setGlobalSurface("repository");
      }
    }
    expect(bar()).toContain("data-manvi-chip");
  });

  it("drops the pill with the bar when it is hidden", () => {
    interfaceStore.setStatusBarMode("hidden");
    expect(bar()).not.toContain("data-manvi-chip");
  });

  it("tells its host whether it is on screen, and that it is not once it unmounts", () => {
    expect(source).toContain('shown = $bindable(false)');
    expect(source).toContain('shown = detail !== "hidden";');
    expect(source).toMatch(/return \(\) => \{\s*shown = false;\s*\};/);
  });

  it("renders nothing at all when hidden and the repository is quiet", () => {
    interfaceStore.setStatusBarMode("hidden");
    expect(bar()).not.toContain('role="contentinfo"');
  });

  it("sits on the window's ground like the title bar, not as a covering slab", () => {
    // The strip paints no band of its own: macOS clears its fill in
    // `html.macos .gp-glass.gp-status-bar` and elsewhere `bg-background` is the
    // page's own ground. A `bg-surface` fill (bare or with alpha) made it a
    // darker band across the foot of every page. `gp-gpu` is `translateZ(0)`,
    // which promotes a WKWebView layer that no longer composites with the
    // NSVisualEffectView behind it; the title bar has neither.
    const footer = source.match(/<footer[\s\S]*?class="([^"]+)"/)?.[1];
    const tokens = footer?.split(/\s+/) ?? [];
    expect(tokens).toContain("gp-glass");
    expect(tokens).toContain("gp-status-bar");
    expect(tokens).toContain("bg-background");
    expect(tokens).not.toContain("gp-repo-tabs");
    expect(tokens.some((token) => token.startsWith("bg-surface"))).toBe(false);
    expect(tokens).not.toContain("gp-gpu");
  });

  it("feeds all four override signals to the visibility rule", () => {
    // The rule itself is covered in ui/statusBarMode.test.ts. What cannot be
    // reached from a server render — repoStore state arrives from Tauri — is
    // whether the bar actually hands it a parked operation, the conflict
    // count, the watcher state and a silent status poll, so that wiring is pinned here: dropping
    // one would leave a hidden bar silent about it.
    const wiring = source.slice(
      source.indexOf("resolveStatusBarMode($interfaceStore.statusBarMode"),
      source.indexOf("let detail ="),
    );
    expect(wiring).toContain("operationParked: Boolean(operationMarker)");
    expect(wiring).toContain("conflictedCount,");
    expect(wiring).toContain("watchDegraded: Boolean(watchLabel)");
    expect(wiring).toContain("statusUnavailable: statusUnavailableSince !== null");
    expect(wiring).toContain("hostLoadRefusing,");
  });

  it("forces a hidden status bar to minimal only while host load is actively refusing launches", () => {
    interfaceStore.setStatusBarMode("hidden");
    const calm = pulseOf(hostLoadStatusFixture());
    publishHostLoadPulseForTests(calm);
    expect(bar()).not.toContain('role="contentinfo"');

    // Overridden or dead/disabled refusals stay hidden when user asked for hidden.
    publishHostLoadPulseForTests({
      ...calm,
      governor: { ...calm.governor, admit: false, overridden: true },
    });
    expect(bar()).not.toContain('role="contentinfo"');
    publishHostLoadPulseForTests({
      ...calm,
      state: "dead",
      governor: { ...calm.governor, admit: false, overridden: false },
    });
    expect(bar()).not.toContain('role="contentinfo"');

    // Active refusal on a governing sensor pulls the hidden bar back to minimal.
    publishHostLoadPulseForTests({
      ...calm,
      level: "critical",
      pressure: "critical",
      governor: {
        ...calm.governor,
        admit: false,
        overridden: false,
        binding: "memory_pressure",
      },
    });
    const forced = bar();
    expect(forced).toContain('role="contentinfo"');
    expect(forced).toContain("Shown despite the hidden status-bar setting");
    expect(forced).toContain("Memory: critical · launches refused");
  });

  it("shows live CPU/GPU/Memory/Thermal/Storage metrics in the repository bar and alerts in minimal mode", () => {
    publishHostLoadPulseForTests(pulseOf(hostLoadStatusFixture()));
    const full = bar();
    expect(full).toContain('data-testid="status-host-load"');
    expect(full).toContain('data-metric-kind="cpu"');
    expect(full).toContain('data-metric-kind="gpu"');
    expect(full).toContain('data-metric-kind="memory"');
    expect(full).toContain('data-metric-kind="thermal"');
    expect(full).toContain('data-metric-kind="storage"');

    interfaceStore.setStatusBarMode("minimal");
    expect(bar()).not.toContain('data-testid="status-host-load"');

    publishHostLoadPulseForTests({
      ...pulseOf(hostLoadStatusFixture()),
      level: "critical",
      pressure: "critical",
    });
    expect(bar()).toContain('data-testid="status-host-load"');
    expect(bar()).toContain("Memory: critical");

    interfaceStore.setGlobalSurface("fleet");
    try {
      expect(bar()).not.toContain('data-testid="status-host-load"');
    } finally {
      interfaceStore.setGlobalSurface("repository");
    }
  });
});
