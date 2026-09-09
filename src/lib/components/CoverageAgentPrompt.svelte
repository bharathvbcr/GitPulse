<script lang="ts">
  import { onDestroy } from "svelte";
  import { Check, Clipboard, LoaderCircle, Terminal, RefreshCw } from "@lucide/svelte";
  import { formatCoverageAgentPrompt, type CoverageExclusionNotice, type CoverageAgentFocus } from "../coverage/report";
  import type { CoverageReport } from "../coverage/types";
  import { copyText } from "../desktop/clipboard";
  import { interfaceStore } from "../stores/interfaceStore";
  import { terminalLaunchRequests, type PromptLauncher } from "../terminal/launchRequests";
  import { formatError } from "../ui/formatError";

  let { repoPath, report, exclusions = [], scanFailed = false, focus, selectedScope = $bindable(false), onRescan, scanning = false }: {
    repoPath: string | null;
    report: CoverageReport | null;
    exclusions?: readonly CoverageExclusionNotice[];
    scanFailed?: boolean;
    focus?: CoverageAgentFocus;
    selectedScope?: boolean;
    onRescan: () => void;
    scanning?: boolean;
  } = $props();

  const prompt = $derived(formatCoverageAgentPrompt(report, repoPath ?? "Open a repository first", exclusions, scanFailed, selectedScope ? focus : undefined));
  const agentSessions = terminalLaunchRequests.sessions;
  const recentSession = $derived($agentSessions.filter(session => session.repoPath === repoPath).at(-1));
  let launcher = $state<PromptLauncher>("claude");
  const actionLabel = $derived(selectedScope && focus ? "Improve this file" : report && report.overall.lines_found > 0 ? "Improve coverage" : "Generate coverage");
  let copied = $state(false);
  let copying = $state(false);
  let launching = $state(false);
  let previewOpen = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let launchController: AbortController | undefined;
  let destroyed = false;

  $effect(() => {
    // Feedback always refers to the snapshot currently shown in the preview.
    void prompt;
    copied = false;
    error = null;
    clearTimeout(copyTimer);
  });

  onDestroy(() => {
    destroyed = true;
    clearTimeout(copyTimer);
    launchController?.abort();
  });

  async function copyPrompt() {
    if (!repoPath || copying) return;
    const text = prompt;
    copying = true;
    copied = false;
    error = null;
    const ok = await copyText(text);
    if (destroyed) return;
    copying = false;
    if (prompt !== text) return;
    if (ok) {
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => { copied = false; }, 1800);
    } else {
      error = "Could not copy. Select and copy the prompt from the preview.";
      previewOpen = true;
    }
  }

  async function runAgent(launcher: PromptLauncher) {
    if (!repoPath || launching) return;
    launching = true;
    error = null;
    notice = null;
    launchController = new AbortController();
    try {
      const opened = terminalLaunchRequests.request(repoPath, launcher, prompt, launchController.signal);
      interfaceStore.setTerminalDockOpen(true);
      await opened;
      if (!destroyed) notice = "Agent session opened in the terminal. Follow progress below, then Rescan coverage.";
    } catch (err: unknown) {
      if (!destroyed) error = formatError(err);
    } finally {
      if (!destroyed) launching = false;
    }
  }
</script>

<section aria-label="Generate coverage with a coding agent" class="shrink-0 border-b border-border/60 bg-surface/40 px-4 py-2 font-sans max-h-[45%] overflow-y-auto">
  <div class="flex flex-wrap items-center justify-between gap-x-5 gap-y-2">
    <div class="min-w-0">
      <p class="font-medium text-textPrimary">{actionLabel} with your coding agent</p>
      <p class="mt-0.5 text-[11px] text-textMuted">Run in GitPulse’s terminal, or copy to an existing session.</p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <select aria-label="Coverage coding agent" bind:value={launcher} class="rounded border border-border bg-background px-2 py-1 text-[11px] text-textPrimary">
        <option value="claude">Claude Code</option><option value="codex">Codex</option>
      </select>
      <button type="button" class="gp-btn-primary py-1! px-2.5! text-[11px]!" aria-label={`Run in ${launcher === "claude" ? "Claude Code" : "Codex"}`} disabled={!repoPath || launching} onclick={() => void runAgent(launcher)}>
        {#if launching}<LoaderCircle size={12} class="animate-spin" />{:else}<Terminal size={12} />{/if} {actionLabel}
      </button>
      <button type="button" class="gp-btn py-1! px-2.5! text-[11px]!" disabled={!repoPath || copying} onclick={() => void copyPrompt()} aria-label={copied ? "Agent prompt copied" : "Copy agent prompt"}>
        {#if copying}<LoaderCircle size={12} class="animate-spin" />{:else if copied}<Check size={12} />{:else}<Clipboard size={12} />{/if}
        {copied ? "Copied" : "Copy agent prompt"}
      </button>
    </div>
  </div>
  {#if focus}
    <label class="mt-2 flex items-center gap-2 text-[11px] text-textMuted min-w-0">
      <input type="checkbox" bind:checked={selectedScope} />
      <span class="truncate" title={focus.file.path}>Focus on {focus.file.path}</span>
    </label>
  {/if}
  {#if recentSession}
    <div class="mt-2 flex flex-wrap items-center gap-2 text-[11px]">
      <span class="text-textMuted">{recentSession.launcher === "claude" ? "Claude Code" : "Codex"} session: {recentSession.status}</span>
      <button type="button" class="gp-btn py-0.5! px-2!" onclick={() => { interfaceStore.setTerminalDockOpen(true); recentSession?.reveal(); }}>View agent session</button>
      <button type="button" class="gp-btn py-0.5! px-2!" disabled={scanning} onclick={onRescan}><RefreshCw size={11} /> Rescan results</button>
      <span class="text-textMuted">Session status does not verify coverage. Rescan after the report is written.</span>
    </div>
  {/if}
  <details class="mt-2 text-[11px]" bind:open={previewOpen}>
    <summary class="cursor-pointer text-textMuted hover:text-textPrimary w-fit">Preview prompt</summary>
    <p class="my-2 text-textMuted">Includes this checkout’s path, accepted report locations and coverage snapshot. The agent runs tests using its configured permissions; artifacts appear after Rescan.</p>
    <textarea aria-label="Coverage agent prompt" readonly value={prompt} spellcheck={false} class="w-full h-48 resize-y rounded border border-border bg-background p-3 font-mono text-[11px] leading-relaxed text-textPrimary focus:outline-accent"></textarea>
  </details>
  {#if !repoPath}<p class="mt-2 text-[11px] text-textMuted">Open a repository to generate its coverage prompt.</p>{/if}
  {#if launching}<p role="status" class="mt-2 text-[11px] text-textMuted">Opening agent terminal…</p>{/if}
  {#if copied}<p role="status" class="sr-only">Agent prompt copied</p>{/if}
  {#if error}<p role="alert" class="mt-2 text-[11px] text-rose-400">{error}</p>{/if}
  {#if notice && !recentSession}<p role="status" class="mt-2 text-[11px] text-textMuted">{notice}</p>{/if}
</section>
