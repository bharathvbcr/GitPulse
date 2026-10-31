<script lang="ts">
  import { AlertCircle } from "@lucide/svelte";
  import { formatRelativeTime } from "../../format";

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
  const age = $derived(measuredAt ? formatRelativeTime(Math.floor(measuredAt / 1000)) : "");
</script>

<!-- A refresh that failed after an earlier success leaves the old figures on
     screen. Without this line they read as current. -->
<div
  role="status"
  class="px-3 py-2 rounded-lg bg-amber-500/10 border border-amber-500/30 text-xs text-amber-500 flex items-start gap-2"
>
  <AlertCircle size={14} class="shrink-0 mt-px" />
  <span class="min-w-0 wrap-break-word"
    >Last refresh failed: {error}. Showing the previous scan{age ? ` from ${age}` : ""}.</span
  >
</div>
