/**
 * Which external editor "Open in editor" hands a file to.
 *
 * A closed set, mirroring `EditorChoice` in src-tauri/src/desktop/editor.rs
 * (pinned by scripts/enum-variant-contract.test.ts). The webview only ever
 * names one of these four; the programs behind them and their flags are
 * constants in Rust, so a stored preference can never become a command line.
 */
export type EditorChoice = "auto" | "vscode" | "cursor" | "zed";

/** The first installed of VS Code, Cursor and Zed. */
export const DEFAULT_EDITOR_CHOICE: EditorChoice = "auto";

/** The choices in the order the setting lists them. */
export const EDITOR_CHOICES: readonly { value: EditorChoice; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "vscode", label: "VS Code" },
  { value: "cursor", label: "Cursor" },
  { value: "zed", label: "Zed" },
];

export function isEditorChoice(value: unknown): value is EditorChoice {
  return EDITOR_CHOICES.some((choice) => choice.value === value);
}
