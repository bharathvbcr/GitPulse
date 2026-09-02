<script lang="ts">
  /**
   * Remotes, submodules and the stash stack in one place.
   *
   * These three answer the questions a Git client is otherwise silent about:
   * where does this repository push to, why are these folders empty, and what
   * did I put aside and forget. Each section loads on demand and reports its
   * own failure rather than failing the pane — a broken `.gitmodules` must not
   * hide the remote list.
   */
  import { repoStore } from "../stores/repoStore";
  import { toastStore } from "../stores/toastStore";

  let { embedded = false }: { embedded?: boolean } = $props();
  import { createAsyncGuard, type AsyncGuard } from "../async/guard";
  import { formatError } from "../ui/formatError";
  import { reportPanelError } from "../diagnostics/report";
  import {
    Cloud,
    Package,
    Archive,
    RefreshCw,
    AlertTriangle,
    ShieldAlert,
    Loader2,
    Plus,
    Trash2,
  } from "lucide-svelte";
  import {
    describeRemotes,
    carriesEmbeddedCredential,
    hasSplitUrls,
    isDestructiveRemoteChange,
    redactRemoteUrl,
    remoteChangeConsequence,
    remoteHost,
    validateRemoteName,
    validateRemoteUrl,
    type RemoteChange,
    type RemoteInfo,
  } from "../repos/remotes";
  import {
    canDeinit,
    canInitialize,
    canSync,
    blockedInitializeReason,
    describeSubmodules,
    initializableSubmodules,
    isDestructiveSubmoduleChange,
    needsAttention,
    sortSubmodules,
    submoduleChangeConsequence,
    submoduleStateExplanation,
    submoduleStateLabel,
    type SubmoduleChange,
    type SubmoduleInfo,
  } from "../repos/submodules";
  import {
    isDestructiveStashAction,
    isStaleStackError,
    stashActionConsequence,
    stashActionLabel,
    stashEmptyMessage,
    stashSubtitle,
    stashTitle,
    STASH_ACTIONS,
    type StashAction,
    type StashEntry,
  } from "../repos/stash";

  let remotes = $state<RemoteInfo[]>([]);
  let remotesError = $state<string | null>(null);
  let remotesLoaded = $state(false);
  let remotesTruncated = $state(false);

  let submodules = $state<SubmoduleInfo[]>([]);
  let submodulesError = $state<string | null>(null);
  let submodulesLoaded = $state(false);
  let submodulesTruncated = $state(false);

  let busy = $state<string | null>(null);
  /**
   * Two-stage confirm for the actions that can lose work.
   *
   * Held as a structured value rather than a composed string key: matching an
   * armed entry by string suffix would arm the wrong row if one object id
   * happened to end with another, and parsing the action back out of a key is
   * a decode step that can silently disagree with what was encoded.
   */
  type Armed =
    | { kind: "stash"; key: string; action: StashAction; oid: string }
    | { kind: "remote"; key: string; change: RemoteChange }
    | { kind: "submodule"; key: string; change: SubmoduleChange };
  let armed = $state<Armed | null>(null);

  let addName = $state("");
  let addUrl = $state("");
  let addError = $state<string | null>(null);
  let editingRemote = $state<string | null>(null);
  let editUrl = $state("");
  let renamingRemote = $state<string | null>(null);
  let renameTo = $state("");
  let renameError = $state<string | null>(null);

  let guard: AsyncGuard | null = null;
  let loadedPath: string | null = null;

  const repoPath = $derived($repoStore.currentPath);
  // The stash stack rides the session snapshot, so it is already refreshed by
  // every mutation and watcher event — no separate load or staleness here.
  const stashEntries = $derived($repoStore.stashEntries);
  const stashFailed = $derived($repoStore.stashFailed);

  $effect(() => {
    const path = repoPath;
    if (path === loadedPath) return;
    loadedPath = path;
    armed = null;
    if (!path) {
      guard?.cancel();
      remotes = [];
      submodules = [];
      remotesLoaded = false;
      submodulesLoaded = false;
      remotesError = null;
      remotesTruncated = false;
      submodulesError = null;
      submodulesTruncated = false;
      renamingRemote = null;
      editingRemote = null;
      return;
    }
    void load();
  });

  $effect(() => () => guard?.cancel());

  async function load() {
    guard?.cancel();
    const active = createAsyncGuard();
    guard = active;
    // Loaded independently: a broken .gitmodules must not hide the remotes.
    await Promise.all([
      (async () => {
        try {
          const list = await repoStore.listRemotes();
          if (!active.isLive()) return;
          remotes = list.remotes;
          remotesTruncated = list.truncated;
          remotesError = null;
        } catch (err) {
          if (!active.isLive()) return;
          remotes = [];
          remotesTruncated = false;
          remotesError = reportPanelError("remotes", err);
        } finally {
          if (active.isLive()) remotesLoaded = true;
        }
      })(),
      (async () => {
        try {
          const list = await repoStore.listSubmodules();
          if (!active.isLive()) return;
          submodules = sortSubmodules(list.submodules);
          submodulesTruncated = list.truncated;
          submodulesError = null;
        } catch (err) {
          if (!active.isLive()) return;
          submodules = [];
          submodulesTruncated = false;
          submodulesError = reportPanelError("submodules", err);
        } finally {
          if (active.isLive()) submodulesLoaded = true;
        }
      })(),
    ]);
  }

  async function run(key: string, action: () => Promise<{ ok: boolean; error?: string }>) {
    if (busy) return;
    busy = key;
    try {
      const outcome = await action();
      if (!outcome.ok) {
        const message = outcome.error ?? "The action could not be completed.";
        toastStore.error(
          isStaleStackError(message)
            ? "The stash list changed — refreshed. Try again."
            : message,
        );
      }
      await load();
    } catch (err) {
      toastStore.error(formatError(err));
    } finally {
      busy = null;
      armed = null;
      renamingRemote = null;
      renameTo = "";
      renameError = null;
    }
  }

  /** Stash actions arm first when they can lose work. */
  function activateStash(entry: StashEntry, action: StashAction) {
    const key = `stash-${action}-${entry.oid}`;
    if (isDestructiveStashAction(action) && armed?.key !== key) {
      armed = { kind: "stash", key, action, oid: entry.oid };
      return;
    }
    void run(key, () => repoStore.stashAction(action, entry));
  }

  function activateRemote(change: RemoteChange) {
    const key = `remote-${change.kind}-${change.name}`;
    if (isDestructiveRemoteChange(change) && armed?.key !== key) {
      armed = { kind: "remote", key, change };
      return;
    }
    void run(key, () => repoStore.remoteChange(change));
  }

  function activateSubmodule(change: SubmoduleChange) {
    const ident = "path" in change ? (change.path ?? "all") : "all";
    const key = `submodule-${change.kind}-${ident}`;
    if (isDestructiveSubmoduleChange(change) && armed?.key !== key) {
      armed = { kind: "submodule", key, change };
      return;
    }
    void run(key, () => repoStore.submoduleChange(change));
  }

  function submitAddRemote() {
    const nameErr = validateRemoteName(addName);
    const urlErr = validateRemoteUrl(addUrl);
    addError = nameErr ?? urlErr;
    if (addError) return;
    const name = addName.trim();
    const url = addUrl.trim();
    void run("remote-add", async () => {
      const outcome = await repoStore.remoteChange({ kind: "add", name, url });
      if (outcome.ok) {
        addName = "";
        addUrl = "";
        addError = null;
      }
      return outcome;
    });
  }

  function submitSetUrl(name: string) {
    const urlErr = validateRemoteUrl(editUrl);
    if (urlErr) {
      addError = urlErr;
      return;
    }
    activateRemote({ kind: "seturl", name, url: editUrl.trim(), push: false });
  }

  function submitRename(name: string) {
    const nameErr = validateRemoteName(renameTo);
    if (nameErr) {
      renameError = nameErr;
      return;
    }
    const newName = renameTo.trim();
    if (newName === name) {
      renameError = "The new name is the same as the current one.";
      return;
    }
    renameError = null;
    activateRemote({ kind: "rename", name, new_name: newName });
  }

  const initializable = $derived(initializableSubmodules(submodules));
  const syncable = $derived(submodules.filter(canSync));
</script>

<!-- `embedded` drops the pane chrome so this can be a section inside the Work
     view without nesting a second scroll container inside the first, which
     traps the wheel and strands the reader mid-page. -->
<div
  class={embedded
    ? "text-xs font-sans"
    : "flex-1 min-h-0 overflow-auto bg-background p-4 text-xs font-sans"}
>
  <div class={embedded ? "space-y-4" : "mx-auto max-w-4xl space-y-4"}>
    <!-- Remotes -->
    <section class="gp-card rounded-2xl p-4">
      <header class="mb-3 flex items-center justify-between gap-2">
        <div class="flex items-center gap-2 min-w-0">
          <Cloud size={15} class="shrink-0 text-accent" />
          <h2 class="font-semibold text-textPrimary">Remotes</h2>
          <span class="truncate text-[11px] text-textMuted">
            {remotesLoaded ? describeRemotes(remotes) : "Loading…"}
          </span>
        </div>
        <button
          type="button"
          class="gp-btn !py-1 !px-2 !text-[11px]"
          onclick={() => void load()}
          disabled={busy !== null}
          title="Reload remotes and submodules"
        >
          <RefreshCw size={11} />
        </button>
      </header>

      {#if remotesTruncated}
        <p class="mb-2 flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
          <AlertTriangle size={11} class="mt-0.5 shrink-0" />
          <span>Showing {remotes.length} remotes. More exist in .git/config and are not listed.</span>
        </p>
      {/if}
      {#if remotesError}
        <p class="text-[11px] text-red-600 dark:text-red-400">{remotesError}</p>
      {:else if remotes.length === 0}
        <p class="text-[11px] text-textMuted">
          {remotesLoaded
            ? "Nothing to fetch from or push to. Add a remote to sync this repository."
            : "Reading .git/config…"}
        </p>
      {:else}
        <ul class="space-y-2">
          {#each remotes as remote (remote.name)}
            {@const pruneKey = `remote-prune-${remote.name}`}
            {@const removeKey = `remote-remove-${remote.name}`}
            {@const setKey = `remote-seturl-${remote.name}`}
            <li class="rounded-xl border border-border/50 px-3 py-2">
              <div class="flex flex-wrap items-center gap-2">
                <span class="font-mono font-semibold text-textPrimary">{remote.name}</span>
                {#if remote.is_default}
                  <span class="rounded-full bg-accent/15 px-1.5 py-0.5 text-[10px] text-accent">default</span>
                {/if}
                <span class="text-[11px] text-textMuted">
                  {remote.tracking_branches} tracked branch{remote.tracking_branches === 1 ? "" : "es"}
                </span>
              </div>
              <p class="mt-1 truncate font-mono text-[11px] text-textMuted">
                {remote.fetch_url ? redactRemoteUrl(remote.fetch_url) : "no fetch URL"}
                {#if remoteHost(remote.fetch_url)}
                  <span class="text-textMuted/70"> · {remoteHost(remote.fetch_url)}</span>
                {/if}
              </p>
              {#if hasSplitUrls(remote)}
                <!-- Either a deliberate fork workflow or work going to the
                     wrong repository; both are invisible if collapsed. -->
                <p class="mt-1 flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
                  <AlertTriangle size={11} class="mt-0.5 shrink-0" />
                  <span class="min-w-0 break-all">
                    Pushes go elsewhere: <span class="font-mono">{redactRemoteUrl(remote.push_url ?? "")}</span>
                  </span>
                </p>
              {/if}
              {#if carriesEmbeddedCredential(remote.fetch_url) || carriesEmbeddedCredential(remote.push_url)}
                <p class="mt-1 flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
                  <ShieldAlert size={11} class="mt-0.5 shrink-0" />
                  <span>This URL embeds a password or token. It is hidden here, but it is stored in plain text in .git/config.</span>
                </p>
              {/if}
              <div class="mt-2 flex flex-wrap items-center gap-1.5">
                <button
                  type="button"
                  class="gp-btn !py-1 !px-2.5 !text-[11px]"
                  disabled={busy !== null}
                  onclick={() => {
                    editingRemote = remote.name;
                    editUrl = remote.fetch_url ?? "";
                    renamingRemote = null;
                    renameError = null;
                    armed = null;
                  }}
                >
                  Set URL
                </button>
                <button
                  type="button"
                  class="gp-btn !py-1 !px-2.5 !text-[11px]"
                  disabled={busy !== null}
                  onclick={() => {
                    renamingRemote = remote.name;
                    renameTo = remote.name;
                    renameError = null;
                    editingRemote = null;
                    armed = null;
                  }}
                >
                  Rename
                </button>
                <button
                  type="button"
                  class="gp-btn !py-1 !px-2.5 !text-[11px] {armed?.key === pruneKey
                    ? '!border-red-500/60 !text-red-600 dark:!text-red-400'
                    : ''}"
                  disabled={busy !== null}
                  onclick={() => activateRemote({ kind: "prune", name: remote.name })}
                >
                  {armed?.key === pruneKey ? "Confirm prune" : "Prune"}
                </button>
                <button
                  type="button"
                  class="gp-btn !py-1 !px-2.5 !text-[11px] {armed?.key === removeKey
                    ? '!border-red-500/60 !text-red-600 dark:!text-red-400'
                    : ''}"
                  disabled={busy !== null}
                  onclick={() => activateRemote({ kind: "remove", name: remote.name })}
                >
                  <Trash2 size={11} />
                  {armed?.key === removeKey ? "Confirm remove" : "Remove"}
                </button>
                {#if armed?.kind === "remote" && (armed.key === pruneKey || armed.key === removeKey || armed.key === setKey)}
                  <button type="button" class="gp-btn !py-1 !px-2.5 !text-[11px]" onclick={() => (armed = null)}>
                    Cancel
                  </button>
                {/if}
              </div>
              {#if editingRemote === remote.name}
                <form
                  class="mt-2 flex flex-wrap items-center gap-1.5"
                  onsubmit={(e) => {
                    e.preventDefault();
                    submitSetUrl(remote.name);
                  }}
                >
                  <input
                    class="gp-input min-w-0 flex-1 font-mono !text-[11px]"
                    bind:value={editUrl}
                    aria-label="New URL for {remote.name}"
                    placeholder="https://github.com/owner/repo.git"
                  />
                  <button type="submit" class="gp-btn-primary !py-1 !px-2.5 !text-[11px]" disabled={busy !== null}>
                    {armed?.key === setKey ? "Confirm URL" : "Apply URL"}
                  </button>
                </form>
              {/if}
              {#if renamingRemote === remote.name}
                <form
                  class="mt-2 flex flex-wrap items-center gap-1.5"
                  onsubmit={(e) => {
                    e.preventDefault();
                    submitRename(remote.name);
                  }}
                >
                  <input
                    class="gp-input min-w-0 flex-1 font-mono !text-[11px]"
                    bind:value={renameTo}
                    aria-label="New name for {remote.name}"
                    placeholder="upstream"
                  />
                  <button type="submit" class="gp-btn-primary !py-1 !px-2.5 !text-[11px]" disabled={busy !== null}>
                    Apply name
                  </button>
                </form>
                {#if renameError}
                  <p class="mt-1 text-[11px] text-red-600 dark:text-red-400">{renameError}</p>
                {:else if renameTo.trim() && renameTo.trim() !== remote.name}
                  <p class="mt-1 text-[11px] leading-relaxed text-textMuted">
                    {remoteChangeConsequence({ kind: "rename", name: remote.name, new_name: renameTo.trim() })}
                  </p>
                {/if}
              {/if}
              {#if armed?.kind === "remote" && armed.change.name === remote.name}
                <p class="mt-1.5 text-[11px] leading-relaxed text-red-600 dark:text-red-400">
                  {remoteChangeConsequence(armed.change)}
                </p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      <form
        class="mt-3 flex flex-col gap-1.5"
        onsubmit={(e) => {
          e.preventDefault();
          submitAddRemote();
        }}
      >
        <div class="flex flex-wrap items-center gap-1.5">
          <input
            class="gp-input w-28 font-mono !text-[11px]"
            bind:value={addName}
            aria-label="Remote name"
            placeholder="origin"
          />
          <input
            class="gp-input min-w-0 flex-1 font-mono !text-[11px]"
            bind:value={addUrl}
            aria-label="Remote URL"
            placeholder="https://github.com/owner/repo.git"
          />
          <button type="submit" class="gp-btn-primary !py-1 !px-2.5 !text-[11px] inline-flex items-center gap-1" disabled={busy !== null}>
            {#if busy === "remote-add"}<Loader2 size={11} class="animate-spin" />{/if}
            <Plus size={11} />
            Add remote
          </button>
        </div>
        {#if addError}
          <p class="text-[11px] text-red-600 dark:text-red-400">{addError}</p>
        {/if}
      </form>
    </section>

    <!-- Submodules -->
    <section class="gp-card rounded-2xl p-4">
      <header class="mb-3 flex items-center justify-between gap-2">
        <div class="flex items-center gap-2 min-w-0">
          <Package size={15} class="shrink-0 text-accent" />
          <h2 class="font-semibold text-textPrimary">Submodules</h2>
          <span class="truncate text-[11px] text-textMuted">
            {submodulesLoaded ? describeSubmodules(submodules) : "Loading…"}
          </span>
        </div>
        <div class="flex items-center gap-1.5 shrink-0">
          {#if syncable.length > 0}
            <button
              type="button"
              class="gp-btn !py-1 !px-2.5 !text-[11px] inline-flex items-center gap-1.5"
              disabled={busy !== null}
              onclick={() =>
                activateSubmodule({ kind: "sync", path: null, recursive: true })}
            >
              {#if busy === "submodule-sync-all"}<Loader2 size={11} class="animate-spin" />{/if}
              <span>Sync URLs</span>
            </button>
          {/if}
          {#if initializable.length > 0}
            <button
              type="button"
              class="gp-btn-primary !py-1 !px-2.5 !text-[11px] inline-flex items-center gap-1.5"
              disabled={busy !== null}
              onclick={() =>
                activateSubmodule({ kind: "update", path: null, recursive: true })}
            >
              {#if busy === "submodule-update-all"}<Loader2 size={11} class="animate-spin" />{/if}
              <span>Initialize {initializable.length}</span>
            </button>
          {/if}
        </div>
      </header>

      {#if submodulesTruncated}
        <p class="mb-2 flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
          <AlertTriangle size={11} class="mt-0.5 shrink-0" />
          <span>Showing {submodules.length} submodules. More exist and are not listed.</span>
        </p>
      {/if}
      {#if submodulesError}
        <p class="text-[11px] text-red-600 dark:text-red-400">{submodulesError}</p>
      {:else if submodules.length === 0}
        <p class="text-[11px] text-textMuted">
          {submodulesLoaded ? "Nothing is embedded in this repository." : "Reading .gitmodules…"}
        </p>
      {:else}
        <ul class="space-y-2">
          {#each submodules as sub (sub.path)}
            <li class="rounded-xl border border-border/50 px-3 py-2">
              <div class="flex flex-wrap items-center gap-2">
                <span class="font-mono text-textPrimary">{sub.path}</span>
                <span
                  class="rounded-full px-1.5 py-0.5 text-[10px] {needsAttention(sub)
                    ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
                    : 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'}"
                >
                  {submoduleStateLabel(sub.state)}
                </span>
                {#if sub.orphaned}
                  <span class="rounded-full bg-red-500/15 px-1.5 py-0.5 text-[10px] text-red-600 dark:text-red-400">
                    not in .gitmodules
                  </span>
                {/if}
              </div>
              {#if needsAttention(sub)}
                <p class="mt-1 text-[11px] leading-relaxed text-textMuted">
                  {submoduleStateExplanation(sub.state)}
                </p>
              {/if}
              {#if blockedInitializeReason(sub)}
                <!-- Saying why the button is absent is the only way the user
                     learns the real problem is a missing .gitmodules entry. -->
                <p class="mt-1 text-[11px] leading-relaxed text-red-600 dark:text-red-400">
                  {blockedInitializeReason(sub)}
                </p>
              {:else if canInitialize(sub)}
                <button
                  type="button"
                  class="gp-btn !mt-1.5 !py-1 !px-2.5 !text-[11px]"
                  disabled={busy !== null}
                  onclick={() =>
                    activateSubmodule({ kind: "update", path: sub.path, recursive: true })}
                >
                  Initialize
                </button>
              {/if}
              <div class="mt-1.5 flex flex-wrap items-center gap-1.5">
                {#if canSync(sub)}
                  <button
                    type="button"
                    class="gp-btn !py-1 !px-2.5 !text-[11px]"
                    disabled={busy !== null}
                    onclick={() =>
                      activateSubmodule({ kind: "sync", path: sub.path, recursive: false })}
                  >
                    Sync URL
                  </button>
                {/if}
                {#if canDeinit(sub)}
                  {@const deinitKey = `submodule-deinit-${sub.path}`}
                  <button
                    type="button"
                    class="gp-btn !py-1 !px-2.5 !text-[11px] {armed?.key === deinitKey
                      ? '!border-red-500/60 !text-red-600 dark:!text-red-400'
                      : ''}"
                    disabled={busy !== null}
                    onclick={() =>
                      activateSubmodule({ kind: "deinit", path: sub.path, force: false })}
                  >
                    {armed?.key === deinitKey ? "Confirm deinit" : "Remove working copy"}
                  </button>
                {/if}
                {#if armed?.kind === "submodule" && armed.change.kind === "deinit" && armed.change.path === sub.path}
                  <button type="button" class="gp-btn !py-1 !px-2.5 !text-[11px]" onclick={() => (armed = null)}>
                    Cancel
                  </button>
                {/if}
              </div>
              {#if armed?.kind === "submodule" && armed.change.kind === "deinit" && armed.change.path === sub.path}
                <p class="mt-1.5 text-[11px] leading-relaxed text-red-600 dark:text-red-400">
                  {submoduleChangeConsequence(armed.change)}
                </p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <!-- Stash -->
    <section class="gp-card rounded-2xl p-4">
      <header class="mb-3 flex items-center gap-2">
        <Archive size={15} class="shrink-0 text-accent" />
        <h2 class="font-semibold text-textPrimary">Stash</h2>
        <span class="text-[11px] text-textMuted">
          {stashEntries.length} entr{stashEntries.length === 1 ? "y" : "ies"}
        </span>
      </header>

      {#if stashFailed}
        <!-- An unreadable stack must not render as an empty one: a forgotten
             stash is work that exists nowhere else. -->
        <p class="flex items-start gap-1.5 text-[11px] text-amber-600 dark:text-amber-400">
          <AlertTriangle size={11} class="mt-0.5 shrink-0" />
          <span>The stash list could not be read, so this may not be complete.</span>
        </p>
      {:else if stashEntries.length === 0}
        <p class="text-[11px] text-textMuted">{stashEmptyMessage(true)}</p>
      {:else}
        <ul class="space-y-2">
          {#each stashEntries as entry (entry.oid)}
            <li class="rounded-xl border border-border/50 px-3 py-2">
              <p class="truncate font-medium text-textPrimary">{stashTitle(entry)}</p>
              <p class="mt-0.5 font-mono text-[11px] text-textMuted">{stashSubtitle(entry)}</p>
              <div class="mt-2 flex flex-wrap items-center gap-1.5">
                {#each STASH_ACTIONS as action (action)}
                  {@const key = `stash-${action}-${entry.oid}`}
                  {@const isArmed = armed?.kind === "stash" && armed.key === key}
                  <button
                    type="button"
                    class="{action === 'apply' ? 'gp-btn-primary' : 'gp-btn'} !py-1 !px-2.5 !text-[11px] inline-flex items-center gap-1.5 {isArmed
                      ? '!border-red-500/60 !text-red-600 dark:!text-red-400'
                      : ''}"
                    disabled={busy !== null}
                    title={stashActionConsequence(action)}
                    onclick={() =>
                      // The entry travels whole, so its object id can never be
                      // separated from its index.
                      activateStash(entry as StashEntry, action)}
                  >
                    {#if busy === key}<Loader2 size={11} class="animate-spin" />{/if}
                    <span>{isArmed ? `Confirm: ${stashActionLabel(action)}` : stashActionLabel(action)}</span>
                  </button>
                {/each}
                {#if armed?.kind === "stash" && armed.oid === entry.oid}
                  <button type="button" class="gp-btn !py-1 !px-2.5 !text-[11px]" onclick={() => (armed = null)}>
                    Cancel
                  </button>
                {/if}
              </div>
              {#if armed?.kind === "stash" && armed.oid === entry.oid}
                <p class="mt-1.5 text-[11px] leading-relaxed text-red-600 dark:text-red-400">
                  {stashActionConsequence(armed.action)}
                </p>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>
</div>
