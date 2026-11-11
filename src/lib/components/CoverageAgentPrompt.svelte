<script lang="ts">
  import { onDestroy } from "svelte";
  import { Check, Clipboard, LoaderCircle, Terminal, RefreshCw } from "@lucide/svelte";
  import { formatCoverageAgentPrompt, type CoverageExclusionNotice, type CoverageAgentFocus } from "../coverage/report";
  import type { CoverageReport } from "../coverage/types";
  import { copyText } from "../desktop/clipboard";
  import { repoStore } from "../stores/repoStore";
  import { PROMPT_LAUNCHERS, terminalLaunchRequests, type PromptLauncher } from "../terminal/launchRequests";
  import { PROVIDER_LABELS } from "../workbench/taskHandoff";
  import { describeFailure, type FailureCopy } from "../ui/copy/describeFailure";
  import { isCaseInsensitiveFs, sameRepo } from "../repos/paths";
  import Checkbox from "./Checkbox.svelte";

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
  // One checkout, however it is spelled: the same rule the tab strip and the
  // terminal registry match on.
  const pathOptions = { caseInsensitive: isCaseInsensitiveFs() };
  const recentSession = $derived(repoPath
    ? $agentSessions.filter(session => sameRepo(session.repoPath, repoPath, pathOptions)).at(-1)
    : undefined);
  let launcher = $state<PromptLauncher>("claude");
  const actionLabel = $derived(selectedScope && focus ? "Improve this file" : report && report.overall.lines_found > 0 ? "Improve coverage" : "Generate coverage");
  let copied = $state(false);
  let copying = $state(false);
  let launching = $state(false);
  let previewOpen = $state(false);
  let failure = $state<FailureCopy | null>(null);
  const uid = $props.id();
  const focusId = `coverage-focus-${uid}`;
  let notice = $state<string | null>(null);
  let copyTimer: ReturnType<typeof setTimeout> | undefined;
  let launchController: AbortController | undefined;
  let destroyed = false;

  $effect(() => {
    // Feedback always refers to the snapshot currently shown in the preview.
    void prompt;
    copied = false;
    failure = null;
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
    failure = null;
    const ok = await copyText(text);
    if (destroyed) return;
    copying = false;
    if (prompt !== text) return;
    if (ok) {
      copied = true;
      clearTimeout(copyTimer);
      copyTimer = setTimeout(() => { copied = false; }, 1800);
    } else {
      failure = describeFailure({ verb: "copy", object: "the prompt" }, "Select and copy it from the preview.");
      previewOpen = true;
    }
  }

  async function runAgent(launcher: PromptLauncher) {
    if (!repoPath || launching) return;
    launching = true;
    failure = null;
    notice = null;
    launchController = new AbortController();
    try {
      const opened = terminalLaunchRequests.request(repoPath, launcher, prompt, launchController.signal);
      repoStore.setTerminalOpen(true);
      await opened;
      if (!destroyed) notice = "Agent session opened in the terminal. Follow progress below, then Rescan coverage.";
    } catch (err: unknown) {
      if (!destroyed) failure = describeFailure({ verb: "run", object: "the agent" }, err);
    } finally {
      if (!destroyed) launching = false;
    }
  }
</script>

<!--
  One toolbar line, not a four-row card.

  This is an action bar, and it was laid out as a feature panel: a heading, a
  subtitle restating the two buttons beside it, then the focus toggle and the
  preview disclosure each on a row of its own. Standing permanently above the
  coverage data, that cost about a hundred pixels to say what the buttons
  already say. The heading is now the button's own label, the subtitle is its
  tooltip, and the preview stays a `<details>` — a chip on this row when
  closed, a full-width panel when open.
-->
<section aria-label="Generate coverage with a coding agent" class="shrink-0 bg-surface/40 px-4 py-1.5 font-sans max-h-[45%] overflow-y-auto">
  <div class="flex flex-wrap items-center gap-x-2 gap-y-1.5">
    <Terminal size={13} class="shrink-0 text-textMuted" />
    <span class="gp-label shrink-0">Agent</span>
    <select aria-label="Coverage coding agent" bind:value={launcher} class="gp-select max-w-40">
      {#each PROMPT_LAUNCHERS as kind (kind)}<option value={kind}>{PROVIDER_LABELS[kind]}</option>{/each}
    </select>
    <button
      type="button"
      class="gp-btn"
      data-variant="primary"
      data-size="sm"
      aria-label={`Run in ${PROVIDER_LABELS[launcher]}`}
      title="Runs in GitPulse’s terminal with this repository as the working directory"
      disabled={!repoPath || launching}
      onclick={() => void runAgent(launcher)}
    >
      {#if launching}<LoaderCircle size={12} class="animate-spin" />{:else}<Terminal size={12} />{/if} {actionLabel}
    </button>
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      title="Copy the prompt for an agent session you already have open"
      disabled={!repoPath || copying}
      onclick={() => void copyPrompt()}
      aria-label={copied ? "Agent prompt copied" : "Copy agent prompt"}
    >
      {#if copying}<LoaderCircle size={12} class="animate-spin" />{:else if copied}<Check size={12} />{:else}<Clipboard size={12} />{/if}
      {copied ? "Copied" : "Copy prompt"}
    </button>
    {#if focus}
      <span class="gp-chip min-w-0" data-tone="neutral" title="Scope the prompt to {focus.file.path}">
        <Checkbox id={focusId} ariaLabel="Focus on {focus.file.path}" checked={selectedScope} onchange={(next) => { selectedScope = next; }} />
        <label for={focusId} class="min-w-0 cursor-pointer truncate hover:text-textPrimary">Focus on {focus.file.path}</label>
      </span>
    {/if}
    <!-- Full width only while open, so the closed disclosure costs this row's
         tail rather than a row of its own. -->
    <details class="text-meta {previewOpen ? 'w-full' : 'ml-auto shrink-0'}" bind:open={previewOpen}>
      <summary class="gp-chip w-fit cursor-pointer hover:text-textPrimary" data-tone="neutral" data-kind="label">Preview prompt</summary>
      <p class="my-2 text-textMuted">Includes this checkout’s path, accepted report locations and coverage snapshot. The agent runs tests using its configured permissions; artifacts appear after Rescan.</p>
      <textarea aria-label="Coverage agent prompt" readonly value={prompt} spellcheck={false} class="gp-field gp-field-multi h-48 w-full resize-y p-3 font-mono text-meta leading-relaxed"></textarea>
    </details>
  </div>
  {#if recentSession}
    <div class="mt-1.5 flex flex-wrap items-center gap-2 text-meta">
      <span class="text-textMuted">{PROVIDER_LABELS[recentSession.launcher]} session: {recentSession.status}</span>
      <button type="button" class="gp-btn" data-size="sm" onclick={() => { repoStore.setTerminalOpen(true); recentSession?.reveal(); }}>View agent session</button>
      <button type="button" class="gp-btn" data-size="sm" disabled={scanning} onclick={onRescan}><RefreshCw size={11} /> Rescan results</button>
      <span class="text-textMuted">Session status does not verify coverage. Rescan after the report is written.</span>
    </div>
  {/if}
  {#if !repoPath}<p class="mt-1.5 text-meta text-textMuted">Open a repository to generate its coverage prompt.</p>{/if}
  {#if launching}<p role="status" class="mt-1.5 text-meta text-textMuted">Opening agent terminal…</p>{/if}
  {#if copied}<p role="status" class="sr-only">Agent prompt copied</p>{/if}
  {#if failure}
    <div class="mt-1.5">
      <div role="alert" class="gp-notice text-meta" data-tone="danger">{failure.headline}{#if failure.detail} {failure.detail}{/if}</div>
    </div>
  {/if}
  {#if notice && !recentSession}<p role="status" class="mt-1.5 text-meta text-textMuted">{notice}</p>{/if}
</section>
