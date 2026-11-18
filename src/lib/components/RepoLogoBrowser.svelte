<script lang="ts" module>
  import { isCaseInsensitiveFs } from "../repos/paths";

  const pathOptions = { caseInsensitive: isCaseInsensitiveFs() };

  /** A thumbnail's state: loading, drawn, not a drawable logo, or not readable. */
  type Thumb = { state: "loading" } | { state: "ready"; url: string } | { state: "unusable" } | { state: "failed" };
</script>

<script lang="ts">
  import { ChevronRight, Folder, ImageOff, Search } from "@lucide/svelte";
  import { SvelteMap } from "svelte/reactivity";
  import Dialog from "./Dialog.svelte";
  import { logoDirOf, logoFolderView, logoParentDir, MAX_BROWSE_SHOWN, type LogoImageList } from "../repos/repoLogo";
  import { logoSubject, repoLogos, type LogoTab } from "../repos/repoLogos";

  /**
   * Choose a repository's logo from any image it holds: walk its folders or
   * filter by name, see each image as it would draw, and use one. Every image
   * is read through the same trust-gated read and the same checks the
   * automatic pick uses, so a file this shows as usable is one the tab can
   * draw. Thumbnails are read only as they scroll into view, a few at a time,
   * and whatever is still queued is dropped when the dialog closes.
   */
  interface Props {
    tab: LogoTab;
    /** Where focus goes when the dialog closes; the control that opened it may be gone. */
    returnFocus?: HTMLElement | null;
    onclose: () => void;
  }
  let { tab, returnFocus = null, onclose }: Props = $props();

  // The subject is fixed for the dialog's life: a tab that re-resolves its
  // family while the dialog is open must not move the choice to another key.
  // svelte-ignore state_referenced_locally
  const subject = logoSubject(tab.path, tab, pathOptions);
  const chosen = repoLogos.choiceFor(subject.target.key);
  const shownPath = $repoLogos[subject.target.key]?.path ?? null;
  const startPath = chosen.kind === "file" ? chosen.path : shownPath;

  let open = $state(true);
  let list = $state<LogoImageList | null>(null);
  let failed = $state(false);
  let dir = $state(startPath ? logoDirOf(startPath) : "");
  let query = $state("");
  let selected = $state<string | null>(chosen.kind === "file" ? chosen.path : null);
  const thumbs = new SvelteMap<string, Thumb>();
  const abort = new AbortController();
  let searchEl = $state<HTMLInputElement | null>(null);
  let gridEl = $state<HTMLElement | null>(null);

  $effect(() => {
    let live = true;
    repoLogos.images(subject.target).then(
      (found) => {
        if (!live) return;
        list = found;
        // A start folder that holds no images any more opens at the root.
        if (dir !== "" && !found.paths.some((path) => path.startsWith(`${dir}/`))) dir = "";
      },
      () => {
        if (live) failed = true;
      },
    );
    return () => {
      live = false;
      abort.abort();
      // Thumbnails are held as data URLs: none outlive the dialog.
      repoLogos.releasePreviews(subject.target);
    };
  });

  // The filter searches the whole repository: someone typing a name wants
  // the file wherever it sits, not only below the folder in view.
  const view = $derived(list ? logoFolderView(list.paths, query.trim() ? "" : dir, query) : null);
  const crumbs = $derived.by(() => {
    if (dir === "") return [];
    const parts = dir.split("/");
    return parts.map((name, i) => ({ name, path: parts.slice(0, i + 1).join("/") }));
  });
  const selectedThumb = $derived(selected ? thumbs.get(selected) : undefined);
  const canUse = $derived(selected !== null && selectedThumb?.state === "ready");

  function load(path: string) {
    if (thumbs.has(path)) return;
    thumbs.set(path, { state: "loading" });
    repoLogos.preview(subject.target, path, abort.signal).then(
      (url) => thumbs.set(path, url ? { state: "ready", url } : { state: "unusable" }),
      () => {
        if (!abort.signal.aborted) thumbs.set(path, { state: "failed" });
      },
    );
  }

  /** Reads a tile's thumbnail once it scrolls into view. */
  function thumbnail(node: HTMLElement, path: string) {
    if (typeof IntersectionObserver === "undefined") {
      load(path);
      return {};
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          load(path);
          observer.disconnect();
        }
      },
      { root: gridEl?.closest("[data-dialog-body]") ?? null, rootMargin: "120px" },
    );
    observer.observe(node);
    return { destroy: () => observer.disconnect() };
  }

  function openFolder(path: string) {
    dir = path;
    query = "";
    gridEl?.closest("[data-dialog-body]")?.scrollTo?.({ top: 0 });
  }

  function close() {
    open = false;
    // svelte-ignore state_referenced_locally
    const back = returnFocus;
    onclose();
    // After the focus trap has handed focus back to what it recorded, which
    // is the menu item that opened this and is no longer in the page.
    window.setTimeout(() => {
      if (back?.isConnected) back.focus();
    }, 0);
  }

  function use(path: string | null = selected) {
    if (!path || thumbs.get(path)?.state !== "ready") return;
    void repoLogos.choose(subject.target, { kind: "file", path });
    close();
  }

  function fileName(path: string): string {
    return path.slice(path.lastIndexOf("/") + 1);
  }

  function onSearchKey(event: KeyboardEvent) {
    if (event.isComposing) return;
    if (event.key === "Backspace" && query === "" && dir !== "") {
      event.preventDefault();
      openFolder(logoParentDir(list?.paths ?? [], dir));
    } else if (event.key === "ArrowDown") {
      const first = gridEl?.querySelector<HTMLElement>("[data-browse-tile]");
      if (first) {
        event.preventDefault();
        first.focus();
      }
    } else if (event.key === "Enter" && view && view.folders.length + view.files.length === 1) {
      // One match: Enter opens it or picks it.
      event.preventDefault();
      if (view.folders[0]) openFolder(view.folders[0].path);
      else if (view.files[0]) selected = view.files[0];
    }
  }

  /** Arrow keys move between tiles; up from the first row returns to the filter. */
  function onGridKey(event: KeyboardEvent) {
    if (!gridEl) return;
    const tiles = [...gridEl.querySelectorAll<HTMLElement>("[data-browse-tile]")];
    const index = tiles.findIndex((tile) => tile === document.activeElement);
    if (index < 0) return;
    const top = tiles[0]?.offsetTop ?? 0;
    const columns = Math.max(1, tiles.filter((tile) => tile.offsetTop === top).length);
    let next: number | null = null;
    if (event.key === "ArrowRight") next = Math.min(tiles.length - 1, index + 1);
    else if (event.key === "ArrowLeft") next = Math.max(0, index - 1);
    else if (event.key === "ArrowDown") next = Math.min(tiles.length - 1, index + columns);
    else if (event.key === "ArrowUp") {
      if (index < columns) {
        event.preventDefault();
        searchEl?.focus();
        return;
      }
      next = index - columns;
    } else if (event.key === "Backspace" && dir !== "") {
      event.preventDefault();
      openFolder(logoParentDir(list?.paths ?? [], dir));
      searchEl?.focus();
      return;
    }
    if (next === null) return;
    event.preventDefault();
    tiles[next]?.focus();
    tiles[next]?.scrollIntoView?.({ block: "nearest" });
  }
</script>

<Dialog bind:open size="lg" title={`Choose a logo for ${subject.name}`} onclose={close} initialFocus={() => searchEl}>
  <div class="flex flex-col gap-2" data-logo-browser>
    <div class="flex flex-wrap items-center gap-2">
      <nav
        aria-label="Folder"
        class="flex min-w-0 flex-1 flex-wrap items-center gap-0.5 text-meta transition-opacity {query.trim() ? 'opacity-50' : ''}"
      >
        <button
          type="button"
          class="rounded px-1 py-0.5 hover:bg-surfaceHover {dir === '' ? 'font-semibold text-textPrimary' : 'text-textMuted'}"
          aria-current={dir === "" ? "location" : undefined}
          data-browse-crumb=""
          onclick={() => openFolder("")}
        >{subject.name}</button>
        {#each crumbs as crumb (crumb.path)}
          <ChevronRight size={12} class="shrink-0 text-textMuted" aria-hidden="true" />
          <button
            type="button"
            class="max-w-48 truncate rounded px-1 py-0.5 hover:bg-surfaceHover {crumb.path === dir ? 'font-semibold text-textPrimary' : 'text-textMuted'}"
            aria-current={crumb.path === dir ? "location" : undefined}
            data-browse-crumb={crumb.path}
            onclick={() => openFolder(crumb.path)}
          >{crumb.name}</button>
        {/each}
      </nav>
      <label class="relative w-56 max-w-full">
        <span class="sr-only">Search all images</span>
        <Search size={13} class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-textMuted" aria-hidden="true" />
        <input
          bind:this={searchEl}
          bind:value={query}
          type="search"
          class="gp-field w-full pl-8"
          placeholder="Search all images"
          autocomplete="off"
          spellcheck="false"
          data-browse-filter
          onkeydown={onSearchKey}
        />
      </label>
    </div>

    <div class="text-micro text-textMuted" role="status" aria-live="polite" data-browse-status>
      {#if subject.target.blocked}
        Trust this repository to choose one of its images.
      {:else if failed}
        This repository's files can't be listed right now, so its images can't be shown.
      {:else if !list || !view}
        Looking for images…
      {:else if list.total === 0}
        This repository has no image files. Its tab shows initials.
      {:else if query.trim() !== ""}
        {view.total === 0 ? "No images match" : view.total === 1 ? "1 image matches" : `${view.total} images match`}{view.total > MAX_BROWSE_SHOWN ? `; showing the first ${MAX_BROWSE_SHOWN}` : ""}
      {:else}
        {list.total === 1 ? "1 image" : `${list.total} images`} in this repository{list.total > list.paths.length ? `; the first ${list.paths.length} can be browsed` : ""}{view.total > MAX_BROWSE_SHOWN ? `. This folder shows ${MAX_BROWSE_SHOWN} of ${view.total}: filter to find the rest` : ""}
      {/if}
    </div>

    {#if view && view.total > 0}
      <!-- The grid only moves focus between its own tile buttons with the
           arrow keys; each tile is a button that does its own work. -->
      <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
      <div
        bind:this={gridEl}
        class="grid grid-cols-[repeat(auto-fill,minmax(6.5rem,1fr))] gap-2 pb-1"
        role="group"
        aria-label={query.trim() ? "Matching images" : "Folders and images"}
        onkeydown={onGridKey}
        data-browse-grid
      >
        {#each view.folders as folder (folder.path)}
          <button
            type="button"
            class="flex h-24 flex-col items-center justify-center gap-1 rounded-lg border border-border bg-surface px-1.5 text-center hover:bg-surfaceHover"
            title={folder.path}
            data-browse-tile
            data-browse-folder={folder.path}
            onclick={() => openFolder(folder.path)}
          >
            <Folder size={22} class="text-textMuted" aria-hidden="true" />
            <span class="w-full truncate text-meta text-textPrimary">{folder.name}</span>
            <span class="text-micro text-textMuted">{folder.images === 1 ? "1 image" : `${folder.images} images`}</span>
          </button>
        {/each}
        {#each view.files as path (path)}
          {@const thumb = thumbs.get(path)}
          {@const isSelected = selected === path}
          {@const unusable = thumb?.state === "unusable" || thumb?.state === "failed"}
          <button
            type="button"
            use:thumbnail={path}
            class="flex h-24 flex-col items-center justify-center gap-1 rounded-lg border bg-surface px-1.5 text-center {isSelected
              ? 'border-accent ring-2 ring-accent'
              : 'border-border hover:bg-surfaceHover'} {unusable ? 'opacity-60' : ''}"
            title={thumb?.state === "unusable"
              ? `${path}: not the image its name says`
              : thumb?.state === "failed"
                ? `${path}: couldn't be read`
                : path}
            aria-pressed={isSelected}
            aria-label={`${query.trim() ? path : fileName(path)}${unusable ? ", can't be used" : ""}`}
            data-browse-tile
            data-browse-file={path}
            data-browse-thumb={thumb?.state ?? "waiting"}
            onclick={() => {
              if (!unusable) selected = path;
            }}
            ondblclick={() => use(path)}
          >
            <span class="grid size-12 place-items-center overflow-hidden rounded-md bg-background p-1">
              {#if thumb?.state === "ready"}
                <img src={thumb.url} alt="" class="h-full w-full object-contain" draggable="false" />
              {:else if unusable}
                <ImageOff size={18} class="text-textMuted" aria-hidden="true" />
              {/if}
            </span>
            <span class="w-full truncate text-meta text-textPrimary">{fileName(path)}</span>
            {#if query.trim() && logoDirOf(path)}
              <span class="w-full truncate text-micro text-textMuted">{logoDirOf(path)}</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    <span class="mr-auto min-w-0 truncate text-xs text-textMuted" data-browse-selection>
      {#if selected}
        {selected}
      {:else}
        Pick an image, or double-click one to use it
      {/if}
    </span>
    <button type="button" class="gp-btn" data-size="md" onclick={close}>Cancel</button>
    <button
      type="button"
      class="gp-btn"
      data-variant="primary"
      data-size="md"
      disabled={!canUse}
      data-browse-use
      onclick={() => use()}
    >Use as logo</button>
  {/snippet}
</Dialog>
