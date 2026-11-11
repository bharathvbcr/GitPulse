<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import { harnessStore } from "../stores/harnessStore";
  import { handleTablistKeydown } from "../dom/tablist";
  import { activeSectionFor, sectionsFor, VIEW_PANE_ID } from "../views/viewRegistry";
  import { destinationGuide, tipGuideDescId, tipGuideKey } from "../views/viewGuide";
  import type { ViewTab } from "../repos/persist";
  import { formatViewTabLabel, type ViewNavItem } from "../views/viewNav";
  import { visibleViewNav } from "../views/viewVisibility";
  import { sectionIcon, VIEW_ICONS } from "../views/viewIcons";
  import { sectionAttention, viewAttention, type AttentionContext } from "../views/viewAttention";
  import { interfaceStore } from "../stores/interfaceStore";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";
  import { portal } from "../dom/portal";
  import { popover, popoverExit } from "../ui/popover";
  import { LAYERS } from "../ui/layers";
  import ViewSectionTabs from "./ViewSectionTabs.svelte";

  const macos = isMacOS();
  // Svelte owns interruption and element cleanup. Only the decorative pill
  // moves; labels and focus targets stay in place, even during rapid changes.
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  /**
   * The header view switcher: one group per view.
   *
   * The active view's group is expanded: its pill, then its sections as icon
   * tabs (`ViewSectionTabs`), which used to be a second full-width row under
   * the header. Every other view is a collapsed pill. Hovering one opens a
   * flyout of its sections, so any destination is one pointer move and one
   * click away; clicking the pill itself opens the section that view was last
   * left on (`activeSectionFor` remembers it per view).
   *
   * Three choices keep the pattern from going wrong the usual ways:
   *
   * - Nothing re-renders under the pointer. The hovered group never changes
   *   size; its sections open in a portaled flyout, so the groups beside it
   *   never move and the click lands on what the reader aimed at. That is the
   *   whole class of bug a pointerdown-then-click de-duplication works around.
   * - Hover has one meaning. A collapsed pill's flyout already says what the
   *   view is, so that pill carries no destination guide of its own; a guide
   *   card opening over the flyout would be two popups for one gesture. The
   *   section icons keep theirs.
   * - The keyboard does not need the flyout. Left/Right move between views
   *   (selection follows focus, as before) and Down enters the active view's
   *   sections; the flyout is a pointer shortcut to destinations the keyboard
   *   already reaches.
   *
   * The menu this replaced: a portalled click dropdown for views that would
   * not fit a title bar, with dismissal hand-rolled four times over. Views
   * still never hide behind a click. The flyout is a preview of a pill that is
   * already on screen, and its dismissal is the shared popover owner's.
   */

  let { conflictedCount = 0 }: { conflictedCount?: number } = $props();

  let scroller: HTMLDivElement | undefined = $state();
  let sectionTabs: ViewSectionTabs | undefined = $state();

  let activeTab = $derived($repoStore.activeTab);
  // The header lists only the views the user kept, plus the ones visibility
  // pins on regardless (the active view, and Work while conflicts stand).
  let navItems = $derived(
    visibleViewNav($interfaceStore.hiddenViews, { activeTab, conflictedCount }),
  );
  let attention: AttentionContext = $derived({
    conflictedCount,
    verdictStatus: $harnessStore.lastVerdict?.status ?? null,
  });

  function selectTab(tab: ViewTab, section?: string) {
    closeFlyout();
    interfaceStore.setGlobalSurface("repository");
    repoStore.setActiveTab(tab, section);
  }

  // Resolve is a section of Work now, so the warning colour follows the
  // conflicts rather than the tab: Work only turns amber while files actually
  // carry markers. Colouring Work amber whenever it is active would make the
  // signal mean "you are on Work", which is not a warning at all.
  function tabClass(item: ViewNavItem, active: boolean): string {
    const warning = item.id === "work" && conflictedCount > 0;
    if (active && warning) {
      return "text-warn!";
    }
    return "";
  }

  function focusViewButton(index: number) {
    scroller?.querySelectorAll<HTMLElement>("[data-view-button]")[index]?.focus();
  }

  /**
   * Arrow keys move between views, and only the selected view is in the tab
   * order. Down enters the active view's sections; Up from a section comes
   * back here (`ViewSectionTabs`' `onExitUp`).
   */
  function onKeydown(event: KeyboardEvent) {
    if (!(event.target instanceof HTMLElement) || !event.target.hasAttribute("data-view-button")) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      sectionTabs?.focusActive();
      return;
    }
    const current = navItems.findIndex((item) => item.id === activeTab);
    const move = handleTablistKeydown(event.key, current, navItems.length);
    if (!move) return;
    event.preventDefault();
    const target = navItems[move.index];
    if (!target) return;
    selectTab(target.id);
    focusViewButton(move.index);
  }

  // ---- the hover flyout ------------------------------------------------------
  // Intent before it opens, grace before it closes: a pointer crossing the
  // header on its way somewhere else should not flash four panels, and the gap
  // between a pill and its flyout must not close the flyout under the pointer.
  const OPEN_DELAY_MS = 120;
  const CLOSE_GRACE_MS = 220;

  let flyoutView: ViewTab | null = $state(null);
  let flyoutAnchor: HTMLElement | null = $state(null);
  let openTimer: ReturnType<typeof setTimeout> | undefined;
  let closeTimer: ReturnType<typeof setTimeout> | undefined;

  function clearTimers() {
    clearTimeout(openTimer);
    clearTimeout(closeTimer);
    openTimer = closeTimer = undefined;
  }

  function closeFlyout() {
    clearTimers();
    flyoutView = null;
    flyoutAnchor = null;
  }

  /** Touch has no hover: a tap is a click, which opens the view. */
  function isHoverPointer(event: PointerEvent): boolean {
    return event.pointerType === "mouse" || event.pointerType === "pen";
  }

  function onGroupEnter(event: PointerEvent, view: ViewTab) {
    if (!isHoverPointer(event)) return;
    clearTimers();
    if (view === activeTab || sectionsFor(view).length < 2) {
      if (flyoutView) closeTimer = setTimeout(closeFlyout, CLOSE_GRACE_MS);
      return;
    }
    const anchor = event.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    if (flyoutView) {
      // Already reading flyouts: move straight to this one.
      flyoutView = view;
      flyoutAnchor = anchor;
      return;
    }
    openTimer = setTimeout(() => {
      flyoutView = view;
      flyoutAnchor = anchor;
    }, OPEN_DELAY_MS);
  }

  function onGroupLeave(event: PointerEvent) {
    if (!isHoverPointer(event)) return;
    clearTimeout(openTimer);
    clearTimeout(closeTimer);
    closeTimer = setTimeout(closeFlyout, CLOSE_GRACE_MS);
  }

  function onFlyoutEnter() {
    clearTimers();
  }

  function onFlyoutKeydown(event: KeyboardEvent) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeFlyout();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const items = [...(event.currentTarget as HTMLElement).querySelectorAll<HTMLElement>(".gp-view-flyout-item")];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const next = event.key === "ArrowDown" ? (at + 1) % items.length : (at - 1 + items.length) % items.length;
    event.preventDefault();
    items[next]?.focus();
  }

  const flyoutPlacement = $derived({
    anchor: { kind: "element" as const, element: flyoutAnchor, gap: 6 },
    estimate: { width: 248, height: 220 },
    dismiss: {
      inside: "[data-view-group], [data-view-flyout]",
      pointer: "pointerdown" as const,
      scroll: true,
      resize: true,
      escape: "none" as const,
    },
    onDismiss: () => closeFlyout(),
    revision: flyoutView,
  });

  // A view change (from anywhere: palette, native menu, a link) retires any
  // flyout that was open for the view that just became active.
  $effect(() => {
    if (flyoutView && flyoutView === activeTab) closeFlyout();
  });

  $effect(() => () => clearTimers());

  $effect(() => {
    activeTab;
    const active = scroller?.querySelector("[data-active-view='true']");
    if (active instanceof HTMLElement) {
      active.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

<!-- The `nav "Views"` landmark is the view bar that hosts this switcher
     (ViewBar.svelte): the switcher is one part of it, beside the pills. -->
<div class="relative shrink-0" data-tour="views">
  <div class="sr-only">
    {#each navItems as item (item.id)}
      {@const guide = destinationGuide(tipGuideKey(item.id))}
      {#if guide}
        <span id={tipGuideDescId(tipGuideKey(item.id))}>{guide.summary}</span>
      {/if}
    {/each}
  </div>
  <!-- Justified: one delegated keydown for the view buttons' roving arrows;
       it acts only when a view button is the target. -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    bind:this={scroller}
    class="flex items-center gap-1.5 shrink-0"
    onkeydown={onKeydown}
  >
    {#each navItems as item, index (item.id)}
      {@const active = activeTab === item.id}
      {@const guideKey = tipGuideKey(item.id)}
      {@const guide = destinationGuide(guideKey)}
      {@const Icon = VIEW_ICONS[item.id]}
      {@const hasSections = sectionsFor(item.id).length > 1}
      {@const flag = active ? null : viewAttention(item.id, sectionsFor(item.id).map((s) => s.id), attention)}
      <!-- Justified: pointer enter/leave only drive the hover preview; the
           view button inside is the control, and the keyboard needs no flyout. -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="gp-segmented"
        class:gp-liquid-tabs={macos}
        data-view-group={item.id}
        data-active={active ? "true" : "false"}
        onpointerenter={(event) => onGroupEnter(event, item.id)}
        onpointerleave={onGroupLeave}
      >
        <button
          type="button"
          data-view-button
          aria-current={active ? "page" : undefined}
          aria-controls={active ? VIEW_PANE_ID : undefined}
          aria-describedby={guide ? tipGuideDescId(guideKey) : undefined}
          aria-label={flag ? `${formatViewTabLabel(item, conflictedCount)} — ${flag.reason}` : undefined}
          aria-haspopup={!active && hasSections ? "true" : undefined}
          aria-expanded={!active && hasSections ? flyoutView === item.id : undefined}
          tabindex={active || (index === 0 && !navItems.some((n) => n.id === activeTab)) ? 0 : -1}
          data-active-view={active ? "true" : "false"}
          data-active={active ? "true" : "false"}
          data-tip-guide={active ? guideKey : undefined}
          onclick={() => selectTab(item.id)}
          class="gp-seg-btn relative inline-flex items-center gap-1.5 {tabClass(item, active)}"
        >
          {#if macos && active}
            <span
              class="gp-liquid-selection gp-gpu"
              aria-hidden="true"
              in:receiveSelection={{ key: "active-view" }}
              out:sendSelection={{ key: "active-view" }}
            ></span>
          {/if}
          <Icon size={13} aria-hidden="true" class="relative shrink-0" />
          <span>{formatViewTabLabel(item, conflictedCount)}</span>
          {#if flag}
            <b
              class="gp-attention-badge {flag.tone === 'rose' ? 'bg-danger' : 'bg-warn'} {flag.count === null ? 'gp-attention-dot' : ''}"
              aria-hidden="true">{flag.count ?? ""}</b
            >
          {/if}
        </button>
        {#if active}
          <ViewSectionTabs
            bind:this={sectionTabs}
            view={item.id}
            {attention}
            onExitUp={() => focusViewButton(index)}
          />
        {/if}
      </div>
    {/each}
  </div>
</div>

{#if flyoutView && flyoutAnchor}
  {@const view = flyoutView}
  {@const item = navItems.find((n) => n.id === view)}
  {@const guide = destinationGuide(tipGuideKey(view))}
  {@const remembered = activeSectionFor(view, $repoStore.viewSections)}
  {@const Icon = VIEW_ICONS[view]}
  <!-- Justified: keydown moves focus among its own buttons and Escape closes
       it; the pointer handlers keep it open while it is under the pointer. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <div
    use:portal={"body"}
    use:popover={flyoutPlacement} out:popoverExit
    data-view-flyout={view}
    role="group"
    aria-label="{item?.label ?? view} sections"
    onpointerenter={onFlyoutEnter}
    onpointerleave={onGroupLeave}
    onkeydown={onFlyoutKeydown}
    class="fixed w-62 gp-menu gp-pop p-1.5 text-xs text-textPrimary"
    style="z-index: {LAYERS.MENU}"
  >
    <button type="button" class="gp-view-flyout-item" onclick={() => selectTab(view)}>
      <Icon size={14} class="shrink-0 text-accent" aria-hidden="true" />
      <span class="min-w-0">
        <span class="block font-semibold">{item?.label ?? view}</span>
        {#if guide}<span class="block text-meta leading-snug text-textMuted whitespace-normal">{guide.summary}</span>{/if}
      </span>
    </button>
    <div class="gp-separator my-1" aria-hidden="true"></div>
    {#each sectionsFor(view) as section (section.id)}
      {@const SectionIcon = sectionIcon(view, section.id)}
      {@const flag = sectionAttention(view, section.id, attention)}
      <button
        type="button"
        class="gp-view-flyout-item"
        data-section={section.id}
        data-remembered={section.id === remembered ? "true" : "false"}
        onclick={() => selectTab(view, section.id)}
      >
        <SectionIcon size={13} class="shrink-0 text-textMuted" aria-hidden="true" />
        <span class="flex-1 min-w-0 truncate">{section.label}</span>
        {#if flag}
          <span class="shrink-0 rounded-full px-1.5 text-micro font-semibold text-white {flag.tone === 'rose' ? 'bg-danger' : 'bg-warn'}" title={flag.reason}
            >{flag.count ?? "!"}</span
          >
        {/if}
      </button>
    {/each}
  </div>
{/if}
