/**
 * Canonical way to hand a repository file to the OS — "open with the default
 * app" and "reveal in the file manager".
 *
 * Deliberately NOT the `@tauri-apps/plugin-opener` commands. Those take an
 * absolute path straight from the webview, which makes the containment check
 * advisory: whichever call site forgets it, wins. That is not hypothetical —
 * three of the four former call sites joined through `joinWorktreePath` and
 * MarkDevViewer interpolated `${repo}/${filePath}` raw, so a repo-relative
 * path containing `..` escaped the worktree there and nowhere else.
 *
 * These pass the repository root and the repo-relative path as two arguments
 * and let Rust rebuild the absolute path (`desktop::shell`), so the frontend
 * cannot name an absolute path at all and containment is proven on the
 * trusted side against the canonicalized root — which also catches a symlink
 * pointing out of the repo, something a string prefix check cannot see.
 *
 * Both reject rather than resolve on failure, so callers surface the reason
 * instead of a silently dead menu item.
 */
import { invoke } from "../ipc/invoke";
import type { EditorChoice } from "./editorChoice";

/** Opens a repo-relative path with the OS default application. */
export async function openInDefaultApp(repo: string, relative: string): Promise<void> {
  await invoke("cmd_open_worktree_path", { repo, relative });
}

/** Reveals a repo-relative path in the OS file manager. */
export async function revealInFileManager(repo: string, relative: string): Promise<void> {
  await invoke("cmd_reveal_worktree_path", { repo, relative });
}

/** Where in the file to land: 1-based, and a column needs a line. */
export interface EditorPosition {
  line?: number;
  column?: number;
}

/**
 * Hands a repo-relative file to an external editor, at a line when one is
 * known. `editor` is the closed `EditorChoice` set, never a program name: the
 * program and its flags live in Rust (`desktop::editor`), which also applies
 * the containment and trust checks. Rejects with `editor_not_found` when the
 * chosen editor is not installed; it never substitutes another.
 */
export async function openInEditor(
  repo: string,
  relative: string,
  options: EditorPosition & { editor?: EditorChoice } = {},
): Promise<void> {
  const { line, column, editor } = options;
  await invoke("cmd_open_in_editor", { repo, relative, line, column, editor });
}

/** Reveals the canonical Git root; file paths retain their stricter containment API. */
export async function revealRepository(repo: string): Promise<void> {
  await invoke("cmd_reveal_repository", { repo });
}
