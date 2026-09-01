<script lang="ts">
  import { branchHealth, needsAttention, type BranchHealthLevel } from "../branches/health";
  import type { BranchInfo } from "../branches/types";

  let {
    branch,
    now = Date.now(),
  }: {
    branch: BranchInfo;
    /** Injected so the indicator is deterministic under test. */
    now?: number;
  } = $props();

  const health = $derived(branchHealth(branch, now));

  const TONE: Record<BranchHealthLevel, string> = {
    healthy: "bg-emerald-500/70",
    info: "bg-sky-500/70",
    warn: "bg-amber-500",
    attention: "bg-rose-500",
  };
</script>

<!-- Only branches a reader might act on get an indicator. Drawing a dot on
     every row would make the healthy majority noisy and the exceptions
     invisible, which is the opposite of an at-a-glance signal. -->
{#if needsAttention(health)}
  <span
    class="w-1.5 h-1.5 rounded-full shrink-0 {TONE[health.level]}"
    role="img"
    aria-label="{health.title}: {health.detail}"
    title="{health.title} — {health.detail}"
  ></span>
{/if}
