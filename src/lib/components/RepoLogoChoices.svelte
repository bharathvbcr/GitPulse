<script lang="ts" module>
  import { isCaseInsensitiveFs } from "../repos/paths";

  const pathOptions = { caseInsensitive: isCaseInsensitiveFs() };
</script>

<script lang="ts">
  import { FolderOpen, WandSparkles } from "@lucide/svelte";
  import { logoInitials, monogramColor } from "../repos/repoLogo";
  import { logoSubject, repoLogos, type LogoTab } from "../repos/repoLogos";
  import type { LogoCandidate, LogoChoice } from "../repos/repoLogoStore";
  import { TAB_COLOR_INK, type TabColor } from "../repos/tabColors";

  /**
   * The tab menu's "Logo" row: the automatic pick, initials only, one of
   * the repository's likeliest images, or Browse for any other image it
   * holds. Mounted only while the menu is open, so the listing and
   * thumbnails are read only when someone asks to choose.
   */
  interface Props {
    tab: LogoTab;
    color: TabColor | null;
    /** Opens the browser for every image in the repository. */
    onbrowse?: () => void;
  }
  let { tab, color, onbrowse }: Props = $props();

  const subject = $derived(logoSubject(tab.path, tab, pathOptions));
  const tile = $derived(monogramColor(subject.name, color ?? subject.color));
  const initials = $derived(logoInitials(subject.name));
  // Bumped by a pick so the row's selection mark follows the store's record.
  let revision = $state(0);
  const choice = $derived.by((): LogoChoice => {
    void revision;
    return repoLogos.choiceFor(subject.target.key);
  });
  const shown = $derived($repoLogos[subject.target.key]);

  let candidates = $state<LogoCandidate[] | null>(null);
  let failed = $state(false);

  $effect(() => {
    const target = subject.target;
    if (target.blocked) return;
    let live = true;
    repoLogos.candidates(target).then(
      (found) => {
        if (live) candidates = found;
      },
      () => {
        if (live) failed = true;
      },
    );
    return () => {
      live = false;
    };
  });

  function pick(next: LogoChoice) {
    void repoLogos.choose(subject.target, next);
    revision++;
  }

  function fileName(path: string): string {
    return path.slice(path.lastIndexOf("/") + 1);
  }
</script>

<div class="px-2 py-1.5" role="group" aria-label="Logo" data-logo-choices>
  <div class="gp-label">Logo</div>
  <div class="pt-0.5 text-micro text-textMuted">
    {#if choice.kind === "file"}
      Using {fileName(choice.path)}
    {:else if choice.kind === "initials"}
      Using initials
    {:else if shown?.status === "ready" && shown.path}
      Found {fileName(shown.path)}
    {:else if subject.target.blocked}
      Trust this repository to use its images
    {:else}
      No logo found: showing initials
    {/if}
  </div>
  <div class="flex flex-wrap items-center gap-1 pt-1">
    <button
      type="button"
      role="menuitem"
      data-logo-choice="auto"
      aria-label={choice.kind === "auto" ? "Find automatically, selected" : "Find automatically"}
      title="Find the logo automatically"
      class="grid size-6 place-items-center rounded-md border border-border bg-surface text-textMuted hover:text-textPrimary {choice.kind === 'auto' ? 'ring-2 ring-textPrimary' : ''}"
      onclick={() => pick({ kind: "auto" })}
    >
      <WandSparkles size={12} aria-hidden="true" />
    </button>
    <button
      type="button"
      role="menuitem"
      data-logo-choice="initials"
      aria-label={choice.kind === "initials" ? `Initials ${initials}, selected` : `Initials ${initials}`}
      title="Initials only"
      class="grid size-6 place-items-center rounded-md text-micro font-semibold text-white {choice.kind === 'initials' ? 'ring-2 ring-textPrimary' : ''}"
      style:background-color={TAB_COLOR_INK[tile]}
      onclick={() => pick({ kind: "initials" })}
    >{initials}</button>
    {#if subject.target.blocked}
      <!-- No images to offer: the repository may not be read. -->
    {:else if failed}
      <span class="text-micro text-textMuted">Images unavailable</span>
    {:else if candidates === null}
      <span class="text-micro text-textMuted" data-logo-choices-loading>Looking for images…</span>
    {:else}
      {#each candidates as candidate (candidate.path)}
        {@const selected = choice.kind === "file" && choice.path === candidate.path}
        <button
          type="button"
          role="menuitem"
          data-logo-choice={candidate.path}
          aria-label={selected ? `${candidate.path}, selected` : candidate.path}
          title={candidate.path}
          class="grid size-6 place-items-center overflow-hidden rounded-md border border-border bg-surface p-0.5 {selected ? 'ring-2 ring-textPrimary' : ''}"
          onclick={() => pick({ kind: "file", path: candidate.path })}
        >
          <img src={candidate.url} alt="" class="h-full w-full object-contain" draggable="false" />
        </button>
      {/each}
    {/if}
    {#if onbrowse && !subject.target.blocked}
      {@const browsedFile =
        choice.kind === "file" && candidates !== null && !candidates.some((candidate) => candidate.path === choice.path)}
      <button
        type="button"
        role="menuitem"
        data-logo-choice="browse"
        aria-label={browsedFile ? "Browse the repository's images, a file from there is selected" : "Browse the repository's images"}
        title="Browse all of the repository's images…"
        class="grid size-6 place-items-center rounded-md border border-dashed border-border bg-surface text-textMuted hover:text-textPrimary {browsedFile ? 'ring-2 ring-textPrimary' : ''}"
        onclick={onbrowse}
      >
        <FolderOpen size={12} aria-hidden="true" />
      </button>
    {/if}
  </div>
</div>
