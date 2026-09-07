import { isViewTab, type ViewTab } from "../repos/persist";
import { VIEW_REGISTRY, sectionsFor } from "./viewRegistry";
import { sectionAccelerator, viewAccelerator } from "./viewShortcuts";

/**
 * Glanceable destination cards for the header and section bars.
 *
 * Copy lives on the view registry (one catalog). This module only assembles
 * it with the shortcut table and the chip list the tooltip paints, so a
 * renamed section cannot leave the card advertising a pane that is gone.
 */

export interface GuideChip {
  readonly id: string;
  readonly label: string;
  readonly active: boolean;
}

export interface DestinationGuide {
  readonly key: string;
  readonly view: ViewTab;
  readonly section: string | null;
  readonly title: string;
  readonly summary: string;
  readonly shortcut: string | null;
  readonly chips: readonly GuideChip[];
}

/** Attribute value written on a view or section tab. */
export function tipGuideKey(view: ViewTab, section?: string | null): string {
  return section ? `${view}:${section}` : view;
}

/** Stable id for the visually-hidden description a tab points at. */
export function tipGuideDescId(key: string): string {
  return `gitpulse-view-guide-${key.replace(/[^a-z0-9-]+/gi, "-")}`;
}

export function parseTipGuideKey(
  key: string,
): { view: ViewTab; section: string | null } | null {
  const trimmed = key.trim();
  if (!trimmed) return null;
  const colon = trimmed.indexOf(":");
  if (colon === -1) {
    return isViewTab(trimmed) ? { view: trimmed, section: null } : null;
  }
  const view = trimmed.slice(0, colon);
  const section = trimmed.slice(colon + 1);
  if (!isViewTab(view) || section.length === 0) return null;
  return { view, section };
}

/**
 * The card for a `data-tip-guide` key, or null when the key names nothing
 * this build still offers.
 */
export function destinationGuide(key: string): DestinationGuide | null {
  const parsed = parseTipGuideKey(key);
  if (!parsed) return null;
  const { view, section } = parsed;
  const registration = VIEW_REGISTRY[view];
  const sections = sectionsFor(view);
  if (!section) {
    return {
      key,
      view,
      section: null,
      title: registration.label,
      summary: registration.summary,
      shortcut: viewAccelerator(view),
      chips: sections.map((entry) => ({
        id: entry.id,
        label: entry.label,
        active: false,
      })),
    };
  }
  const index = sections.findIndex((entry) => entry.id === section);
  const found = sections[index];
  if (!found) return null;
  return {
    key,
    view,
    section,
    title: found.label,
    summary: found.summary,
    shortcut: sectionAccelerator(index),
    chips: sections.map((entry) => ({
      id: entry.id,
      label: entry.label,
      active: entry.id === section,
    })),
  };
}
