<script lang="ts">
  import { repoStore } from "../stores/repoStore";
  import type { ViewTab } from "../repos/persist";
  import { activeSectionFor, sectionsFor, VIEW_REGISTRY } from "../views/viewRegistry";
  import { destinationGuide, tipGuideDescId, tipGuideKey } from "../views/viewGuide";
  import { focusTabAt, handleTablistKeydown, tabProps } from "../dom/tablist";
  import { sectionIcon } from "../views/viewIcons";
  import { sectionAttention, type AttentionContext } from "../views/viewAttention";
  import { crossfade } from "svelte/transition";
  import { isMacOS } from "../platform";
  import { liquidSelection } from "../ui/transitions";

  const macos = isMacOS();
  const [sendSelection, receiveSelection] = crossfade(liquidSelection());

  /**
   * The active view's sections, drawn in the header beside the view itself.
   *
   * They used to be a second, full-width row under the header. Now the header
   * reads as one switcher: each view a pill, the active one expanded into its
   * sections, so a lens change and a view change are the same gesture in the
   * same place and the pane gets the row back.
   *
   * Only the ACTIVE view's sections render here, and they are real tabs: each
   * points at the `ViewSectionPanel` that exists for it, and the panel points
   * back. A hovered view's sections are in the header's flyout as plain
   * buttons, because their panels do not exist until that view opens.
   *
   * Icons with the active section's label: the label says where the reader
   * is, the icons say what else is here, and each icon's destination guide
   * says what it is on hover.
   *
   * No track of its own: this sits inside the header group's segmented
   * track, which carries `gp-liquid-tabs` on the Mac, so the tabs take the
   * liquid styling from there. A second track here drew a box inside the box
   * and made the active view's pill taller than the collapsed ones.
   */

  let {
    view,
    attention,
    /** Arrow-up from a section hands focus back to the view's own button. */
    onExitUp,
  }: {
    view: ViewTab;
    attention: AttentionContext;
    onExitUp?: () => void;
  } = $props();

  const sections = $derived(sectionsFor(view));
  const active = $derived(activeSectionFor(view, $repoStore.viewSections));
  const activeIndex = $derived(Math.max(0, sections.findIndex((s) => s.id === active)));
  const groupLabel = $derived(VIEW_REGISTRY[view].label);

  let list: HTMLDivElement | undefined = $state();

  /** Focus the selected section; the header's ArrowDown lands here. */
  export function focusActive() {
    focusTabAt(list, activeIndex);
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "ArrowUp" && onExitUp) {
      event.preventDefault();
      onExitUp();
      return;
    }
    const move = handleTablistKeydown(event.key, activeIndex, sections.length);
    if (!move) return;
    event.preventDefault();
    const target = sections[move.index];
    if (!target) return;
    repoStore.setViewSection(view, target.id);
    // Selection follows focus, and focus has to be moved explicitly: roving
    // tabindex changes which tab is tabbable, not where focus sits.
    focusTabAt(list, move.index);
  }

  $effect(() => {
    active;
    const el = list?.querySelector("[data-active='true']");
    if (el instanceof HTMLElement) {
      el.scrollIntoView({ block: "nearest", inline: "nearest" });
    }
  });
</script>

{#if sections.length > 1}
  <div class="sr-only">
    {#each sections as section (section.id)}
      {@const guide = destinationGuide(tipGuideKey(view, section.id))}
      {#if guide}
        <span id={tipGuideDescId(tipGuideKey(view, section.id))}>{guide.summary}</span>
      {/if}
    {/each}
  </div>
  <div
    bind:this={list}
    class="gp-section-tabs flex items-center gap-0.5"
    role="tablist"
    aria-label="{groupLabel} sections"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    {#each sections as section (section.id)}
      {@const isActive = active === section.id}
      {@const props = tabProps(view, section.id, isActive)}
      {@const guideKey = tipGuideKey(view, section.id)}
      {@const guide = destinationGuide(guideKey)}
      {@const Icon = sectionIcon(view, section.id)}
      {@const flag = sectionAttention(view, section.id, attention)}
      <button
        type="button"
        role={props.role}
        id={props.id}
        aria-selected={props["aria-selected"]}
        aria-controls={props["aria-controls"]}
        aria-describedby={guide ? tipGuideDescId(guideKey) : undefined}
        aria-label={flag ? `${section.label} — ${flag.reason}` : section.label}
        tabindex={props.tabindex}
        data-active={isActive ? "true" : "false"}
        data-section={section.id}
        data-tip-guide={guideKey}
        onclick={() => repoStore.setViewSection(view, section.id)}
        class="gp-seg-btn gp-section-tab relative inline-flex h-[26px] items-center gap-1 text-meta! py-0! {isActive ? 'px-2!' : 'px-1.5!'}"
      >
        {#if macos && isActive}
          <span
            class="gp-liquid-selection gp-gpu"
            aria-hidden="true"
            in:receiveSelection={{ key: `active-section-${view}` }}
            out:sendSelection={{ key: `active-section-${view}` }}
          ></span>
        {/if}
        <!-- `relative`: on the Mac the selection pill is absolutely placed and
             would otherwise paint over a static icon. -->
        <Icon size={13} aria-hidden="true" class="relative shrink-0" />
        {#if isActive}<span>{section.label}</span>{/if}
        {#if flag}
          <!-- A <b>, not a <span>: the Mac liquid tabs make every span in a
               tab position:relative, which would pull the badge into the row. -->
          <b
            class="gp-attention-badge {flag.tone === 'rose' ? 'bg-danger' : 'bg-warn'} {flag.count === null ? 'gp-attention-dot' : ''}"
            aria-hidden="true">{flag.count ?? ""}</b
          >
        {/if}
      </button>
    {/each}
  </div>
{/if}
