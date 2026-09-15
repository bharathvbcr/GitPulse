<script lang="ts">
  import { onMount } from "svelte";
  import { Archive, ArrowLeft, Bot, Calendar, Check, Clipboard, Copy, Hash, Minus, Plus, Sparkles, SquareCheck, SquarePen, Tag, Trash2, User } from "@lucide/svelte";
  import { portal } from "../dom/portal";
  import { popover } from "../ui/popover";
  import { cycleFocus } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { menuPageItems, submenuTitle, typeAheadIndex, type TaskMenuIcon, type TaskMenuItem, type TaskMenuSubmenu } from "../workbench/taskMenu";

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
  let page = $state<"root" | TaskMenuSubmenu>("root");
  const shown = $derived(menuPageItems(items, page));

  /**
   * Type-ahead buffer, cleared after a pause so "re" then "d" is two searches
   * rather than one for "red". 700ms matches the platform menu convention.
   */
  let typed = "";
  let typedTimer: ReturnType<typeof setTimeout> | undefined;

  /**
   * Dismissal and placement, from the shared popover owner.
   *
   * `scroll` is the one worth reading twice. A page scroll moves the anchor
   * out from under the menu, so the menu closes; its *own* scroll does not.
   * The owner registers that listener on the capture phase — scroll does not
   * bubble — and judges it by containment, which is what keeps focusing a row
   * below the fold from dismissing the menu it is navigating. End, ArrowDown
   * past the fold and type-ahead all used to close the menu instead of moving
   * through it, and the longer the page (Labels, Owner) the more often it
   * happened.
   *
   * `revision` re-measures when a submenu page or a changed item list changes
   * the menu's height under an already-clamped position. Escape stays with
   * `onKey` below: this menu owns its keyboard, and Escape there backs out of
   * a submenu before it closes anything.
   */
  const dismissal = $derived({
    anchor: { kind: "point" as const, x, y },
    inset: 8,
    revision: `${items.length}:${page}`,
    dismiss: {
      inside: "[data-task-menu]",
      contextmenu: true,
      scroll: true,
      resize: true,
      escape: "none" as const,
    },
    onDismiss: () => onClose(false),
  });

  // The type-ahead buffer outlives any one open menu page, so its timer is
  // cleared on the component's own teardown rather than the popover's.
  onMount(() => () => { if (typedTimer) clearTimeout(typedTimer); });

  /**
   * Every focusable row, in DOM order.
   *
   * Selected by tag rather than by `role`, because value rows are
   * `menuitemcheckbox` and a `[role="menuitem"]` selector silently skipped
   * them — End would land on the last *plain* item with four checkable rows
   * still below it.
   */
  function allRows(): HTMLElement[] {
    return menuEl ? [...menuEl.querySelectorAll<HTMLElement>("button:not([disabled])")] : [];
  }

  $effect(() => {
    if (!menuEl) return;
    shown.length;
    const timer = window.setTimeout(() => { allRows()[0]?.focus(); }, 0);
    return () => window.clearTimeout(timer);
  });

  /** Rows on this page, including disabled ones, aligned with `shown`. */
  function rows(): HTMLElement[] {
    return menuEl ? [...menuEl.querySelectorAll<HTMLElement>("[data-menu-id]")] : [];
  }

  /**
   * Focus the next row whose label starts with what has been typed.
   *
   * Only single printable characters feed the buffer — a modifier chord is a
   * shortcut, not a search, and swallowing it here would break Escape, Tab
   * and the arrow keys handled above.
   */
  function typeAhead(key: string) {
    if (typedTimer) clearTimeout(typedTimer);
    typed += key.toLowerCase();
    typedTimer = setTimeout(() => { typed = ""; }, 700);
    const elements = rows();
    const current = elements.indexOf(document.activeElement as HTMLElement);
    // A repeated single letter cycles matches; a longer buffer restarts the
    // search from the top so "re" finds Ready even while Review has focus.
    const from = typed.length > 1 ? -1 : current;
    const index = typeAheadIndex(
      shown.map((item) => ({ label: item.label, disabled: item.disabled })),
      typed,
      from,
    );
    if (index >= 0) elements[index]?.focus();
  }

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
      allRows()[0]?.focus();
    }
    else if (event.key === "End") {
      event.preventDefault();
      allRows().at(-1)?.focus();
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
    else if (event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey && event.key !== " ") {
      event.preventDefault();
      typeAhead(event.key);
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
  use:popover={dismissal}
  data-task-menu
  class="gp-menu gp-pop fixed min-w-56 max-w-[min(18rem,calc(100vw-1rem))] max-h-[min(32rem,calc(100vh-1rem))] overflow-y-auto text-xs text-textPrimary py-1"
  style="z-index: {LAYERS.MENU}"
  role="menu"
  aria-label={label}
  tabindex="-1"
  onkeydown={onKey}
>
  {#if page !== "root"}
    <button type="button" role="menuitem" class="gp-menu-item" data-menu-back onclick={() => { page = "root"; }}>
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
      role={item.checked === undefined ? "menuitem" : "menuitemcheckbox"}
      data-menu-id={item.id}
      class="gp-menu-item {item.danger ? 'text-rose-400' : ''}"
      aria-disabled={item.disabled}
      aria-checked={item.checked === undefined ? undefined : item.checked === "mixed" ? "mixed" : item.checked}
      aria-haspopup={item.action.kind === "submenu" ? "menu" : undefined}
      disabled={item.disabled}
      onclick={() => choose(item)}
    >
      {#if item.checked !== undefined}
        <span class="mark" aria-hidden="true">
          {#if item.checked === true}<Check size={12} />{:else if item.checked === "mixed"}<Minus size={12} />{/if}
        </span>
      {:else if item.icon}{@render menuIcon(item.icon, item.danger)}{/if}
      <span class="flex-1 min-w-0 truncate">{item.label}</span>
      {#if item.hint}<span class="text-[10px] text-textMuted shrink-0">{item.hint}</span>{/if}
      {#if item.action.kind === "submenu"}<span class="text-textMuted shrink-0" aria-hidden="true">›</span>{/if}
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
  {:else if name === "agent"}<Bot size={13} class="text-accent" />
  {:else if name === "due"}<Calendar size={13} />
  {:else if name === "owner"}<User size={13} />
  {:else if name === "label"}<Tag size={13} />
  {:else if name === "move"}<span class="w-[13px] text-center" aria-hidden="true">→</span>
  {:else if name === "priority"}<span class="w-[13px] text-center" aria-hidden="true">!</span>
  {:else if name === "select"}<SquareCheck size={13} />
  {:else if name === "add"}<Plus size={13} />
  <!-- The same glyph the board header's Archive toggle carries, so the row
       and the panel it files into read as one thing. -->
  {:else if name === "archive"}<Archive size={13} />
  {:else if name === "delete"}<Trash2 size={13} class={danger ? "text-rose-400" : ""} />
  {/if}
{/snippet}

<style>
  /* Reserve the checkmark column so labels stay aligned whether or not the
     row is currently marked — a value list that shifts sideways as the mark
     moves is unreadable at a glance. */
  .mark{width:13px;display:inline-flex;align-items:center;justify-content:center;flex-shrink:0}
</style>
