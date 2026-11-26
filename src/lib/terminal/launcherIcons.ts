/**
 * The glyph and tint for each kind of terminal-dock tab, so a shell, a
 * Claude session and a browser pane read apart at a glance in the tab strip,
 * the session lists and the new-session menu.
 *
 * The agent CLIs carry their vendors' own marks (`BrandMark.svelte`): Claude,
 * Codex, Grok and Antigravity. Manvi carries its own too, and a plain shell
 * carries GitPulse's, all from the same table. The rest (a browser, a file)
 * are generic glyphs, where the tint carries most of the recognition. Every
 * surface that draws one also says the kind in text, so the icon is
 * decorative.
 */

import { FileText, Globe } from "@lucide/svelte";
import type { BrandMarkKind } from "../components/BrandMark.svelte";
import type { TabKind } from "./tabs";

export interface LauncherIconSpec {
  /** A generic glyph, or null when the kind has a mark of its own (`mark`). */
  icon: typeof Globe | null;
  /** A one-path mark drawn by `BrandMark.svelte` instead of `icon`. */
  mark?: BrandMarkKind;
  /** A CSS color that reads on light and dark surfaces. */
  tint: string;
}

export const LAUNCHER_ICONS: Readonly<Record<TabKind, LauncherIconSpec>> = {
  // GitPulse's own prompt-and-pulse glyph, in the muted text colour: a shell
  // is the unbranded kind, so it must not out-shout the agents beside it.
  shell: { icon: null, mark: "shell", tint: "var(--color-textMuted)" },
  // Claude's own mark in its brand color.
  claude: { icon: null, mark: "claude", tint: "#d97757" },
  // Manvi's published Fold M mark draws in its own colours; the tint is Red400,
  // the accent its brand ramp uses where the red has to read as text.
  manvi: { icon: null, mark: "manvi", tint: "#d92d3f" },
  // Codex's blue-violet and Antigravity's blue are from their own marks;
  // Grok's mark is monochrome, so it takes the text color.
  codex: { icon: null, mark: "codex", tint: "#7a9dff" },
  grok: { icon: null, mark: "grok", tint: "var(--color-textPrimary)" },
  agy: { icon: null, mark: "agy", tint: "#3186ff" },
  browser: { icon: Globe, tint: "#0ea5e9" },
  file: { icon: FileText, tint: "#d97706" },
};

export function launcherIcon(kind: TabKind | null | undefined): LauncherIconSpec {
  return (kind && LAUNCHER_ICONS[kind]) || LAUNCHER_ICONS.shell;
}
