<script lang="ts">
  /**
   * Host load in Health: the sampler's readings for this machine, the agent
   * sessions running on it, and what is using memory
   * (`docs/LOAD_GOVERNOR.md`).
   *
   * Machine-wide, not per repository, which the section's summary says. It
   * reads the status while it is on screen, every few seconds; the sampler
   * itself runs whether or not anyone is looking.
   *
   * The one action here is Terminate on a foreign orphan, and it takes two
   * clicks. The backend checks again, on the live process, that it is still
   * the same orphan before it signals anything.
   */
  import { onDestroy, onMount } from "svelte";
  import { keyedList } from "../../ui/eachKeys";
  import HealthTable from "./HealthTable.svelte";
  import {
    hostLoadStatus,
    refreshHostLoad,
    terminateOrphan,
    type Orphan,
  } from "../../stores/hostLoadStore";
  import {
    bytes,
    bytesPerSecond,
    describeReading,
    formatAge,
    levelLabel,
    memguardLine,
    sensorStateLabel,
  } from "../../health/hostLoad";
  import { explainError } from "../../workbench/client";

  const REFRESH_MS = 4_000;
  const SESSION_COLUMNS = Object.freeze([
    { label: "Agent" },
    { label: "PID" },
    { label: "Directory" },
    { label: "Started by" },
    { label: "Memory" },
  ]);
  const TREE_COLUMNS = Object.freeze([
    { label: "Launch" },
    { label: "Processes" },
    { label: "Memory" },
    { label: "Disk writes" },
  ]);
  const FOREIGN_COLUMNS = Object.freeze([
    { label: "Process" },
    { label: "PID" },
    { label: "Memory" },
  ]);
  const ORPHAN_COLUMNS = Object.freeze([
    { label: "Process" },
    { label: "Checkout" },
    { label: "Running" },
    { label: "CPU" },
    { label: "Memory" },
    { label: "Action", hideLabel: true, width: "w-32" },
  ]);

  let error = $state("");
  let confirming = $state<number | null>(null);
  let terminating = $state(false);
  let outcome = $state("");
  let timer: ReturnType<typeof setInterval> | null = null;

  const status = $derived($hostLoadStatus);
  const snap = $derived(status?.snapshot ?? null);
  const host = $derived(snap?.host ?? null);
  const procs = $derived(status?.procs ?? null);

  async function refresh() {
    try {
      await refreshHostLoad();
      error = "";
    } catch (cause) {
      error = explainError(cause);
    }
  }

  onMount(() => {
    void refresh();
    timer = setInterval(() => void refresh(), REFRESH_MS);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  async function terminate(orphan: Orphan) {
    if (confirming !== orphan.pid) {
      confirming = orphan.pid;
      return;
    }
    terminating = true;
    outcome = "";
    try {
      outcome = await terminateOrphan(orphan.pid, orphan.start_us);
    } catch (cause) {
      outcome = explainError(cause);
    } finally {
      terminating = false;
      confirming = null;
    }
  }

  const PLATFORM_LABEL = {
    governed: "macOS: measured and governed",
    advise: "Linux: measured through PSI, advice only (not verified on a real Linux host)",
    unavailable: "This platform has no host-load sensor",
  } as const;

  function roleLine(): string {
    if (!status) return "";
    const writer = status.writer;
    switch (status.role) {
      case "writer":
        return `This GitPulse samples (pid ${writer?.pid ?? "?"}, ${writer?.path ?? "unknown path"})`;
      case "reader":
        return writer
          ? `Another GitPulse samples: pid ${writer.pid}, ${writer.path} (version ${writer.version})`
          : "Another GitPulse holds the sampler; it has not written yet";
      case "not_sampling":
        return "No sampler runs in this process";
      case "unsupported":
        return "No sampler on this platform";
    }
  }
</script>

<div class="space-y-3 text-[11px]" data-testid="host-load">
  {#if error}
    <p class="text-amber-300">Host load could not be read: {error}</p>
  {/if}
  {#if !status}
    <p class="text-textMuted">Reading host load…</p>
  {:else}
    <div class="rounded-2xl border border-border/70 bg-surface p-3 shadow-card space-y-1">
      <p class="text-textPrimary font-medium" data-testid="host-load-sensor">
        Memory pressure {levelLabel(status.sensor.level)} · sensor {sensorStateLabel(status.sensor.state, status.sensor.age_ms)}
        {#if status.sensor.enabled === false}
          · switched off in Settings
        {/if}
      </p>
      {#if status.sensor.reason}
        <p class="text-amber-300">{status.sensor.reason}</p>
      {/if}
      <p class="text-textMuted">{PLATFORM_LABEL[status.platform]}</p>
      <p class="text-textMuted font-mono" data-testid="host-load-writer">{roleLine()}</p>
      {#if status.snapshot_path}
        <p class="text-textMuted font-mono">{status.snapshot_path}</p>
      {/if}
      <p class="text-textMuted">
        Replay-verified; no real memory-pressure event has been observed by this sensor yet.
      </p>
    </div>

    {#if host && snap}
      <dl class="grid grid-cols-2 md:grid-cols-4 gap-2" data-testid="host-load-readings">
        <div><dt class="text-textMuted">Swap used</dt><dd class="text-textPrimary">{describeReading(host.swap_used, bytes)} of {describeReading(host.swap_total, bytes)}</dd></div>
        <div><dt class="text-textMuted">Compressor</dt><dd class="text-textPrimary">{describeReading(host.compressed_bytes, bytes)}{snap.compressor_thrash ? " · thrashing" : ""}</dd></div>
        <div><dt class="text-textMuted">Load (1 / 5 / 15 min)</dt><dd class="text-textPrimary font-mono">{describeReading(host.load, (l) => l.map((v) => v.toFixed(2)).join(" / "))}</dd></div>
        <div><dt class="text-textMuted">CPUs</dt><dd class="text-textPrimary">{describeReading(host.cores, (c) => c.levels > 0 ? `${c.logical} (${c.perf0} + ${c.perf1} by level)` : `${c.logical}`)}</dd></div>
        <div><dt class="text-textMuted">Open files</dt><dd class="text-textPrimary">{describeReading(host.open_files, String)} of {describeReading(host.max_files, String)}</dd></div>
        <div><dt class="text-textMuted">Free disk</dt><dd class="text-textPrimary">{describeReading(host.free_disk, bytes)}</dd></div>
        <div><dt class="text-textMuted">Thermal · power</dt><dd class="text-textPrimary">{describeReading(host.thermal, String)} · {describeReading(host.power, (p) => p === "ac" ? "on power" : "on battery")}</dd></div>
        <div><dt class="text-textMuted">Processes</dt><dd class="text-textPrimary">{describeReading(snap.processes, String)}{snap.unreadable_processes > 0 ? ` (${snap.unreadable_processes} unreadable)` : ""}</dd></div>
        <div><dt class="text-textMuted">Launched by GitPulse</dt><dd class="text-textPrimary">{describeReading(snap.launched_footprint, bytes)} · writing {describeReading(snap.launched_write_rate, bytesPerSecond)}</dd></div>
        <div><dt class="text-textMuted">Other agent sessions</dt><dd class="text-textPrimary">{describeReading(snap.observed_footprint, bytes)}</dd></div>
        <div><dt class="text-textMuted">Claude sessions</dt><dd class="text-textPrimary">{describeReading(snap.claude, (c) => `${c.launched} launched · ${c.observed} other`)}</dd></div>
        <div><dt class="text-textMuted">Codex sessions</dt><dd class="text-textPrimary">{describeReading(snap.codex, (c) => `${c.launched} launched · ${c.observed} other`)}</dd></div>
      </dl>
    {/if}

    {#if procs}
      {#if !procs.ok}
        <p class="text-amber-300">The process table could not be read{procs.error ? `: ${procs.error}` : ""}. Sessions and memory use below are unknown, not zero.</p>
      {/if}

      <p class="{procs.memguard.warn ? 'text-amber-300' : 'text-textMuted'}" data-testid="host-load-memguard">
        {memguardLine(procs.memguard)}
      </p>
      {#each procs.memguard.warnings as warning (warning)}
        <p class="text-amber-300">{warning}</p>
      {/each}

      {#if procs.sessions.length > 0}
        <HealthTable caption="Agent sessions running on this machine" columns={SESSION_COLUMNS}>
          {#each keyedList(procs.sessions, (s) => `${s.pid}:${s.start_us}`) as { item: session, key } (key)}
            <tr class="border-t border-border/40">
              <td class="px-3 py-1.5 text-textPrimary">{session.kind}</td>
              <td class="px-3 py-1.5 font-mono text-textMuted">{session.pid}</td>
              <td class="px-3 py-1.5 font-mono text-textMuted break-all">{session.cwd ?? "unknown"}</td>
              <td class="px-3 py-1.5 text-textMuted">{session.tree ? (session.run_id ? `task ${session.run_id}` : "GitPulse terminal") : "elsewhere"}</td>
              <td class="px-3 py-1.5 text-textMuted">{describeReading(session.footprint, bytes)}{session.unreadable_members > 0 ? ` (+${session.unreadable_members} unreadable)` : ""}</td>
            </tr>
          {/each}
        </HealthTable>
        {#if procs.sessions_truncated}
          <p class="text-amber-300">More sessions are running than one scan describes; the counts above are a floor.</p>
        {/if}
      {/if}

      {#if procs.trees.length > 0}
        <HealthTable caption="Process trees GitPulse launched" columns={TREE_COLUMNS}>
          {#each keyedList(procs.trees, (t) => t.tree) as { item: tree, key } (key)}
            <tr class="border-t border-border/40">
              <td class="px-3 py-1.5 font-mono text-textPrimary">{tree.run_id ? `task ${tree.run_id}` : (tree.session_id ?? tree.tree)}</td>
              <td class="px-3 py-1.5 text-textMuted">{tree.members}</td>
              <td class="px-3 py-1.5 text-textMuted">{describeReading(tree.footprint, bytes)}{tree.unreadable_members > 0 ? ` (+${tree.unreadable_members} unreadable)` : ""}</td>
              <td class="px-3 py-1.5 text-textMuted">{describeReading(tree.write_rate, bytesPerSecond)}</td>
            </tr>
          {/each}
        </HealthTable>
      {/if}

      {#if procs.foreign_top.length > 0}
        <HealthTable caption="The heaviest processes GitPulse did not start" columns={FOREIGN_COLUMNS}>
          {#each keyedList(procs.foreign_top, (p) => String(p.pid)) as { item: proc, key } (key)}
            <tr class="border-t border-border/40">
              <td class="px-3 py-1.5 text-textPrimary">{proc.name}{proc.session ? ` (${proc.session} session)` : ""}</td>
              <td class="px-3 py-1.5 font-mono text-textMuted">{proc.pid}</td>
              <td class="px-3 py-1.5 text-textMuted">{bytes(proc.footprint)}</td>
            </tr>
          {/each}
        </HealthTable>
        <p class="text-textMuted">
          Memory a virtual machine or a browser takes shows up in the pressure level about 6 s after it is
          allocated, so a sudden jump can appear here before the level moves. Other users' processes cannot be
          measured and are not listed ({procs.other_users} of them).
        </p>
      {/if}

      {#if procs.orphans.length > 0}
        <HealthTable caption="Processes that look orphaned" columns={ORPHAN_COLUMNS}>
          {#each keyedList(procs.orphans, (o) => `${o.pid}:${o.start_us}`) as { item: orphan, key } (key)}
            <tr class="border-t border-border/40">
              <td class="px-3 py-1.5 text-textPrimary" title={orphan.exec ?? ""}>{orphan.name} <span class="font-mono text-textMuted">{orphan.pid}</span></td>
              <td class="px-3 py-1.5 font-mono text-textMuted break-all">{orphan.checkout}</td>
              <td class="px-3 py-1.5 text-textMuted">{formatAge(orphan.age_s * 1000)}</td>
              <td class="px-3 py-1.5 text-textMuted">{Math.round(orphan.cpu_ratio * 100)}%</td>
              <td class="px-3 py-1.5 text-textMuted">{describeReading(orphan.footprint, bytes)}</td>
              <td class="px-3 py-1.5">
                <button
                  type="button"
                  class="gp-btn py-0.5! px-2! text-[10px]! {confirming === orphan.pid ? 'text-rose-400' : ''}"
                  disabled={terminating}
                  onclick={() => void terminate(orphan)}
                  data-testid="host-load-terminate"
                >
                  {confirming === orphan.pid ? "Confirm terminate" : "Terminate"}
                </button>
              </td>
            </tr>
          {/each}
        </HealthTable>
        <p class="text-textMuted">
          Each has no parent left, works in a git checkout, belongs to no agent session or GitPulse launch, and has
          kept a CPU busy for most of its life. GitPulse only lists them; Terminate sends SIGTERM after checking again
          that it is still the same process.
        </p>
      {/if}
      {#if outcome}
        <p class="text-textMuted" data-testid="host-load-terminate-outcome">{outcome}</p>
      {/if}
    {/if}

    <p class="text-textMuted" data-testid="host-load-sampler">
      Sampler: {status.tick.ticks} ticks · {status.tick.overruns} over 2 s · {status.tick.missed} missed ·
      last {(status.tick.last_us / 1000).toFixed(1)} ms · slowest {(status.tick.max_us / 1000).toFixed(1)} ms ·
      CPU {(status.tick.cpu_ns / 1e9).toFixed(1)} s
    </p>
    {#each status.problems as problem (problem)}
      <p class="text-amber-300">{problem}</p>
    {/each}
  {/if}
</div>
