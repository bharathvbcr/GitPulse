<script lang="ts">
  import { AlertCircle } from "@lucide/svelte";
  import { formatRelativeTime, withoutTrailingPeriod } from "../../format";
  import { describeFailure } from "../../ui/copy/describeFailure";

  let {
    error,
    measuredAt = null,
  }: {
    /** Why the latest refresh failed. */
    error: string;
    /** When the figures on screen were measured (epoch milliseconds). */
    measuredAt?: number | null;
  } = $props();

  // `formatRelativeTime` takes unix SECONDS; the store stamps in milliseconds.
  // The age is fixed when the notice appears, so "just now" would stay on
  // screen long after the scan stopped being recent; say "moments ago" instead.
  const age = $derived.by(() => {
    const relative = measuredAt ? formatRelativeTime(Math.floor(measuredAt / 1000)) : "";
    return relative === "just now" ? "moments ago" : relative;
  });

  // The headline comes from the action alone; the backend text is the reason.
  const headline = describeFailure({ verb: "refresh", object: "the scan" }, "").headline;
</script>

<!-- A refresh that failed after an earlier success leaves the old figures on
     screen. Without this line they read as current. -->
<div class="gp-notice" data-tone="warn" role="status">
  <AlertCircle size={14} class="shrink-0 mt-px" style="color: var(--gp-notice-ink)" />
  <span class="min-w-0 wrap-break-word"
    >{headline} {withoutTrailingPeriod(error)}. Showing the previous scan{age ? ` from ${age}` : ""}.</span
  >
</div>
