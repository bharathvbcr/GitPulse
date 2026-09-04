<script lang="ts">
  // SENSITIVITY CONTROL — deliberately reproduces the defect, so a "no loops
  // found" result from the stress harness can be distinguished from a harness
  // that detects nothing at all. Lives outside src/ on purpose: it must never
  // be scanned by effect-loop-contract or shipped.
  import { locMetric } from "../src/lib/metrics/repoMetrics";
  import { repoStore } from "../src/lib/stores/repoStore";

  let rows = $state<{ path: string; n: number }[]>([]);

  $effect(() => {
    const tabs = $repoStore.openTabs;
    if (tabs.length < 2) return;
    rows = tabs.map((t) => ({ path: t.path, n: 0 }));
    for (const [i, t] of tabs.entries()) {
      locMetric.subscribe(t.path, () => {
        const next = [...rows]; // the defect: tracked read of the written state
        next[i] = { ...next[i], n: (next[i]?.n ?? 0) + 1 };
        rows = next;
      });
    }
  });
</script>

<div>canary rows={rows.length}</div>
