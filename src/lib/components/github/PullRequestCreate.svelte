<script lang="ts">
  import Checkbox from "../Checkbox.svelte";
  /**
   * Opens a pull request from the current branch with `gh pr create`.
   *
   * The backend always passes `--head`, so gh never pushes or forks: the
   * branch must already be on GitHub, and when it is not, gh's refusal is
   * what the reader sees. Sending goes through `confirmAndRunPrAction`.
   */
  import { describeFailure } from "../../ui/copy/describeFailure";
  import { verdictLabel } from "../../stores/harnessStore";
  import { confirmAndRunPrAction } from "../../github/prActions";

  let {
    repoPath,
    slug,
    headBranch,
    defaultBase,
    onCreated,
    onCancel,
  }: {
    repoPath: string;
    slug: string;
    /** Null when HEAD is detached: there is nothing to open a PR from. */
    headBranch: string | null;
    defaultBase: string;
    onCreated: (message: string) => void;
    onCancel: () => void;
  } = $props();

  let title = $state("");
  let body = $state("");
  // Seeded once from the repository's default branch; the reader owns it after.
  let base = $state((() => defaultBase)());
  let draft = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  const ready = $derived(!!headBranch && title.trim().length > 0 && base.trim().length > 0 && base.trim() !== headBranch);

  async function submit() {
    if (!ready || busy) return;
    busy = true;
    error = null;
    try {
      const result = await confirmAndRunPrAction(
        repoPath,
        { kind: "create", title, body, base: base.trim(), draft },
        slug,
        { headBranch },
      );
      if (!result) return;
      onCreated(`${result.output || "Pull request opened."}${result.policy ? ` — ${verdictLabel(result.policy)}` : ""}`);
    } catch (err) {
      const copy = describeFailure({ verb: "open", object: "the pull request" }, err);
      error = copy.detail ? `${copy.headline} ${copy.detail}` : copy.headline;
    } finally {
      busy = false;
    }
  }
</script>

<form
  data-pr-create
  class="mb-3 p-3 rounded-xl border border-edge bg-background/60 text-xs space-y-2"
  onsubmit={(event) => { event.preventDefault(); void submit(); }}
>
  {#if !headBranch}
    <div class="text-warn">HEAD is detached. Check out a branch to open a pull request from it.</div>
  {:else}
    <div class="text-meta text-textMuted font-mono">{headBranch} → <input class="gp-field inline-block w-40 text-meta!" data-size="sm" aria-label="Base branch" bind:value={base} disabled={busy} /></div>
    <input class="gp-field w-full" placeholder="Title" aria-label="Pull request title" bind:value={title} disabled={busy} />
    <textarea class="gp-field-multi w-full min-h-20 font-sans" placeholder="Description" aria-label="Pull request description" bind:value={body} disabled={busy}></textarea>
    <div class="flex items-center gap-3">
      <label class="flex items-center gap-1.5 text-meta text-textMuted"><Checkbox ariaLabel="Draft" checked={draft} disabled={busy} onchange={(next) => (draft = next)} /> draft</label>
      <span class="flex-1"></span>
      <button type="button" class="gp-btn" data-size="sm" onclick={onCancel} disabled={busy}>Cancel</button>
      <button type="submit" class="gp-btn" data-variant="primary" data-size="sm" disabled={!ready || busy}>Open pull request…</button>
    </div>
  {/if}
  {#if error}<div class="gp-notice" data-tone="danger" role="alert">{error}</div>{/if}
</form>
