<script lang="ts">
  /**
   * Title-bar Open/Clone control.
   *
   * Two neighbour pills cost a second hit target for an action most sessions
   * never use. One menu keeps both, and the Layout setting still drops the
   * word on the trigger. The menu is portaled: the title bar clips overflow
   * (`.gp-header-scroll` is overflow-x auto / overflow-y hidden) so a nested
   * dropdown would paint inside the 40px strip.
   */
  import { ChevronDown, Download, FolderOpen } from "@lucide/svelte";
  import { interfaceStore } from "../stores/interfaceStore";
  import { portal } from "../dom/portal";
  import { cycleFocus, enumerateFocusables } from "../ui/focusTrap";
  import { LAYERS } from "../ui/layers";
  import { popover, restoreFocusTo } from "../ui/popover";

  let {
    onOpen,
    onClone,
  }: {
    onOpen: () => void;
    onClone: () => void;
  } = $props();

  let open = $state(false);
  let triggerEl: HTMLButtonElement | undefined = $state();
  let menuEl: HTMLDivElement | undefined = $state();

  function close(options?: { restoreFocus?: boolean }) {
    const opener = triggerEl;
    open = false;
    if (options?.restoreFocus) restoreFocusTo(opener);
  }

  function openMenu() {
    open = true;
  }

  /**
   * Placement and dismissal, from the shared popover owner. The title bar
   * clips overflow (`.gp-header-scroll` is overflow-x auto / overflow-y
   * hidden), so the menu is portaled and positioned against the trigger
   * rather than nested under it.
   *
   * Escape bubbles here, as a fallback for focus that has left the menu;
   * `onMenuKey` below handles it first — and stops propagation — whenever the
   * menu itself has focus, which is the usual case.
   */
  const dismissal = $derived({
    anchor: { kind: "element" as const, element: triggerEl, gap: 6 },
    estimate: { width: 176, height: 88 },
    dismiss: {
      inside: "[data-header-repo-menu], [data-header-repo-menu-popup]",
      resize: true,
      escape: "bubble" as const,
    },
    onDismiss: (reason: string) => close({ restoreFocus: reason === "escape" }),
  });

  function toggle() {
    if (open) close();
    else openMenu();
  }

  function choose(action: () => void) {
    close();
    action();
  }

  function focusPopup() {
    window.setTimeout(() => {
      menuEl?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }, 0);
  }

  function focusAdjacentToTrigger(popup: HTMLElement, backwards: boolean) {
    const opener = triggerEl ?? null;
    const candidates = enumerateFocusables<HTMLElement>(document).filter(
      (candidate) =>
        candidate.tabIndex >= 0 &&
        !popup.contains(candidate) &&
        candidate.getClientRects().length > 0,
    );
    if (candidates.length === 0) return;

    const openerIndex = opener ? candidates.indexOf(opener) : -1;
    let target = openerIndex >= 0
      ? candidates[openerIndex + (backwards ? -1 : 1)]
      : undefined;
    if (!target && opener?.isConnected) {
      const direction = backwards
        ? Node.DOCUMENT_POSITION_PRECEDING
        : Node.DOCUMENT_POSITION_FOLLOWING;
      const ordered = backwards ? [...candidates].reverse() : candidates;
      target = ordered.find((candidate) => opener.compareDocumentPosition(candidate) & direction);
    }
    target ??= candidates[backwards ? candidates.length - 1 : 0];
    window.setTimeout(() => target?.focus(), 0);
  }

  $effect(() => {
    if (open && menuEl) focusPopup();
  });

  function onTriggerKey(event: KeyboardEvent) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (!open) openMenu();
      else focusPopup();
    }
  }

  function onMenuKey(event: KeyboardEvent) {
    event.stopPropagation();
    if (event.key === "Tab") {
      event.preventDefault();
      const popup = event.currentTarget;
      if (popup instanceof HTMLElement) {
        focusAdjacentToTrigger(popup, event.shiftKey);
      }
      close();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      close({ restoreFocus: true });
      return;
    }
    if (!menuEl) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      cycleFocus(menuEl, true);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      cycleFocus(menuEl, false);
    } else if (event.key === "Home") {
      event.preventDefault();
      menuEl.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    } else if (event.key === "End") {
      event.preventDefault();
      const items = menuEl.querySelectorAll<HTMLElement>('[role="menuitem"]');
      items[items.length - 1]?.focus();
    }
  }

</script>

<div class="relative shrink-0" data-header-repo-menu>
  <button
    bind:this={triggerEl}
    type="button"
    class="gp-btn py-1! shrink-0"
    title="Open or clone a repository"
    aria-label="Open or clone a repository"
    aria-haspopup="menu"
    aria-expanded={open}
    aria-controls="header-repo-menu"
    data-testid="header-repo-menu"
    data-tour="repository"
    onclick={toggle}
    onkeydown={onTriggerKey}
  >
    <FolderOpen size={13} class="text-accent" />
    {#if $interfaceStore.showHeaderActionLabels}<span>Open</span>{/if}
    <ChevronDown size={11} class="text-textMuted" aria-hidden="true" />
  </button>
</div>

{#if open}
  <div
    bind:this={menuEl}
    use:portal={"body"}
    use:popover={dismissal}
    id="header-repo-menu"
    data-header-repo-menu-popup
    role="menu"
    aria-label="Open or clone a repository"
    tabindex="-1"
    onkeydown={onMenuKey}
    class="fixed min-w-44 gp-menu gp-pop text-xs text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <button
      type="button"
      role="menuitem"
      class="gp-menu-item"
      onclick={() => choose(onOpen)}
    >
      <FolderOpen size={13} class="text-accent" />
      Open Repository…
    </button>
    <button
      type="button"
      role="menuitem"
      class="gp-menu-item"
      onclick={() => choose(onClone)}
    >
      <Download size={13} class="text-accent" />
      Clone Repository…
    </button>
  </div>
{/if}
