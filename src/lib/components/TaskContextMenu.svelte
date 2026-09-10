<script lang="ts">
  import { onMount } from "svelte";
  import { ArrowLeft, Clipboard, Copy, Hash, Plus, Sparkles, SquareCheck, SquarePen, Trash2 } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { shouldDismissOverlay } from "../ui/dismiss";
  import { cycleFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { clampMenuPosition } from "../branches/menuPosition";
  import { menuPageItems, submenuTitle, type TaskMenuIcon, type TaskMenuItem, type TaskMenuSubmenu } from "../workbench/taskMenu";

  let {
    items,
    x,
    y,
    label,
    onAction,
    onClose,
  }: {
    items: TaskMenuItem[];
    x: number;
    y: number;
    label: string;
    onAction: (item: TaskMenuItem) => void;
    onClose: (restoreFocus?: boolean) => void;
  } = $props();

  let menuEl: HTMLDivElement | undefined = $state();
  let left = $state(0);
  let top = $state(0);
  let page = $state<"root" | TaskMenuSubmenu>("root");
  const shown = $derived(menuPageItems(items, page));

  function fit() {
    if (!menuEl) return;
    // Layout dimensions remain stable while the opening animation scales the menu.
    const next = clampMenuPosition(x, y, menuEl.offsetWidth, menuEl.offsetHeight, window.innerWidth - 8, window.innerHeight - 8);
    left = Math.max(8, next.left);
    top = Math.max(8, next.top);
  }

  onMount(() => {
    fit();
    const onPointer = (event: PointerEvent) => {
      if (shouldDismissOverlay(event.target, "[data-task-menu]")) onClose(false);
    };
    const onContext = (event: MouseEvent) => {
      if (shouldDismissOverlay(event.target, "[data-task-menu]")) onClose(false);
    };
    const onViewport = () => onClose(false);
    window.addEventListener("pointerdown", onPointer, true);
    window.addEventListener("contextmenu", onContext);
    window.addEventListener("resize", onViewport);
    window.addEventListener("scroll", onViewport, true);
    return () => {
      window.removeEventListener("pointerdown", onPointer, true);
      window.removeEventListener("contextmenu", onContext);
      window.removeEventListener("resize", onViewport);
      window.removeEventListener("scroll", onViewport, true);
    };
  });

  $effect(() => {
    left = x;
    top = y;
    items.length;
    page;
    queueMicrotask(fit);
  });

  $effect(() => {
    if (!menuEl) return;
    shown.length;
    const timer = window.setTimeout(() => {
      menuEl?.querySelector<HTMLElement>('[role="menuitem"]:not([aria-disabled="true"])')?.focus();
    }, 0);
    return () => window.clearTimeout(timer);
  });

  function onKey(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Escape") {
      event.preventDefault();
      if (page !== "root") page = "root";
      else onClose(true);
      return;
    }
    if (event.key === "ArrowLeft" && page !== "root") {
      event.preventDefault();
      page = "root";
      return;
    }
    if (event.key === "Tab") {
      event.preventDefault();
      onClose(false);
      return;
    }
    if (!menuEl) return;
    if (event.key === "ArrowDown") { event.preventDefault(); cycleFocus(menuEl, true); }
    else if (event.key === "ArrowUp") { event.preventDefault(); cycleFocus(menuEl, false); }
    else if (event.key === "Home") {
      event.preventDefault();
      menuEl.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }
    else if (event.key === "End") {
      event.preventDefault();
      const buttons = menuEl.querySelectorAll<HTMLElement>('[role="menuitem"]');
      buttons[buttons.length - 1]?.focus();
    }
    else if (event.key === "ArrowRight") {
      const focused = event.target;
      if (!(focused instanceof HTMLElement)) return;
      const id = focused.dataset.menuId;
      const item = shown.find((entry) => entry.id === id);
      if (item?.action.kind === "submenu") {
        event.preventDefault();
        page = item.action.submenu;
      }
    }
  }

  function choose(item: TaskMenuItem) {
    if (item.disabled) return;
    if (item.action.kind === "submenu") {
      page = item.action.submenu;
      return;
    }
    onAction(item);
  }
</script>

<div
  bind:this={menuEl}
  use:portal={"body"}
  data-task-menu
  class="gp-menu gp-pop fixed min-w-56 max-w-[min(18rem,calc(100vw-1rem))] max-h-[min(24rem,calc(100vh-1rem))] overflow-y-auto text-xs text-textPrimary py-1"
  style="left: {left}px; top: {top}px; z-index: {LAYERS.MENU}"
  role="menu"
  aria-label={label}
  tabindex="-1"
  onkeydown={onKey}
>
  {#if page !== "root"}
    <button type="button" role="menuitem" class="gp-menu-item" onclick={() => { page = "root"; }}>
      <ArrowLeft size={13} />
      <span class="flex-1 min-w-0 truncate">{submenuTitle(page)}</span>
    </button>
    <div class="gp-menu-sep" role="separator"></div>
  {:else}
    <div class="px-2.5 py-1 text-[10px] text-textMuted">{label}</div>
  {/if}
  {#each shown as item (item.id)}
    {#if item.separatorBefore && page === "root"}<div class="gp-menu-sep" role="separator"></div>{/if}
    <button
      type="button"
      role="menuitem"
      data-menu-id={item.id}
      class="gp-menu-item {item.danger ? 'text-rose-400' : ''}"
      aria-disabled={item.disabled}
      aria-haspopup={item.action.kind === "submenu" ? "menu" : undefined}
      disabled={item.disabled}
      onclick={() => choose(item)}
    >
      {#if item.icon}{@render menuIcon(item.icon, item.danger)}{/if}
      <span class="flex-1 min-w-0 truncate">{item.label}</span>
      {#if item.hint}<span class="text-[10px] text-textMuted">{item.hint}</span>{/if}
      {#if item.action.kind === "submenu"}<span class="text-textMuted" aria-hidden="true">›</span>{/if}
    </button>
  {/each}
</div>

{#snippet menuIcon(name: TaskMenuIcon, danger?: boolean)}
  {#if name === "open"}<SquarePen size={13} />
  {:else if name === "enhance"}<Sparkles size={13} class="text-accent" />
  {:else if name === "duplicate"}<Copy size={13} />
  {:else if name === "copy"}<Copy size={13} />
  {:else if name === "id"}<Hash size={13} />
  {:else if name === "brief"}<Clipboard size={13} />
  {:else if name === "move"}<span class="w-[13px] text-center" aria-hidden="true">→</span>
  {:else if name === "priority"}<span class="w-[13px] text-center" aria-hidden="true">!</span>
  {:else if name === "select"}<SquareCheck size={13} />
  {:else if name === "add"}<Plus size={13} />
  {:else if name === "delete"}<Trash2 size={13} class={danger ? "text-rose-400" : ""} />
  {/if}
{/snippet}
