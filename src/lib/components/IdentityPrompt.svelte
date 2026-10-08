<script lang="ts">
  /**
   * Offered when a commit was refused because git has no name or email to
   * record it under. Writes `user.name` / `user.email` to this repository or
   * to the global config, through the same gated command as any other write,
   * then hands control back so the caller can retry the commit.
   */
  import { onMount } from "svelte";
  import { UserRound } from "@lucide/svelte";
  import { repoStore } from "../stores/repoStore";
  import { identityProblem, type IdentityScope } from "../repos/gitPreflight";

  let { onSaved, onDismiss }: { onSaved: () => void; onDismiss: () => void } = $props();

  let name = $state("");
  let email = $state("");
  let scope = $state<IdentityScope>("global");
  let saving = $state(false);
  let error = $state<string | null>(null);
  let problem = $derived(identityProblem(name, email));

  onMount(() => {
    // Prefill whatever half is already known, so a missing email does not
    // also make the user retype a name git already has.
    void repoStore.gitIdentity().then((identity) => {
      if (!identity) return;
      if (!name && identity.name) name = identity.name;
      if (!email && identity.email) email = identity.email;
    }).catch(() => {});
  });

  async function save() {
    if (problem || saving) return;
    saving = true;
    error = null;
    const outcome = await repoStore.setGitIdentity(name.trim(), email.trim(), scope);
    saving = false;
    if (outcome.ok) onSaved();
    else error = outcome.error ?? "The identity was not saved.";
  }
</script>

<form class="rounded-lg border border-border/60 bg-background p-2.5 space-y-2 text-[11px]" onsubmit={(event) => { event.preventDefault(); void save(); }}>
  <p class="flex items-center gap-1.5 font-medium text-textPrimary"><UserRound size={12} /> Who should commits be recorded as?</p>
  <p class="text-[10px] text-textMuted">Git needs a name and an email for every commit. Nothing was committed yet.</p>
  <label class="block"><span class="sr-only">Name</span>
    <input class="gp-field w-full" placeholder="Your name" autocomplete="name" bind:value={name} disabled={saving} />
  </label>
  <label class="block"><span class="sr-only">Email</span>
    <input class="gp-field w-full" type="email" placeholder="you@example.org" autocomplete="email" bind:value={email} disabled={saving} />
  </label>
  <fieldset class="flex flex-wrap gap-x-3 gap-y-1" disabled={saving}>
    <legend class="sr-only">Where to save it</legend>
    <label class="inline-flex items-center gap-1.5"><input type="radio" name="identity-scope" value="global" bind:group={scope} />Every repository (global)</label>
    <label class="inline-flex items-center gap-1.5"><input type="radio" name="identity-scope" value="repo" bind:group={scope} />This repository only</label>
  </fieldset>
  {#if error}<p role="alert" class="text-rose-400 whitespace-pre-wrap">{error}</p>
  {:else if problem && (name || email)}<p class="text-[10px] text-amber-400">{problem}</p>{/if}
  <div class="flex justify-end gap-2">
    <button type="button" class="gp-btn" disabled={saving} onclick={onDismiss}>Not now</button>
    <button type="submit" class="gp-btn-primary" disabled={saving || !!problem}>{saving ? "Saving…" : "Save and commit"}</button>
  </div>
</form>
