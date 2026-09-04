/**
 * The Settings modal's category rail.
 *
 * One catalog so the rail, the panel switch and the contract test all derive
 * from the same list: adding a category here and its panel branch is the whole
 * change, and a category without a panel fails the test rather than rendering
 * an empty pane.
 */
export const SETTINGS_SECTION_IDS = [
  "appearance",
  "layout",
  "views",
  "graph",
  "analysis",
  "agents",
  "updates",
] as const;

export type SettingsSectionId = (typeof SETTINGS_SECTION_IDS)[number];

export interface SettingsSection {
  readonly id: SettingsSectionId;
  /** Rail entry and panel heading. */
  readonly label: string;
  /** One line under the panel heading saying what the category covers. */
  readonly summary: string;
}

export const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  {
    id: "appearance",
    label: "Appearance",
    summary: "Theme and how large the interface draws.",
  },
  {
    id: "layout",
    label: "Layout",
    summary: "Which parts of the window frame stay on screen.",
  },
  {
    id: "views",
    label: "Views",
    summary: "Which views the header lists. Hidden ones stay in ⌘K.",
  },
  {
    id: "graph",
    label: "Commit graph",
    summary: "Lane spacing, graph width and the author gutter.",
  },
  {
    id: "analysis",
    label: "Analysis",
    summary: "Work GitPulse may run against the repository itself.",
  },
  {
    id: "agents",
    label: "Agents",
    summary: "The read-only MCP surface agents connect through.",
  },
  {
    id: "updates",
    label: "Updates",
    summary: "Whether GitPulse checks its own repository for releases.",
  },
];

export function isSettingsSectionId(value: unknown): value is SettingsSectionId {
  return (
    typeof value === "string" &&
    (SETTINGS_SECTION_IDS as readonly string[]).includes(value)
  );
}
