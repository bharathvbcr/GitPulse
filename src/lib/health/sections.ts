/**
 * The Health view's section catalog.
 *
 * Two things used to be decided independently and could disagree: the order
 * the panel rendered its sections in, and (once there was a jump nav) the
 * order the nav listed them in. They are one decision, so they are one list.
 *
 * The order here is the fix for the panel's information architecture. It
 * previously ran cap-notice → Issues → Packages → Vulnerabilities → …, which
 * put an inventory of package manifests above the findings and pushed the one
 * high-severity vulnerability off the first screen. Findings come first now
 * and inventory last.
 *
 * The panel does not iterate this list — each section's content is different
 * enough that driving it from a table would cost more indirection than it
 * saves. It declares the order, and two guards enforce that the markup agrees:
 * `HealthPanel.test.ts` checks the source's section order against this list,
 * and the browser harness checks the rendered `top` coordinates. A divergence
 * fails both, so the declared order cannot quietly stop being the real one.
 */

export interface HealthSectionSpec {
  /** DOM id of the section, and the jump target of its summary chip. */
  id: string;
  /** Short form, for the jump chip where horizontal room is scarce. */
  label: string;
  /**
   * Full form, for the section's own `<h3>`. Separate from `label` because
   * "Outdated" is the right chip and "Outdated npm packages" is the right
   * heading — the scope qualifier matters once you are reading the table, and
   * costs a line wrap in a chip row.
   */
  heading: string;
  /**
   * What this section answers, for the section's accessible description.
   * Short: it is read out before the section's contents.
   */
  summary: string;
}

/**
 * One content width for every section.
 *
 * The panel previously mixed `max-w-2xl`, `3xl`, `4xl` and `5xl` across
 * sibling sections, so the right edge stepped in and out down the page and
 * nothing lined up with anything. The widest thing here is a five-column
 * table, so that is the width, and it is stated once.
 */
export const HEALTH_CONTENT_WIDTH = "max-w-5xl";

/** Narrower measure for running prose, which is unreadable at full width. */
export const HEALTH_PROSE_WIDTH = "max-w-3xl";

/**
 * Rendered order, findings first.
 *
 * `plan` is the MANVI remediation plan. It sits above the findings because it
 * only exists after the reader asked for it, and it is what they are waiting
 * on when it does.
 */
export const HEALTH_SECTIONS: readonly HealthSectionSpec[] = Object.freeze([
  {
    id: "summary",
    label: "Summary",
    heading: "Summary",
    summary: "The verdict for this scan, and what it could not establish.",
  },
  {
    id: "plan",
    label: "Plan",
    heading: "MANVI remediation plan",
    summary: "The remediation plan generated for this report, and its steps.",
  },
  {
    id: "vulnerabilities",
    label: "Vulnerabilities",
    heading: "Vulnerabilities",
    summary: "Known advisories against this repository's dependencies.",
  },
  {
    id: "dependabot",
    label: "Dependabot",
    heading: "GitHub Dependabot",
    summary: "Open Dependabot alerts fetched from GitHub.",
  },
  {
    id: "code-scanning",
    label: "Code scanning",
    heading: "GitHub Code Scanning",
    summary: "Open GitHub code scanning alerts.",
  },
  {
    id: "issues",
    label: "Issues",
    heading: "Issues",
    summary: "Repository configuration findings from the local scan.",
  },
  {
    id: "outdated",
    label: "Outdated",
    heading: "Outdated npm packages",
    summary: "npm packages behind their latest published release.",
  },
  {
    id: "packages",
    label: "Packages",
    heading: "Packages",
    summary: "Package manifests and the other ecosystems detected here.",
  },
  {
    id: "code-graph",
    label: "Code graph",
    heading: "Code graph",
    summary: "Whether this repository has an indexed code graph, and its size.",
  },
  {
    id: "dead-code",
    label: "Dead code",
    heading: "Dead-code candidates",
    summary: "Symbols the code graph found no reference to.",
  },
]);

/** Section ids in rendered order. */
export const HEALTH_SECTION_IDS: readonly string[] = Object.freeze(
  HEALTH_SECTIONS.map((section) => section.id),
);

/** DOM id of a section's region, used by both the section and its jump chip. */
export function healthSectionDomId(id: string): string {
  return `health-section-${id}`;
}

/** DOM id of a section's heading, for `aria-labelledby`. */
export function healthSectionHeadingId(id: string): string {
  return `health-heading-${id}`;
}

/** Catalog lookup. Returns null for an id the catalog does not carry. */
export function healthSection(id: string): HealthSectionSpec | null {
  return HEALTH_SECTIONS.find((section) => section.id === id) ?? null;
}
