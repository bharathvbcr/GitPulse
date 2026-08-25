<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { invoke } from "@tauri-apps/api/core";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import {
    ShieldAlert,
    RefreshCw,
    ExternalLink,
    Package,
    AlertTriangle,
    Clipboard,
    LoaderCircle,
    Sparkles,
  } from "lucide-svelte";
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { harnessStore, type AiGeneration } from "../stores/harnessStore";
  import { copyText } from "../desktop/clipboard";
  import { formatHealthReport } from "../health/report";
  import type {
    DepsHealthReport,
    Vulnerability,
    DependabotReport,
  } from "../health/types";
  import {
    formatAuditCounts,
    issueClass,
    severityClass,
    updateKind,
    updateKindClass,
  } from "../health/format";
  import { formatError } from "../ui/formatError";

  let report = $state<DepsHealthReport | null>(null);
  let dependabot = $state<DependabotReport | null>(null);
  let loading = $state(false);
  let errorMsg = $state<string | null>(null);
  let filter = $state<"all" | "direct">("all");
  let copied = $state(false);
  let fixing = $state(false);
  let plan = $state<AiGeneration | null>(null);
  let planError = $state<string | null>(null);
  let planCopied = $state(false);

  let visibleVulns = $derived.by(() => {
    const current = report;
    if (!current) return [] as Vulnerability[];
    if (filter === "direct") {
      return current.vulnerabilities.filter((v) => v.is_direct);
    }
    return current.vulnerabilities;
  });

  /** Open Dependabot alerts, for the header badge. */
  let openDependabotCount = $derived(
    dependabot?.available ? dependabot.alerts.length : 0,
  );
  let dependabotBadgeClass = $derived.by(() => {
    const current = dependabot;
    if (!current?.available || current.alerts.length === 0) return "";
    const worst = current.alerts.some(
      (a) => a.severity === "critical" || a.severity === "high",
    );
    return worst ? "text-rose-300" : "";
  });

  const scanned = { path: "" };
  let inflight: AsyncGuard | null = null;
  let fixInflight: AsyncGuard | null = null;

  async function scan(path?: string) {
    const repoPath = path ?? $repoStore.currentPath;
    if (!repoPath) return;
    inflight?.cancel();
    const guard = createAsyncGuard();
    inflight = guard;
    loading = true;
    errorMsg = null;
    // Both sources run together so the Health page fills in as one picture,
    // and each settles independently: a failed Dependabot fetch must not
    // erase a finished local scan, or the reverse.
    const [deps, alerts] = await Promise.allSettled([
      invoke<DepsHealthReport>("cmd_scan_deps_health", { repoPath }),
      invoke<DependabotReport>("cmd_github_dependabot_alerts", { repoPath }),
    ]);
    if (!guard.isLive()) return;
    if (deps.status === "fulfilled") {
      report = deps.value;
      // A new scan supersedes whatever a plan was written against.
      plan = null;
      planError = null;
    } else {
      errorMsg = formatError(deps.reason);
      report = null;
      // A failed scan must not mark the repo as scanned, or the effect above
      // would refuse to rescan it after something changes.
      scanned.path = "";
    }
    // The Dependabot command reports its own unavailable/error states; only
    // an IPC-level failure is folded into that same shape here, so "could
    // not check" never renders as a clean bill of health.
    dependabot =
      alerts.status === "fulfilled"
        ? alerts.value
        : {
            available: false,
            cli_present: false,
            is_github_remote: true,
            slug: "",
            alerts: [],
            truncated: false,
            error: formatError(alerts.reason),
          };
    if (guard.isLive()) loading = false;
  }

  /** The rendered text behind both "Copy report" and "Fix with MANVI". */
  function renderedReport(): string | null {
    const current = report;
    const repoPath = $repoStore.currentPath;
    if (!current || !repoPath) return null;
    return formatHealthReport(current, repoPath);
  }

  let copyTimer: number | null = null;
  let planCopyTimer: number | null = null;

  async function copyReport() {
    const text = renderedReport();
    if (!text) return;
    if (await copyText(text)) {
      copied = true;
      if (copyTimer !== null) window.clearTimeout(copyTimer);
      copyTimer = window.setTimeout(() => (copied = false), 1500);
    }
  }

  /**
   * Sends the health report through the harness's local-AI plane for a
   * remediation plan. Advisory only — GitPulse applies nothing by itself.
   */
  async function fixWithManvi() {
    const text = renderedReport();
    if (!text || fixing) return;
    fixInflight?.cancel();
    const guard = createAsyncGuard();
    fixInflight = guard;
    fixing = true;
    planError = null;
    plan = null;
    try {
      const next = await harnessStore.fixHealth($repoStore.currentPath!, text);
      if (!guard.isLive()) return;
      plan = next;
    } catch (err) {
      if (!guard.isLive()) return;
      planError = formatError(err);
    } finally {
      if (guard.isLive()) fixing = false;
    }
  }

  async function copyPlan() {
    if (!plan?.text) return;
    if (await copyText(plan.text)) {
      planCopied = true;
      if (planCopyTimer !== null) window.clearTimeout(planCopyTimer);
      planCopyTimer = window.setTimeout(() => (planCopied = false), 1500);
    }
  }

  let aiReady = $derived($harnessStore.ai?.ready ?? false);

  $effect(() => {
    return () => {
      inflight?.cancel();
      fixInflight?.cancel();
      if (copyTimer !== null) window.clearTimeout(copyTimer);
      if (planCopyTimer !== null) window.clearTimeout(planCopyTimer);
    };
  });

  $effect(() => {
    const path = $repoStore.currentPath;
    if (!path) {
      inflight?.cancel();
      fixInflight?.cancel();
      scanned.path = "";
      report = null;
      dependabot = null;
      errorMsg = null;
      loading = false;
      plan = null;
      planError = null;
      return;
    }
    if (path === scanned.path) return;
    scanned.path = path;
    void scan(path);
  });

  async function openExternal(url: string) {
    // No window.open fallback: inside a Tauri webview it can navigate the
    // app shell itself, and these URLs come from advisory/GitHub payloads.
    // If the opener plugin fails, surfacing the failure beats handing the
    // webview to an arbitrary URL.
    try {
      await openUrl(url);
    } catch (err) {
      console.error("openUrl failed for", url, err);
    }
  }
</script>

<div class="flex-1 flex flex-col bg-background h-full text-xs font-sans overflow-hidden">
  {#snippet dependabotSection()}
    {#if dependabot && (dependabot.available || dependabot.error)}
      <section class="space-y-2">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
          GitHub Dependabot{dependabot.available
            ? ` (${dependabot.alerts.length})`
            : ""}
        </h3>
        {#if !dependabot.cli_present}
          <p class="text-textMuted max-w-2xl">
            Install the <span class="font-mono">gh</span> CLI and run
            <span class="font-mono">gh auth login</span> to fetch Dependabot alerts for
            {dependabot.slug || "this repository"}.
          </p>
        {:else if dependabot.error}
          <div class="p-3 rounded-xl border border-amber-500/30 bg-amber-500/10 text-amber-200 max-w-2xl">
            Could not fetch Dependabot alerts: {dependabot.error}
          </div>
        {:else if dependabot.alerts.length === 0}
          <p class="text-textMuted">No open Dependabot alerts on {dependabot.slug}.</p>
        {:else}
          {#if dependabot.truncated}
            <p class="text-amber-300">Showing the first {dependabot.alerts.length} alerts.</p>
          {/if}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Severity</th>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Advisory</th>
                  <th class="px-3 py-2 font-medium">Fix</th>
                  <th class="px-3 py-2 font-medium w-8"></th>
                </tr>
              </thead>
              <tbody>
                {#each dependabot.alerts as alert}
                  <tr class="border-t border-border/40 align-top">
                    <td class="px-3 py-1.5">
                      <span class="px-1.5 py-0.5 rounded-full text-[10px] uppercase font-semibold {severityClass(alert.severity)}">{alert.severity || "unranked"}</span>
                    </td>
                    <td class="px-3 py-1.5">
                      <div class="font-mono text-textPrimary">{alert.package}</div>
                      <div class="text-[10px] text-textMuted">
                        {alert.ecosystem}{alert.scope ? ` · ${alert.scope}` : ""}
                        {#if alert.manifest_path} · {alert.manifest_path}{/if}
                        {#if alert.vulnerable_range} · {alert.vulnerable_range}{/if}
                      </div>
                    </td>
                    <td class="px-3 py-1.5 text-textPrimary">
                      {alert.title}
                      {#if alert.advisory_id || alert.cve_id}
                        <div class="text-[10px] font-mono text-textMuted">
                          {[alert.advisory_id, alert.cve_id].filter(Boolean).join(" · ")}
                        </div>
                      {/if}
                    </td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{alert.first_patched || "no fix yet"}</td>
                    <td class="px-2 py-1.5">
                      {#if alert.url}
                        <button
                          type="button"
                          class="p-1 rounded-full hover:bg-surfaceHover text-textMuted hover:text-accent transition-colors"
                          title="Open alert on GitHub"
                          onclick={() => openExternal(alert.url)}
                        >
                          <ExternalLink size={13} />
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {/if}
  {/snippet}

  <div class="px-4 py-2 border-b border-border/60 bg-surface/60 flex items-center justify-between shrink-0">
    <div class="flex items-center gap-2 min-w-0">
      <ShieldAlert size={16} class="text-accent shrink-0" />
      <span class="font-semibold text-textPrimary">Health</span>
      {#if report}
        <span class="text-textMuted truncate">
          {formatAuditCounts(report.audit)}
          {#if report.outdated.length > 0}
            · {report.outdated.length} outdated
          {/if}
        </span>
      {/if}
      {#if dependabotBadgeClass}
        <span class={`truncate ${dependabotBadgeClass}`}>
          · Dependabot {openDependabotCount}
        </span>
      {/if}
    </div>
    <div class="flex items-center gap-2">
      {#if report}
        <span class="text-[11px] text-textMuted font-mono">
          {report.node_version ? `node ${report.node_version}` : "node —"}
          ·
          {report.npm_version ? `npm ${report.npm_version}` : "npm —"}
        </span>
        <button
          type="button"
          onclick={copyReport}
          class="gp-btn"
          title="Copy the full health report as text"
        >
          <Clipboard size={13} />
          {copied ? "Copied" : "Copy report"}
        </button>
        <button
          type="button"
          onclick={fixWithManvi}
          disabled={fixing}
          class="gp-btn-primary"
          title={aiReady
            ? "Ask the local model (via the MANVI harness) for a remediation plan"
            : "Needs a local model server — see the MANVI view. The exact error will be reported if none is running."}
        >
          {#if fixing}
            <LoaderCircle size={13} class="animate-spin" />
            Planning…
          {:else}
            <Sparkles size={13} />
            Fix with MANVI
          {/if}
        </button>
      {/if}
      <button
        type="button"
        onclick={() => scan()}
        disabled={loading}
        class="gp-btn disabled:opacity-40 disabled:cursor-not-allowed"
        title="Rescan vulnerabilities and updates"
      >
        <RefreshCw size={13} class={loading ? "animate-spin" : ""} />
        Scan
      </button>
    </div>
  </div>

  <div class="flex-1 overflow-auto p-4 space-y-5">
    {#if loading && !report}
      <div class="text-textMuted">Scanning lockfiles, querying advisories, and fetching Dependabot alerts…</div>
    {:else if errorMsg}
      <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 max-w-2xl">
        {errorMsg}
      </div>
      {@render dependabotSection()}
    {:else if report}
      {#if planError}
        <div class="p-3 rounded-xl border border-rose-500/30 bg-rose-500/10 text-rose-200 max-w-3xl">
          Fix with MANVI failed: {planError}
        </div>
      {/if}

      {#if plan || fixing}
        <section class="space-y-2 max-w-4xl rounded-2xl border border-accent/30 bg-surface shadow-card p-4">
          <div class="flex items-center justify-between gap-2">
            <h3 class="flex items-center gap-1.5 text-[10px] font-bold uppercase tracking-wider text-textMuted">
              <Sparkles size={11} class="text-accent" />
              MANVI remediation plan
            </h3>
            {#if plan}
              <button type="button" onclick={copyPlan} class="gp-btn !py-1 !text-[11px]" title="Copy the remediation plan">
                <Clipboard size={12} />
                {planCopied ? "Copied" : "Copy plan"}
              </button>
            {/if}
          </div>
          {#if fixing && !plan}
            <div class="flex items-center gap-2 text-textMuted py-2">
              <LoaderCircle size={14} class="animate-spin" />
              Sending the health report to the local model…
            </div>
          {:else if plan}
            <p class="text-[11px] text-textMuted font-mono truncate">
              {plan.model} @ {plan.base_url} · {plan.elapsed_ms} ms
            </p>
            {#each plan.warnings as warning}
              <div class="text-amber-400 leading-relaxed">{warning}</div>
            {/each}
            <!-- whitespace-pre-wrap keeps the model's numbered steps intact. -->
            <div class="whitespace-pre-wrap leading-relaxed text-textSecondary">{plan.text}</div>
            <p class="text-textMuted">Advisory only — nothing is applied automatically.</p>
          {/if}
        </section>
      {/if}

      {#if report.truncated}
        <div class="text-amber-300">Scan was capped; some findings may be omitted.</div>
      {/if}

      {#if report.issues.length > 0}
        <section class="space-y-1.5 max-w-3xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Issues</h3>
          {#each report.issues as issue}
            <div class="px-3 py-2 rounded-xl border {issueClass(issue.severity)}">
              <div class="flex items-center gap-2">
                <AlertTriangle size={12} class="shrink-0" />
                <span class="font-medium uppercase text-[10px]">{issue.severity}</span>
                <span class="font-mono text-[10px] opacity-70">{issue.code}</span>
                {#if issue.path}
                  <span class="font-mono truncate opacity-70">{issue.path}</span>
                {/if}
              </div>
              <p class="mt-1 leading-relaxed">{issue.message}</p>
            </div>
          {/each}
        </section>
      {/if}

      <section class="space-y-2">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Packages</h3>
        {#if report.manifests.length === 0}
          <p class="text-textMuted">No package.json found. Other ecosystems are listed below when detected.</p>
        {:else}
          <div class="grid gap-2 md:grid-cols-2 max-w-4xl">
            {#each report.manifests as pkg}
              <div class="p-3.5 rounded-2xl border border-border/70 bg-surface shadow-card">
                <div class="flex items-center gap-2 text-textPrimary font-medium">
                  <Package size={13} class="text-accent shrink-0" />
                  <span class="truncate">{pkg.name || pkg.path}</span>
                  {#if pkg.version}
                    <span class="font-mono text-textMuted font-normal">{pkg.version}</span>
                  {/if}
                  {#if pkg.private}
                    <span class="text-[10px] px-1.5 py-0.5 rounded-full bg-surfaceHover text-textMuted">private</span>
                  {/if}
                </div>
                <div class="mt-1.5 text-[11px] text-textMuted font-mono space-y-0.5">
                  <div>{pkg.path} · {pkg.package_manager}{pkg.lockfile ? ` · ${pkg.lockfile}` : ""}</div>
                  <div>
                    {pkg.dep_count} deps · {pkg.dev_dep_count} dev
                    {#if pkg.has_workspaces} · workspaces{/if}
                    {#if pkg.license} · {pkg.license}{/if}
                  </div>
                  {#if pkg.engines_node}
                    <div>engines.node {pkg.engines_node}</div>
                  {/if}
                  {#if pkg.lifecycle_scripts.length > 0}
                    <div class="text-amber-300">scripts: {pkg.lifecycle_scripts.join(", ")}</div>
                  {/if}
                </div>
              </div>
            {/each}
          </div>
        {/if}
        {#if report.ecosystems.length > 0}
          <div class="space-y-1 max-w-3xl pt-1">
            {#each report.ecosystems as eco}
              <div class="text-textMuted">
                <span class="text-textPrimary font-medium">{eco.family}</span>
                <span class="mx-1.5">·</span>
                {eco.note}
                <span class="font-mono text-[10px] ml-1.5 opacity-70">{eco.manifests.slice(0, 4).join(", ")}</span>
              </div>
            {/each}
          </div>
        {/if}
      </section>

      <section class="space-y-2">
        <div class="flex items-center justify-between max-w-5xl">
          <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
            Vulnerabilities ({visibleVulns.length}{filter === "direct" ? " direct" : ""})
          </h3>
          <div class="gp-segmented">
            <button
              type="button"
              data-active={filter === "all" ? "true" : "false"}
              class="gp-seg-btn !text-[11px] !py-0.5"
              onclick={() => (filter = "all")}
            >All</button>
            <button
              type="button"
              data-active={filter === "direct" ? "true" : "false"}
              class="gp-seg-btn !text-[11px] !py-0.5"
              onclick={() => (filter = "direct")}
            >Direct</button>
          </div>
        </div>
        {#if !report.npm_cli_present && report.manifests.length > 0}
          <p class="text-textMuted max-w-2xl">Install npm on PATH to run <span class="font-mono">npm audit</span> against this lockfile. GitPulse does not apply <span class="font-mono">npm audit fix</span>.</p>
        {:else if visibleVulns.length === 0}
          <p class="text-textMuted">
            {report.audit.total === 0 ? "No vulnerabilities reported." : "No direct dependencies are vulnerable."}
          </p>
        {:else}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Severity</th>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Advisory</th>
                  <th class="px-3 py-2 font-medium">Fix</th>
                  <th class="px-3 py-2 font-medium w-8"></th>
                </tr>
              </thead>
              <tbody>
                {#each visibleVulns as vuln}
                  <tr class="border-t border-border/40 align-top">
                    <td class="px-3 py-1.5">
                      <span class="px-1.5 py-0.5 rounded-full text-[10px] uppercase font-semibold {severityClass(vuln.severity)}">{vuln.severity}</span>
                    </td>
                    <td class="px-3 py-1.5">
                      <div class="font-mono text-textPrimary">{vuln.name}</div>
                      <div class="text-[10px] text-textMuted">
                        {vuln.ecosystem}{vuln.is_direct ? " · direct" : " · transitive"}
                        {#if vuln.range} · {vuln.range}{/if}
                      </div>
                    </td>
                    <td class="px-3 py-1.5 text-textPrimary">{vuln.title}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{vuln.fix_available}</td>
                    <td class="px-2 py-1.5">
                      {#if vuln.url}
                        <button
                          type="button"
                          class="p-1 rounded-full hover:bg-surfaceHover text-textMuted hover:text-accent transition-colors"
                          title="Open advisory"
                          onclick={() => openExternal(vuln.url)}
                        >
                          <ExternalLink size={13} />
                        </button>
                      {/if}
                    </td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>

      {@render dependabotSection()}

      <section class="space-y-2 pb-4">
        <h3 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">
          Outdated ({report.outdated.length})
        </h3>
        {#if report.outdated.length === 0}
          <p class="text-textMuted">
            {report.npm_cli_present ? "No outdated npm packages reported." : "Outdated checks need npm on PATH."}
          </p>
        {:else}
          <div class="border border-border/70 rounded-2xl overflow-hidden max-w-5xl shadow-card">
            <table class="w-full text-left">
              <thead class="bg-surface text-[10px] uppercase text-textMuted">
                <tr>
                  <th class="px-3 py-2 font-medium">Package</th>
                  <th class="px-3 py-2 font-medium">Current</th>
                  <th class="px-3 py-2 font-medium">Wanted</th>
                  <th class="px-3 py-2 font-medium">Latest</th>
                  <th class="px-3 py-2 font-medium">Type</th>
                </tr>
              </thead>
              <tbody>
                {#each report.outdated as pkg}
                  {@const kind = updateKind(pkg.current, pkg.latest)}
                  <tr class="border-t border-border/40">
                    <td class="px-3 py-1.5 font-mono text-textPrimary">{pkg.name}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{pkg.current}</td>
                    <td class="px-3 py-1.5 font-mono text-textMuted">{pkg.wanted}</td>
                    <td class="px-3 py-1.5 font-mono {updateKindClass(kind)}">
                      {pkg.latest}
                      <span class="text-[10px] ml-1 uppercase">{kind}</span>
                    </td>
                    <td class="px-3 py-1.5 text-textMuted">{pkg.dep_type || "—"}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </section>
    {:else}
      <div class="text-textMuted">Open a repository to scan dependency health.</div>
    {/if}
  </div>
</div>
