/**
 * Terminal chords for the global keyboard-shortcuts guide.
 *
 * These are the bindings the dock's shortcuts popover listed, and they are
 * the ones the terminal actually handles: tab chords are Control on every
 * host (`terminalTabChord`), and find / text size are Command on macOS and
 * Control+Shift elsewhere (`terminalViewChord`) because plain Control belongs
 * to the shell. Nothing here is invented.
 */

import type { HostOS } from "../platform";

export interface GuideShortcut {
  keys: string[];
  description: string;
}

export function terminalGuideShortcuts(os: HostOS): GuideShortcut[] {
  const mac = os === "macos";
  const rows: GuideShortcut[] = [
    { keys: ["Ctrl", "Shift", "T"], description: "New shell in the directory of the shell beside it" },
    { keys: ["Ctrl", "Shift", "W"], description: "Close session — asks first when a job is running" },
    { keys: ["Ctrl", "Tab"], description: "Next terminal tab" },
    { keys: ["Ctrl", "Shift", "Tab"], description: "Previous terminal tab" },
    { keys: mac ? ["⌘", "F"] : ["Ctrl", "Shift", "F"], description: "Find in the terminal" },
    { keys: ["Enter"], description: "Next match" },
    { keys: ["Shift", "Enter"], description: "Previous match" },
    { keys: ["Esc"], description: "Close find" },
    { keys: mac ? ["⌘", "+"] : ["Ctrl", "Shift", "+"], description: "Larger terminal text" },
    { keys: mac ? ["⌘", "−"] : ["Ctrl", "Shift", "−"], description: "Smaller terminal text" },
    { keys: mac ? ["⌘", "0"] : ["Ctrl", "Shift", "0"], description: "Reset terminal text size" },
    { keys: ["←", "→"], description: "Previous / next tab while the tab strip is focused" },
    { keys: ["Home", "End"], description: "First / last tab while the tab strip is focused" },
  ];
  if (mac) {
    rows.push(
      { keys: ["⌘", "←"], description: "Start of the line" },
      { keys: ["⌘", "→"], description: "End of the line" },
      { keys: ["⌥", "←"], description: "Back a word" },
      { keys: ["⌥", "→"], description: "Forward a word" },
      { keys: ["⌘", "⌫"], description: "Delete to the start of the line" },
    );
  }
  return rows;
}
