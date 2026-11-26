<script lang="ts">
  import { tick } from "svelte";
  import {
    Battery,
    Cpu,
    Gauge,
    Gpu,
    HardDrive,
    MemoryStick,
    Plug,
    Thermometer,
  } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { bytes } from "../health/hostLoad";
  import {
    bottomBarMetrics,
    popoverDetailModel,
    sparklineGeometry,
    type BottomBarDetail,
    type MetricKind,
    type MetricSeverity,
  } from "../health/hostLoadMetrics";
  import { pollHostLoad } from "../health/hostLoadPoll";
  import { hostRelief, reliefChipInput } from "../health/hostRelief";
  import {
    hostLoadHistory,
    hostLoadPulse,
    hostLoadStatus,
  } from "../stores/hostLoadStore";
  import { hostPlatform } from "../stores/platformStore";
  import { repoStore } from "../stores/repoStore";
  import { LAYERS } from "../ui/layers";
  import { fileManagerName } from "../ui/platformCopy";
  import { popover, popoverExit, restoreFocusTo } from "../ui/popover";

  /**
   * Compact CPU, GPU, Memory, Thermal, and Storage status-bar indicators with
   * an expandable live-graph popover (`hostLoadHistory`, 60 ticks × 2 s).
   */
  let {
    open = $bindable(false),
    detail = "full",
  }: {
    open?: boolean;
    detail?: BottomBarDetail;
  } = $props();

  let triggerEl = $state<HTMLButtonElement | null>(null);
  let openHealthBtnEl = $state<HTMLButtonElement | null>(null);

  let bar = $derived(bottomBarMetrics($hostLoadPulse, detail, reliefChipInput($hostRelief)));
  let panelModel = $derived(
    $hostLoadPulse ? popoverDetailModel($hostLoadPulse, $hostLoadStatus, $hostLoadHistory) : null,
  );

  function openPanel(): void {
    open = true;
    void tick().then(() => openHealthBtnEl?.focus());
  }

  function closePanel(restoreFocus = false): void {
    open = false;
    if (restoreFocus) restoreFocusTo(triggerEl);
  }

  function togglePanel(): void {
    if (open) closePanel(false);
    else openPanel();
  }

  function openHealthView(): void {
    repoStore.setActiveTab("insights", "health");
    closePanel(false);
  }

  $effect(() => {
    if (!bar.visible) open = false;
  });

  $effect(() => {
    if (bar.visible && open) return pollHostLoad();
  });

  function formatPeak(peak: number, unit: "%" | "x" | "°C"): string {
    if (unit === "°C") return `${peak.toFixed(1)} °C`;
    if (unit === "x") return `${peak.toFixed(2)}x`;
    return `${peak.toFixed(1)}%`;
  }

  const popoverConfig = $derived({
    anchor: {
      kind: "element" as const,
      element: triggerEl,
      gap: 6,
      place: "above" as const,
      align: "end" as const,
    },
    dismiss: {
      inside: "[data-host-load-panel], [data-host-load-trigger]",
      escape: "capture" as const,
      resize: true,
    },
    onDismiss: (reason: string) => closePanel(reason === "escape"),
    revision: panelModel
      ? `${panelModel.cpu.available}:${panelModel.cpu.series.some((v) => v !== null)}:${panelModel.gpu.available}:${panelModel.gpu.series.some((v) => v !== null)}:${panelModel.memory.available}:${panelModel.memory.segments.length}:${panelModel.memory.series.some((v) => v !== null)}:${panelModel.thermal.available}:${panelModel.thermal.series.some((v) => v !== null)}:${panelModel.storage.available}:${panelModel.storage.stats.length}:${panelModel.topologyLabel ?? ""}`
      : "",
  });

  function itemToneClass(severity: MetricSeverity): string {
    switch (severity) {
      case "critical":
        return "text-danger";
      case "warn":
        return "text-warn";
      case "normal":
        return "text-textMuted";
    }
  }

  function graphToneClass(severity: MetricSeverity): string {
    switch (severity) {
      case "critical":
        return "text-danger";
      case "warn":
        return "text-warn";
      case "normal":
        return "text-accent";
    }
  }

  function chipToneClass(tone: "warn" | "danger" | "muted"): string {
    if (tone === "danger") {
      return "border-danger/65 bg-danger/10 text-danger hover:bg-danger/20";
    }
    if (tone === "warn") {
      return "border-warn/65 bg-warn/10 text-warn hover:bg-warn/20";
    }
    return "border-edge bg-surfaceHover text-textMuted hover:text-textPrimary";
  }

  const SEGMENT_FILL: Record<"app" | "wired" | "compressed" | "cached", string> = {
    app: "bg-accent",
    wired: "bg-warn",
    compressed: "bg-danger",
    cached: "bg-info",
  };

  const THERMAL_STEPS = ["Nominal", "Fair", "Serious", "Critical"] as const;

  function stepTone(stepIdx: number, activeStep: number): string {
    if (activeStep < 0 || stepIdx > activeStep) {
      return "bg-surface text-faint";
    }
    if (activeStep >= 3) return "bg-danger/15 text-danger font-medium";
    if (activeStep === 2) return "bg-warn/15 text-warn font-medium";
    if (activeStep === 1) return "bg-info/15 text-info font-medium";
    return "bg-ok/15 text-ok font-medium";
  }
</script>

{#snippet metricIcon(kind: MetricKind)}
  {#if kind === "cpu"}
    <Cpu size={11} class="shrink-0" aria-hidden="true" />
  {:else if kind === "gpu"}
    <Gpu size={11} class="shrink-0" aria-hidden="true" />
  {:else if kind === "memory"}
    <MemoryStick size={11} class="shrink-0" aria-hidden="true" />
  {:else if kind === "thermal"}
    <Thermometer size={11} class="shrink-0" aria-hidden="true" />
  {:else}
    <HardDrive size={11} class="shrink-0" aria-hidden="true" />
  {/if}
{/snippet}

{#snippet sparkline(
  series: readonly (number | null)[],
  toneClass: string,
  label: string,
  unit: "%" | "x" | "°C",
  domainMax?: number,
)}
  {@const geom = sparklineGeometry(series, 180, 28, 60, domainMax)}
  <div class="flex flex-col gap-0.5">
    {#if geom.peak !== null}
      <span class="sr-only">
        {label} over the last 2 minutes: peak {formatPeak(geom.peak, unit)}{geom.latest !== null ? `, latest ${formatPeak(geom.latest, unit)}` : ""}.
      </span>
    {/if}
    <svg
      viewBox="0 0 180 28"
      preserveAspectRatio="none"
      class="h-7 w-full overflow-visible {toneClass}"
      aria-hidden="true"
    >
      <line
        x1="0"
        y1="14"
        x2="180"
        y2="14"
        stroke="currentColor"
        stroke-opacity="0.12"
        stroke-dasharray="2 2"
      />
      {#if geom.areaPath}
        <path d={geom.areaPath} fill="currentColor" fill-opacity="0.15" />
      {/if}
      {#if geom.linePath}
        <path
          d={geom.linePath}
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      {/if}
      {#if geom.lastPoint}
        <circle cx={geom.lastPoint.x} cy={geom.lastPoint.y} r="2.2" fill="currentColor" />
      {/if}
    </svg>
    <div class="flex items-center justify-between gap-2 text-micro text-faint tabular-nums">
      <span>2 min window</span>
      {#if geom.peak !== null}
        <span>Peak {formatPeak(geom.peak, unit)}</span>
      {/if}
    </div>
  </div>
{/snippet}

{#if bar.visible}
  <button
    bind:this={triggerEl}
    type="button"
    onclick={togglePanel}
    data-host-load-trigger
    data-testid="status-host-load"
    aria-haspopup="dialog"
    aria-expanded={open}
    aria-label={bar.summaryLabel}
    title={bar.summaryLabel}
    class={bar.items.length === 0 && bar.alertChip
      ? `inline-flex shrink-0 items-center gap-1 whitespace-nowrap rounded border px-1.5 py-0.2 font-medium transition-colors ${chipToneClass(bar.alertChip.tone)}`
      : "inline-flex shrink-0 items-center gap-2 rounded px-1.5 py-0.5 text-micro transition-colors hover:bg-surfaceHover hover:text-textPrimary cursor-pointer"}
  >
    {#if bar.items.length === 0 && bar.alertChip}
      <Gauge size={11} class="shrink-0" aria-hidden="true" />
      <span>{bar.alertChip.text}</span>
    {:else}
      {#if bar.alertChip}
        <span
          class="inline-flex items-center gap-1 rounded px-1 py-0.2 font-medium {bar.alertChip.tone === 'danger' ? 'bg-danger/15 text-danger' : bar.alertChip.tone === 'warn' ? 'bg-warn/15 text-warn' : 'bg-surfaceHover text-textMuted'}"
        >
          <Gauge size={10} class="shrink-0" aria-hidden="true" />
          <span class="hidden min-[1200px]:inline">{bar.alertChip.text}</span>
        </span>
      {/if}
      {#each bar.items as item (item.kind)}
        <span
          class="inline-flex items-center gap-1 tabular-nums {itemToneClass(item.severity)}"
          title={item.title}
          data-metric-kind={item.kind}
        >
          {@render metricIcon(item.kind)}
          <span class="font-medium">{item.label}</span>
        </span>
      {/each}
    {/if}
  </button>
{/if}

{#if bar.visible && open && panelModel}
  <div
    use:portal={"body"}
    use:popover={popoverConfig}
    out:popoverExit
    data-host-load-panel
    data-testid="host-load-popover"
    role="dialog"
    aria-label="Host system metrics"
    class="fixed w-96 max-w-[calc(100vw-1rem)] gp-menu gp-pop p-3 text-xs text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <div class="flex flex-col gap-2">
      <!-- Header -->
      <div class="flex items-center justify-between gap-2">
        <div class="min-w-0">
          <div class="flex items-center gap-1.5">
            <span class="font-semibold text-textPrimary">System load</span>
            <span class="rounded bg-surfaceHover px-1.5 py-0.5 text-micro text-textMuted">
              {panelModel.sensorLabel}
            </span>
          </div>
          <div class="mt-0.5 flex items-center gap-1.5 text-micro text-textMuted">
            {#if panelModel.powerSource === "battery"}
              <Battery size={11} class="shrink-0 text-warn" aria-hidden="true" />
            {:else}
              <Plug size={11} class="shrink-0 text-ok" aria-hidden="true" />
            {/if}
            <span>{panelModel.powerLabel}</span>
            {#if panelModel.topologyLabel}
              <span class="text-faint">·</span>
              <span class="truncate">{panelModel.topologyLabel}</span>
            {/if}
          </div>
        </div>
        <button
          bind:this={openHealthBtnEl}
          type="button"
          onclick={openHealthView}
          class="gp-btn shrink-0"
          data-size="sm"
          data-testid="host-load-open-health"
        >
          Open Health
        </button>
      </div>

      <!-- CPU -->
      <div class="rounded-lg bg-surface p-2 flex flex-col gap-1.5">
        <div class="flex items-center justify-between gap-2">
          <span class="inline-flex items-center gap-1.5 font-medium text-textPrimary">
            <Cpu size={12} class="text-accent shrink-0" aria-hidden="true" />
            <span>{panelModel.cpu.title}</span>
          </span>
          <span class="tabular-nums font-semibold {graphToneClass(panelModel.cpu.severity)}">
            {panelModel.cpu.primaryText}
          </span>
        </div>
        {#if panelModel.cpu.available && panelModel.cpu.series.some((v) => v !== null)}
          <div class="rounded bg-background/60 px-1.5 py-1">
            {@render sparkline(
              panelModel.cpu.series,
              graphToneClass(panelModel.cpu.severity),
              panelModel.cpu.title,
              panelModel.cpu.seriesUnit,
              panelModel.cpu.domainMax,
            )}
          </div>
        {/if}
        <div class="flex items-center justify-between gap-2 text-micro text-textMuted">
          <span>{panelModel.cpu.subtext}</span>
          {#if panelModel.cpu.stats[0]}
            <span class="tabular-nums">{panelModel.cpu.stats[0].value}</span>
          {/if}
        </div>
      </div>

      <!-- GPU -->
      <div class="rounded-lg bg-surface p-2 flex flex-col gap-1.5">
        <div class="flex items-center justify-between gap-2">
          <span class="inline-flex items-center gap-1.5 font-medium text-textPrimary">
            <Gpu size={12} class="text-accent shrink-0" aria-hidden="true" />
            <span>GPU</span>
          </span>
          <span
            class="tabular-nums font-semibold {panelModel.gpu.available ? graphToneClass(panelModel.gpu.severity) : 'text-textMuted'}"
          >
            {panelModel.gpu.primaryText}
          </span>
        </div>
        {#if panelModel.gpu.available && panelModel.gpu.series.some((v) => v !== null)}
          <div class="rounded bg-background/60 px-1.5 py-1">
            {@render sparkline(
              panelModel.gpu.series,
              graphToneClass(panelModel.gpu.severity),
              "GPU utilization",
              "%",
              100,
            )}
          </div>
        {/if}
        <div class="flex flex-wrap items-center justify-between gap-x-3 gap-y-0.5 text-micro text-textMuted">
          <span>{panelModel.gpu.subtext}</span>
          {#if panelModel.gpu.stats.length > 0}
            <span class="tabular-nums">
              {panelModel.gpu.stats.map((s) => `${s.label}: ${s.value}`).join(" · ")}
            </span>
          {/if}
        </div>
      </div>

      <!-- Memory -->
      <div class="rounded-lg bg-surface p-2 flex flex-col gap-1.5">
        <div class="flex items-center justify-between gap-2">
          <span class="inline-flex items-center gap-1.5 font-medium text-textPrimary">
            <MemoryStick size={12} class="text-accent shrink-0" aria-hidden="true" />
            <span>Memory</span>
          </span>
          <span class="tabular-nums font-semibold {graphToneClass(panelModel.memory.severity)}">
            {panelModel.memory.primaryText}
          </span>
        </div>
        {#if panelModel.memory.available && panelModel.memory.series.some((v) => v !== null)}
          <div class="rounded bg-background/60 px-1.5 py-1">
            {@render sparkline(
              panelModel.memory.series,
              graphToneClass(panelModel.memory.severity),
              "Memory used",
              "%",
              100,
            )}
          </div>
        {/if}
        {#if panelModel.memory.segments.length > 0}
          <div class="h-1.5 w-full rounded-full overflow-hidden bg-background flex gap-0.5" aria-hidden="true">
            {#each panelModel.memory.segments as seg (seg.key)}
              <span
                class="h-full {SEGMENT_FILL[seg.key]}"
                style="width: {seg.pctOfTotal}%"
              ></span>
            {/each}
          </div>
          <div class="flex flex-wrap items-center gap-x-2.5 gap-y-0.5 text-micro text-textMuted tabular-nums">
            {#each panelModel.memory.segments as seg (seg.key)}
              <span class="inline-flex items-center gap-1">
                <span class="h-1.5 w-1.5 rounded-full {SEGMENT_FILL[seg.key]}" aria-hidden="true"></span>
                <span>{seg.label} {bytes(seg.bytes)}</span>
              </span>
            {/each}
          </div>
        {:else if panelModel.memory.usedPct !== null}
          <div class="h-1.5 w-full rounded-full overflow-hidden bg-background" aria-hidden="true">
            <div
              class="h-full {panelModel.memory.severity === 'critical' ? 'bg-danger' : panelModel.memory.severity === 'warn' ? 'bg-warn' : 'bg-accent'}"
              style="width: {panelModel.memory.usedPct}%"
            ></div>
          </div>
        {/if}
        <div class="flex flex-wrap items-center justify-between gap-x-2 text-micro text-textMuted">
          <span>{panelModel.memory.subtext}</span>
          <span class="tabular-nums">
            {panelModel.memory.stats.map((s) => `${s.label}: ${s.value}`).join(" · ")}
          </span>
        </div>
      </div>

      <!-- Thermal & Storage -->
      <div class="grid grid-cols-2 gap-2">
        <div class="rounded-lg bg-surface p-2 flex flex-col gap-1.5">
          <div class="flex items-center justify-between gap-1">
            <span class="inline-flex items-center gap-1 font-medium text-textPrimary">
              <Thermometer size={12} class="text-accent shrink-0" aria-hidden="true" />
              <span>Thermal</span>
            </span>
            <span class="tabular-nums font-semibold {graphToneClass(panelModel.thermal.severity)}">
              {panelModel.thermal.primaryText}
            </span>
          </div>
          <div class="grid grid-cols-4 gap-0.5 text-center text-micro">
            {#each THERMAL_STEPS as stepName, idx (stepName)}
              <span class="rounded px-1 py-0.5 truncate {stepTone(idx, panelModel.thermal.stepIndex)}">
                {stepName.slice(0, 4)}
              </span>
            {/each}
          </div>
          {#if panelModel.thermal.available && panelModel.thermal.series.some((v) => v !== null)}
            <div class="rounded bg-background/60 px-1 py-0.5">
              {@render sparkline(
                panelModel.thermal.series,
                graphToneClass(panelModel.thermal.severity),
                "Thermal temperature",
                "°C",
                110,
              )}
            </div>
          {/if}
          <span class="text-micro text-textMuted truncate">{panelModel.thermal.subtext}</span>
        </div>

        <div class="rounded-lg bg-surface p-2 flex flex-col gap-1.5">
          <div class="flex items-center justify-between gap-1">
            <span class="inline-flex items-center gap-1 font-medium text-textPrimary">
              <HardDrive size={12} class="text-accent shrink-0" aria-hidden="true" />
              <span>Storage</span>
            </span>
            <span class="tabular-nums font-semibold {graphToneClass(panelModel.storage.severity)}">
              {panelModel.storage.primaryText}
            </span>
          </div>
          {#if panelModel.storage.usedPct !== null}
            <div class="h-1.5 w-full rounded-full overflow-hidden bg-background" aria-hidden="true">
              <div
                class="h-full {panelModel.storage.severity === 'critical' ? 'bg-danger' : panelModel.storage.severity === 'warn' ? 'bg-warn' : 'bg-ok'}"
                style="width: {panelModel.storage.usedPct}%"
              ></div>
            </div>
          {/if}
          <span class="text-micro text-textMuted">{panelModel.storage.subtext}</span>
          {#if panelModel.storage.available}
            <span
              class="text-micro text-faint"
              title={`Measures immediate unallocated space available to Waylode; ${fileManagerName($hostPlatform.os)} may report more by counting purgeable caches.`}
            >
              Immediate unallocated space (excludes purgeable caches).
            </span>
          {/if}
          {#if panelModel.storage.stats.length > 0}
            <span class="text-micro text-faint tabular-nums">
              {panelModel.storage.stats.map((s) => `${s.label}: ${s.value}`).join(" · ")}
            </span>
          {/if}
        </div>
      </div>

      <p class="text-micro text-faint truncate" title={panelModel.governorLine}>
        {panelModel.governorLine} · Live window covers up to 60 samples
      </p>
    </div>
  </div>
{/if}
