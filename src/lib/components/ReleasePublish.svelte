<script lang="ts">
  import { AlertTriangle, LoaderCircle, Rocket, Tag } from "@lucide/svelte";
  import { describeFailure } from "../ui/copy/describeFailure";
  import { currentRepoPath, repoStore } from "../stores/repoStore";
  import { verdictLabel } from "../stores/harnessStore";
  import { formatReleaseDate, releaseTagSuggestion, type ReleaseInfo } from "../ops/model";
  import type { WorkflowRunInfo } from "../github/types";
  import { untrack } from "svelte";
  import Checkbox from "./Checkbox.svelte";

  /**
   * "Push release tag…" for the Remote pane's Releases block: an annotated
   * SemVer tag pushed through Manvi, which is what starts the release workflow.
   * The release list itself stays the pane's own; this takes the releases and
   * runs it already read, to say what the last release was and what the
   * workflow has been doing, and asks the pane to re-read once a tag is out.
   */
  let {
    releases,
    runs,
    onPublished,
  }: {
    releases: ReleaseInfo[];
    runs: WorkflowRunInfo[];
    /** A tag was pushed; the pane re-reads GitHub so the release and its run show up. */
    onPublished: (message: string) => void;
  } = $props();

  const RECENT_RUNS = 3;

  let open = $state(false);
  let publishing = $state(false);
  let releaseTag = $state("");
  let releaseMessage = $state("");
  let releaseConfirmed = $state(false);
  /** Once the person types a tag, a later tag-list refresh must not replace it. */
  let tagEdited = $state(false);
  let error = $state<string | null>(null);

  const repoTags = $derived($repoStore.tags);
  const suggestion = $derived(releaseTagSuggestion(repoTags.map((tag) => tag.name)));
  const latest = $derived(releases.find((release) => release.is_latest) ?? releases[0] ?? null);
  const recentRuns = $derived(runs.slice(0, RECENT_RUNS));
  const uncommitted = $derived($repoStore.statuses.length);
  const canPush = $derived(releaseConfirmed && releaseTag.trim() !== "" && !publishing);

  $effect(() => {
    // A suggestion that arrives after the form opened fills the field only
    // while the field is still the one the form opened with.
    const next = suggestion;
    untrack(() => {
      if (!tagEdited) {
        releaseTag = next;
        releaseMessage = `Release ${next}`;
      }
    });
  });

  $effect(() => {
    void $currentRepoPath;
    untrack(() => {
      open = false;
      releaseConfirmed = false;
      tagEdited = false;
      error = null;
    });
  });

  function runState(run: WorkflowRunInfo): string {
    return run.conclusion || run.status || "unknown";
  }

  function editTag(value: string) {
    releaseTag = value;
    tagEdited = true;
    // A message still reading "Release <old tag>" follows the tag.
    if (releaseMessage === "" || /^Release\s/.test(releaseMessage)) releaseMessage = `Release ${value.trim()}`;
  }

  async function publishRelease() {
    if (!canPush) return;
    const repo = $repoStore.currentPath;
    const tag = releaseTag.trim();
    publishing = true;
    error = null;
    try {
      const outcome = await repoStore.publishRelease(tag, releaseMessage.trim() || `Release ${tag}`);
      if ($repoStore.currentPath !== repo) return;
      if (!outcome.ok) {
        error = outcome.error ?? "Couldn't publish the release.";
        return;
      }
      releaseConfirmed = false;
      open = false;
      tagEdited = false;
      onPublished(
        `Pushed ${outcome.output?.tag ?? tag} to ${outcome.output?.remote ?? "the remote"}${outcome.policy ? ` — ${verdictLabel(outcome.policy)}` : ""}; the release workflow can now build the app.`,
      );
    } catch (cause) {
      if ($repoStore.currentPath === repo) {
        const copy = describeFailure({ verb: "publish", object: "the release" }, cause);
        error = copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
      }
    } finally {
      publishing = false;
    }
  }
</script>

<div class="mt-3 space-y-2" data-testid="release-publish">
  <div class="flex flex-wrap items-center justify-between gap-2">
    <p class="min-w-0 text-meta text-textMuted">
      {#if latest}
        Latest release <span class="font-mono text-textPrimary">{latest.tag_name}</span>{#if latest.published_at || latest.created_at}, {formatReleaseDate(latest.published_at || latest.created_at)}{/if}.
      {:else}
        No release has been published yet.
      {/if}
    </p>
    <button
      type="button"
      class="gp-btn"
      data-size="sm"
      aria-expanded={open}
      onclick={() => (open = !open)}
    >
      <Tag size={13} /> Push release tag…
    </button>
  </div>

  {#if recentRuns.length > 0}
    <div class="text-meta text-textMuted">
      <div class="mb-1">Recent workflow activity</div>
      <div class="space-y-1">
        {#each recentRuns as run (run.id)}
          <div class="flex items-center justify-between gap-2">
            <span class="min-w-0 truncate">{run.title || run.name}</span>
            <span class="gp-chip" data-tone="neutral" data-kind="label">{runState(run)}</span>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  {#if open}
    <div class="gp-card space-y-2 p-3">
      <p class="text-meta text-textMuted">
        On <span class="font-mono">{$repoStore.currentBranch ?? "no branch"}</span>, default branch
        <span class="font-mono">{$repoStore.defaultBranch ?? "unknown"}</span>,
        {uncommitted === 0 ? "no uncommitted files" : `${uncommitted} uncommitted file${uncommitted === 1 ? "" : "s"}`}.
      </p>
      <input
        class="gp-field w-full font-mono"
        placeholder="v1.2.3"
        aria-label="Release tag"
        value={releaseTag}
        oninput={(event) => editTag(event.currentTarget.value)}
      />
      <input class="gp-field w-full" maxlength="4096" placeholder="Release message" aria-label="Release message" bind:value={releaseMessage} />
      <div class="rounded-lg bg-surfaceHover p-2">
        <Checkbox
          checked={releaseConfirmed}
          onchange={(next) => (releaseConfirmed = next)}
          label="I confirm the working tree is ready."
          description="GitPulse will still require a clean, fully synchronized default branch and will refuse duplicate remote tags."
        />
      </div>
      {#if error}
        <div class="gp-notice" data-tone="danger" role="alert">
          <AlertTriangle size={14} class="shrink-0" style="color: var(--gp-notice-ink)" />
          <span class="min-w-0 flex-1">{error}</span>
        </div>
      {/if}
      <button type="button" class="gp-btn w-full justify-center" data-variant="primary" onclick={publishRelease} disabled={!canPush}>
        {#if publishing}<LoaderCircle size={14} class="animate-spin" />{:else}<Rocket size={14} />{/if} Push release tag
      </button>
    </div>
  {/if}
</div>
