/**
 * Terminal chords for the global keyboard-shortcuts guide.
 *
 * These are the bindings the dock's shortcuts popover listed, and they are
 * the ones the terminal actually handles: tab chords are Control on every
 * host (`terminalTabChord`), and find / text size are Command on macOS and
 * Control+Shift elsewhere (`terminalViewChord`) because plain Control belongs
 * to the shell. Nothing here is invented.
 *
 * One row is the app's, not the terminal's: the repository-tab digits are a
 * native menu accelerator, consumed before xterm sees them, so they work with
 * a shell focused. The terminal claims no Command/Control+digit of its own
 * (`shortcutGuide.test.ts` holds both chord classifiers to that).
 */

import type { HostOS } from "../platform";
import { shortcutKeyLabel } from "../ui/platformCopy";

export interface GuideShortcut {
  keys: string[];
  description: string;
}

export function terminalGuideShortcuts(os: HostOS): GuideShortcut[] {
  const mac = os === "macos";
  // On macOS the mapper returns these glyphs unchanged; going through it keeps
  // the platform-vocabulary guard satisfied about where a glyph may be spelled.
  const cmd = shortcutKeyLabel("⌘", os);
  const opt = shortcutKeyLabel("⌥", os);
  const rows: GuideShortcut[] = [
    { keys: ["Ctrl", "Shift", "T"], description: "New shell in the directory of the shell beside it" },
    { keys: ["Ctrl", "Shift", "W"], description: "Close session — asks first when a job is running" },
    { keys: ["Ctrl", "Tab"], description: "Next terminal tab" },
    { keys: ["Ctrl", "Shift", "Tab"], description: "Previous terminal tab" },
    { keys: [cmd, "1–9"], description: "Jump to a repository tab, from the terminal too; 9 is the last" },
    { keys: mac ? [cmd, "F"] : ["Ctrl", "Shift", "F"], description: "Find in the terminal" },
    { keys: ["Enter"], description: "Next match" },
    { keys: ["Shift", "Enter"], description: "Previous match" },
    { keys: ["Esc"], description: "Close find" },
    { keys: mac ? [cmd, "+"] : ["Ctrl", "Shift", "+"], description: "Larger terminal text" },
    { keys: mac ? [cmd, "−"] : ["Ctrl", "Shift", "−"], description: "Smaller terminal text" },
    { keys: mac ? [cmd, "0"] : ["Ctrl", "Shift", "0"], description: "Reset terminal text size" },
    { keys: ["←", "→"], description: "Previous / next tab while the tab strip is focused" },
    { keys: ["Home", "End"], description: "First / last tab while the tab strip is focused" },
  ];
  if (mac) {
    rows.push(
      { keys: [cmd, "←"], description: "Start of the line" },
      { keys: [cmd, "→"], description: "End of the line" },
      { keys: [opt, "←"], description: "Back a word" },
      { keys: [opt, "→"], description: "Forward a word" },
      { keys: [cmd, "⌫"], description: "Delete to the start of the line" },
    );
  }
  return rows;
}
