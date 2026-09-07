/**
 * Thin client for MarkDev tree-sitter highlighting.
 *
 * The owner of backend selection is `diff/highlight.ts`; this module owns
 * the IPC invoke so `check:ipc` sees a production caller for
 * `cmd_syntax_highlight`.
 */

import { invoke } from "@tauri-apps/api/core";

/** UTF-16 span from `cmd_syntax_highlight`. */
export interface TreeSitterSpan {
  start: number;
  end: number;
  kind: string;
}

export function syntaxHighlight(language: string, code: string): Promise<TreeSitterSpan[]> {
  return invoke<TreeSitterSpan[]>("cmd_syntax_highlight", { language, code });
}
