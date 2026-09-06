<script lang="ts">
  /**
   * One cell of the Fleet grid, in whichever of its three states it is in.
   *
   * The reason this is a component rather than three inline branches repeated
   * across nine columns: the moment "not scanned" is rendered by hand in one
   * place, it eventually gets rendered as an em dash in another, and an em
   * dash is indistinguishable from a zero the reader skimmed past. One
   * component means one vocabulary — a value, a hollow "not scanned", or a
   * red "could not read" carrying its reason — across every column.
   */
  import { AlertTriangle } from "lucide-svelte";
  import { formatAge } from "../storage/format";
  import { describeDelta, deltaTone, formatDelta, type DeltaGoal, type DeltaUnit } from "../fleet/format";
  import type { Cell } from "../fleet/types";

  let {
    cell,
    /** What the reader is looking at, for the failure and age tooltips. */
    label,
    /** Why this value may be a floor. Shown only when the cell says partial. */
    partialNote = "This count is a floor: the scan stopped at a budget.",
    align = "right",
    /**
     * Runs this one repository's scan for this one column.
     *
     * Given, the two absent states become buttons. That is the whole point:
     * the place a reader notices a measurement is missing is the cell where it
     * is missing, and making them hunt for the right toolbar sweep — which
     * would rescan all two dozen repositories to fill in one gap — is how a
     * "not scanned" cell stays "not scanned" forever. Omitted for the columns
     * that have no per-repository scan behind them.
     */
    onScan,
    /**
     * True while this repository's scan for this column is actually running.
     *
     * A *queued* repository is not a scanning one — a sweep works two or four
     * at a time — so this comes from the paths the run reports in flight, never
     * from "a sweep is happening somewhere".
     *
     * The marker is strictly additive. A cell that already holds a value keeps
     * showing it while it is rescanned, because last week's measurement is
     * still the last measurement; only the two absent states have nothing to
     * keep and are replaced by "scanning".
     */
    scanning = false,
    /** How this column's units are spelled in a change chip. */
    deltaUnit = "count",
    /** Which direction of change this column treats as an improvement. */
    deltaGoal = "neutral",
    children,
  }: {
    cell: Cell<unknown>;
    label: string;
    partialNote?: string;
    align?: "left" | "right";
    onScan?: () => void;
    scanning?: boolean;
    deltaUnit?: DeltaUnit;
    deltaGoal?: DeltaGoal;
    children?: import("svelte").Snippet;
  } = $props();

  /**
   * The change chip's text, or "" when there is nothing to say.
   *
   * Empty covers two different situations on purpose — no baseline on file,
   * and a baseline the value has not moved from — because neither is something
   * to draw. What matters is that neither renders as a zero.
   */
  const deltaText = $derived(
    cell.kind === "read" && cell.delta ? formatDelta(cell.delta, deltaUnit) : "",
  );

  const now = Date.now();
  const alignClass = $derived(align === "right" ? "text-right justify-end" : "text-left justify-start");
</script>

{#if cell.kind === "read"}
  <div
    class="flex items-baseline gap-1 {alignClass} tabular-nums"
    title={scanning
      ? `${label} — rescanning; the value shown is the last measurement`
      : cell.at !== null
        ? `${label} — scanned ${formatAge(cell.at, now)}`
        : undefined}
    data-testid="fleet-cell"
    data-state="read"
    data-scanning={scanning ? "true" : "false"}
    data-partial={cell.partial ? "true" : "false"}
  >
    <!-- Wrapped so the floor underline spans the whole reading rather than
         only its last fragment; the gap matches the row's own spacing so the
         wrapper is invisible when the value is not a floor. -->
    <span class="inline-flex items-baseline gap-1 {cell.partial ? 'gp-floor' : ''}">
      {@render children?.()}
    </span>
    {#if deltaText && cell.kind === "read" && cell.delta}
      <!-- Only ever drawn when a real earlier measurement exists AND the value
           moved. A first scan has no direction, and "+0" would be a claim
           about a past nobody observed. -->
      <span
        class="text-[10px] tabular-nums {deltaTone(cell.delta, deltaGoal)}"
        title={describeDelta(cell.delta, deltaUnit, label)}
        data-testid="fleet-cell-delta">{deltaText}</span
      >
    {/if}
    {#if scanning}
      <!-- Additive: the existing measurement stays on screen and stays true.
           Blanking it would trade a real, dated reading for nothing. -->
      <span
        class="inline-block h-1 w-1 rounded-full bg-accent animate-pulse shrink-0 motion-reduce:animate-none"
        title="Rescanning this repository now."
        aria-label="{label}: rescanning"
        data-testid="fleet-cell-scanning"
      ></span>
    {/if}
    {#if cell.partial}
      <!-- A floor rendered like a total is the "capped sample presented as
           complete coverage" failure; the marker is what stops it. -->
      <span
        class="text-amber-600 dark:text-amber-400 text-[10px] font-semibold leading-none"
        title={partialNote}
        aria-label="{label}: partial — {partialNote}"
        data-testid="fleet-cell-partial">≥</span
      >
    {/if}
  </div>
{:else if scanning}
  <!-- The one case where the marker replaces rather than adds: an absent cell
       has no measurement to preserve, and "scanning" is more informative than
       "not scanned" for a repository being scanned right now. -->
  <div
    class="flex items-center gap-1.5 {alignClass} text-accent text-[11px]"
    title="{label} is being scanned for this repository now."
    data-testid="fleet-cell"
    data-state="scanning"
  >
    <span
      class="inline-block h-1 w-1 rounded-full bg-accent animate-pulse shrink-0 motion-reduce:animate-none"
      aria-hidden="true"
    ></span>
    <span>scanning</span>
  </div>
{:else if cell.kind === "unscanned"}
  <!-- Still exactly one vocabulary for an absence. The button wrapper changes
       what a click does, never what the cell says: "not scanned" reads the
       same whether or not there is a scan to launch behind it. -->
  <svelte:element
    this={onScan ? "button" : "div"}
    role={onScan ? "button" : undefined}
    type={onScan ? "button" : undefined}
    onclick={onScan
      ? (event: MouseEvent) => {
          event.stopPropagation();
          onScan();
        }
      : undefined}
    class="flex items-center gap-1 {alignClass} text-textMuted/70 text-[11px] italic w-full {onScan
      ? 'hover:text-accent hover:not-italic focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 rounded'
      : ''}"
    title={onScan
      ? `${label} has not been scanned for this repository. Click to scan just this one.`
      : `${label} has not been scanned for this repository.`}
    data-testid="fleet-cell"
    data-state="unscanned"
  >
    not scanned
  </svelte:element>
{:else}
  <svelte:element
    this={onScan ? "button" : "div"}
    role={onScan ? "button" : undefined}
    type={onScan ? "button" : undefined}
    onclick={onScan
      ? (event: MouseEvent) => {
          event.stopPropagation();
          onScan();
        }
      : undefined}
    class="flex items-center gap-1 {alignClass} text-rose-600 dark:text-rose-400 text-[11px] w-full {onScan
      ? 'hover:text-rose-500 focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 rounded'
      : ''}"
    title={onScan
      ? `${label} could not be read: ${cell.reason}. Click to try this repository again.`
      : `${label} could not be read: ${cell.reason}`}
    data-testid="fleet-cell"
    data-state="failed"
  >
    <AlertTriangle size={11} class="shrink-0" />
    <span class="truncate">could not read</span>
  </svelte:element>
{/if}
