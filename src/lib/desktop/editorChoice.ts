/**
 * Which external editor "Open in editor" hands a file to.
 *
 * A closed set, mirroring `EditorChoice` in src-tauri/src/desktop/editor.rs
 * by hand (no contract test ties the two; change both). The webview only ever
 * names one of these five; the programs behind them and their flags are
 * constants in Rust, so a stored preference can never become a command line.
 * "antigravity" is the Antigravity IDE, not the separate Antigravity agent app.
 */
export type EditorChoice = "auto" | "vscode" | "antigravity" | "cursor" | "zed";

/** The first installed of VS Code, Antigravity IDE, Cursor and Zed, in that order. */
export const DEFAULT_EDITOR_CHOICE: EditorChoice = "auto";

/** The choices in the order the setting lists them. */
export const EDITOR_CHOICES: readonly { value: EditorChoice; label: string }[] = [
  { value: "auto", label: "Auto" },
  { value: "vscode", label: "VS Code" },
  { value: "antigravity", label: "Antigravity IDE" },
  { value: "cursor", label: "Cursor" },
  { value: "zed", label: "Zed" },
];

export function isEditorChoice(value: unknown): value is EditorChoice {
  return EDITOR_CHOICES.some((choice) => choice.value === value);
}
