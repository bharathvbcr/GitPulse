<script lang="ts">
  /**
   * The host-wide half of repository hygiene.
   *
   * These are the values every repository inherits, so they live here rather
   * than being retyped in each repository's Storage page. The panel also names
   * the three scopes side by side, because the retention control here and the
   * one in the scheduled cleaner below it used to carry the same words and
   * govern different things.
   */
  import { Clock } from "@lucide/svelte";
  import SettingToggle from "./SettingToggle.svelte";
  import {
    DEFAULT_HYGIENE_DEFAULTS,
    RETENTION_CHOICES,
    readDefaults,
    saveDefaults,
    type HygieneDefaults,
  } from "../storage/hygiene/preferences";

  let defaults = $state<HygieneDefaults>({ ...DEFAULT_HYGIENE_DEFAULTS });
  let unsaved = $state(false);

  function storage() {
    try {
      return localStorage;
    } catch {
      return null;
    }
  }

  function commit(next: HygieneDefaults) {
    defaults = next;
    unsaved = !saveDefaults(storage(), next);
  }

  // Re-read whenever the panel becomes visible: a Storage page in another tab
  // writes the same record, and a stale copy here would overwrite it on edit.
  $effect(() => {
    defaults = readDefaults(storage());
  });
</script>

<section aria-label="Hygiene defaults" class="space-y-3 text-xs">
  <div>
    <h3 class="text-sm font-semibold text-textPrimary">Hygiene defaults</h3>
    <p class="mt-1 text-textMuted">
      What every repository inherits for the previewed cleanup in Insights → Storage. A
      repository can override its retention there; this is the value it starts from.
    </p>
  </div>

  {#if unsaved}
    <p role="alert" class="text-amber-300">
      These defaults could not be saved; they apply only to this visit.
    </p>
  {/if}

  <label class="flex flex-wrap items-center gap-2 text-textSecondary">
    <Clock size={12} /> Default retention · keep output modified within
    <select
      aria-label="Default retention for every repository"
      class="rounded border border-border bg-surface px-2 py-1"
      value={defaults.retentionDays}
      onchange={(event) =>
        commit({ ...defaults, retentionDays: Number(event.currentTarget.value) })}
    >
      {#each RETENTION_CHOICES as days (days)}<option value={days}>{days} days</option>{/each}
    </select>
  </label>

  <SettingToggle
    label="Review shared caches weekly"
    description="Measures the host's shared caches — the Cargo registry, GOCACHE, the npm cache and their peers — at most once a week while a Storage page is open. One switch and one weekly timer for the whole host, because the caches are not owned by any one repository. Measurement only; nothing is removed without a preview you accept."
    ariaLabel="Review shared caches weekly"
    checked={defaults.reviewSharedCaches}
    onchange={(next) => commit({ ...defaults, reviewSharedCaches: next })}
  />

  <div class="rounded-xl border border-border/60 p-3 text-textMuted">
    <p class="font-medium text-textSecondary">Which setting reaches which repository</p>
    <ul class="mt-1.5 list-disc space-y-1 pl-4">
      <li>
        <strong class="text-textSecondary">Default retention</strong> — every repository you
        preview cleanup in, unless that repository overrides it.
      </li>
      <li>
        <strong class="text-textSecondary">A repository's override</strong> — that repository
        alone. Set it in Insights → Storage; it never changes the default or the schedule.
      </li>
      <li>
        <strong class="text-textSecondary">The scheduled cleaner below</strong> — every
        repository under its project roots, including ones not open in GitPulse. It keeps its own
        retention because the schedule runs without this window.
      </li>
    </ul>
  </div>
</section>
