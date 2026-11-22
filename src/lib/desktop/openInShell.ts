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
import { get } from "svelte/store";
import { invoke } from "../ipc/invoke";
import { interfaceStore } from "../stores/interfaceStore";

/** Opens a repo-relative path with the OS default application. */
export async function openInDefaultApp(repo: string, relative: string): Promise<void> {
  await invoke("cmd_open_worktree_path", { repo, relative });
}

/** Reveals a repo-relative path in the OS file manager. */
export async function revealInFileManager(repo: string, relative: string): Promise<void> {
  await invoke("cmd_reveal_worktree_path", { repo, relative });
}

/** Reveals the canonical Git root; file paths retain their stricter containment API. */
export async function revealRepository(repo: string): Promise<void> {
  await invoke("cmd_reveal_repository", { repo });
}

/**
 * Opens a repo-relative file in the user's own editor (VS Code, Cursor or
 * Zed), at a line and column when the caller has them, so the extensions the
 * user runs there keep working. The editor is the "Open in editor" setting: one
 * of four names, never a program, which `desktop::shell` maps to a fixed
 * launcher. Rejects with the backend's reason, including the typed
 * `editor_not_found` error when no editor is installed.
 */
export async function openInEditor(
  repo: string,
  relative: string,
  position: { line?: number; column?: number } = {},
): Promise<void> {
  await invoke("cmd_open_in_editor", {
    repo,
    relative,
    line: position.line ?? null,
    column: position.column ?? null,
    editor: get(interfaceStore).externalEditor,
  });
}
