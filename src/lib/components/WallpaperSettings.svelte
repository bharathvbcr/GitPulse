<script lang="ts">
  import { Check, ImagePlus, Trash2 } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { WALLPAPER_INTENSITY_RANGE, appearanceNotes } from "../ui/appearance/apply";
  import { BUILTIN_BACKDROPS, builtinBackdropUrl, type BackdropId } from "../ui/appearance/backdrops";
  import {
    MAX_WALLPAPER_VIDEO_BYTES,
    WALLPAPER_ACCEPT,
    clearWallpaper,
    customWallpaper,
    saveWallpaper,
  } from "../ui/appearance/wallpaperStore";
  import { BUSY_LOAD, QUIET_LOAD, motionReasonText, wallpaperMotionState } from "../ui/appearance/wallpaperMotion";
  import SettingToggle from "./SettingToggle.svelte";
  import Slider from "./Slider.svelte";

  /**
   * Settings -> Appearance -> Theme and wallpaper -> Wallpaper, right under the
   * presets, since a preset sets one too.
   *
   * The picture is drawn under the window material, in place of the desktop, so
   * it is only offered where that material exists (the catalog entry requires
   * `nativeMaterial`). "None" is the way to take any wallpaper away, a preset's
   * included. The reader's own image or video is kept in IndexedDB by
   * wallpaperStore.ts; nothing about it enters the preferences except the
   * choice `custom`.
   *
   * Intensity fades the picture, still or moving, toward the ground colour.
   * Animate asks for movement; whether it moves right now is
   * wallpaperMotion.ts's decision, and the line under the switch says which
   * of its reasons holds, so a wallpaper standing still never looks broken.
   */
  let picker = $state<HTMLInputElement>();
  let busy = $state(false);
  let failure = $state("");

  const prefs = $derived($interfaceStore);
  const custom = $derived($customWallpaper);
  const notes = $derived($appearanceNotes.filter((note) => note.id.startsWith("backdrop-")));
  const motion = $derived($wallpaperMotionState);
  const motionLine = $derived(prefs.wallpaperMotion && prefs.backdrop !== "none" ? motionReasonText(motion) : null);
  const isVideo = $derived(custom.status === "ready" && custom.videoUrl !== null);

  function choose(id: BackdropId) {
    failure = "";
    interfaceStore.setBackdrop(id);
  }

  async function onPick(event: Event & { currentTarget: HTMLInputElement }) {
    const input = event.currentTarget;
    const file = input.files?.[0];
    // Let the same file be picked again after a fix.
    input.value = "";
    if (!file) return;
    failure = "";
    busy = true;
    try {
      const result = await saveWallpaper(file);
      if (result.ok) interfaceStore.setBackdrop("custom");
      else failure = result.reason;
    } finally {
      busy = false;
    }
  }

  async function removeImage() {
    failure = "";
    const result = await clearWallpaper();
    if (!result.ok) {
      failure = result.reason;
      return;
    }
    if (prefs.backdrop === "custom") interfaceStore.setBackdrop("none");
  }
</script>

<div class="flex flex-col gap-2" data-wallpaper-settings>
  <div class="text-meta font-medium text-textPrimary">Wallpaper</div>
  <div class="grid grid-cols-3 gap-2 sm:grid-cols-4" role="group" aria-label="Wallpaper">
    <button
      type="button"
      class="flex flex-col gap-1 rounded-lg border p-1 text-left {prefs.backdrop === 'none'
        ? 'border-accent bg-accent/10'
        : 'border-edge hover:bg-surfaceHover'}"
      aria-pressed={prefs.backdrop === "none"}
      data-backdrop-option="none"
      onclick={() => choose("none")}
    >
      <span class="flex aspect-[16/10] w-full items-center justify-center rounded-md border border-dashed border-edge text-micro text-textMuted">
        None
      </span>
      <span class="flex items-center justify-between gap-1 px-0.5">
        <span class="truncate text-micro text-textPrimary">Desktop</span>
        {#if prefs.backdrop === "none"}<Check size={11} class="shrink-0 text-accent" />{/if}
      </span>
    </button>

    {#each BUILTIN_BACKDROPS as backdrop (backdrop.id)}
      {@const picked = prefs.backdrop === backdrop.id}
      <button
        type="button"
        class="flex flex-col gap-1 rounded-lg border p-1 text-left {picked
          ? 'border-accent bg-accent/10'
          : 'border-edge hover:bg-surfaceHover'}"
        aria-pressed={picked}
        title={backdrop.description}
        data-backdrop-option={backdrop.id}
        onclick={() => choose(backdrop.id)}
      >
        <img
          src={builtinBackdropUrl(backdrop.id)}
          alt=""
          class="aspect-[16/10] w-full rounded-md object-cover"
          loading="lazy"
          decoding="async"
        />
        <span class="flex items-center justify-between gap-1 px-0.5">
          <span class="truncate text-micro text-textPrimary">{backdrop.label}</span>
          {#if picked}<Check size={11} class="shrink-0 text-accent" />{/if}
        </span>
      </button>
    {/each}

    {#if custom.status === "ready"}
      {@const picked = prefs.backdrop === "custom"}
      <button
        type="button"
        class="flex flex-col gap-1 rounded-lg border p-1 text-left {picked
          ? 'border-accent bg-accent/10'
          : 'border-edge hover:bg-surfaceHover'}"
        aria-pressed={picked}
        title={custom.name}
        data-backdrop-option="custom"
        onclick={() => choose("custom")}
      >
        <span class="relative block">
          <img src={custom.url} alt="" class="aspect-[16/10] w-full rounded-md object-cover" decoding="async" />
          {#if isVideo}
            <span class="absolute bottom-1 left-1 rounded-sm bg-black/60 px-1 text-micro text-white" data-custom-video-badge>Video</span>
          {/if}
        </span>
        <span class="flex items-center justify-between gap-1 px-0.5">
          <span class="truncate text-micro text-textPrimary">{custom.name}</span>
          {#if picked}<Check size={11} class="shrink-0 text-accent" />{/if}
        </span>
      </button>
    {/if}
  </div>

  <div class="flex flex-wrap items-center gap-2">
    <input
      bind:this={picker}
      type="file"
      accept={WALLPAPER_ACCEPT}
      hidden
      aria-label="Wallpaper image or video"
      data-wallpaper-input
      onchange={onPick}
    />
    <button
      type="button"
      class="gp-btn"
      data-variant="secondary"
      data-size="sm"
      disabled={busy}
      onclick={() => picker?.click()}
    >
      <ImagePlus size={12} />
      {busy ? "Preparing…" : custom.status === "ready" ? "Replace image or video…" : "Choose image or video…"}
    </button>
    {#if custom.status === "ready"}
      <button type="button" class="gp-btn" data-variant="ghost" data-size="sm" onclick={removeImage}>
        <Trash2 size={12} />
        {isVideo ? "Remove video" : "Remove image"}
      </button>
    {/if}
  </div>

  {#if failure}
    <div class="gp-notice" data-tone="danger" role="alert" data-wallpaper-failure>
      <div class="min-w-0 flex-1">{failure}</div>
    </div>
  {/if}
  {#if custom.status === "unavailable"}
    <p class="text-meta text-unknown" role="status">
      Your own image can't be stored here: {custom.reason} The built-in wallpapers still work.
    </p>
  {/if}
  {#each notes as note (note.id)}
    <p class="text-meta {note.tone === 'warn' ? 'text-warn' : 'text-textMuted'}" role="status" data-backdrop-note={note.id}>
      {note.text}
    </p>
  {/each}

  <div class="flex flex-col gap-1.5" data-wallpaper-intensity>
    <Slider
      label="Wallpaper intensity"
      value={prefs.wallpaperIntensity}
      min={WALLPAPER_INTENSITY_RANGE.min}
      max={WALLPAPER_INTENSITY_RANGE.max}
      step={0.05}
      disabled={prefs.backdrop === "none"}
      format={(n) => `${Math.round(n * 100)}%`}
      oninput={(next) => interfaceStore.setWallpaperIntensity(next)}
    />
    <p class="text-micro text-textMuted">Lower fades the wallpaper into the theme's background color.</p>
  </div>

  <div class="flex flex-col gap-1.5" data-wallpaper-motion-setting>
    <SettingToggle
      label="Animate"
      description="Built-in wallpapers drift and glint, and a video plays, only while the Mac is plugged in and the system is quiet (load under {QUIET_LOAD} per core; it stops above {BUSY_LOAD}). Reduce Motion always keeps it still."
      ariaLabel="Animate the wallpaper when plugged in and the system is quiet"
      checked={prefs.wallpaperMotion}
      onchange={(next) => interfaceStore.setWallpaperMotion(next)}
    />
    {#if motionLine}
      <p
        class="text-meta {motion.play ? 'text-textPrimary' : 'text-textMuted'}"
        role="status"
        data-wallpaper-motion-status={motion.reason}
      >
        {motionLine}
      </p>
    {/if}
  </div>

  <p class="text-micro text-textMuted">
    Drawn behind the window in place of the desktop, under the same veil, so text stays as readable as over any
    desktop. An image is resized to at most 2560 pixels; a video (MP4, WebM or MOV, up to
    {MAX_WALLPAPER_VIDEO_BYTES / (1024 * 1024)} MB) plays muted and shows its first frame whenever it is still. Both are
    kept on this computer only.
  </p>
</div>
