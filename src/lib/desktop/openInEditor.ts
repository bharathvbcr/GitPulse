/**
 * "Open in editor" as the surfaces use it: the file goes to the editor the user
 * chose in Settings, and a failure is told to them rather than swallowed.
 *
 * The wrapper that reaches Rust is `openInEditor` in ./openInShell, next to the
 * other repository-path hand-offs; this reads the preference and speaks for it.
 */
import { get } from "svelte/store";
import { ipcErrorKind } from "../ipc/invoke";
import { interfaceStore } from "../stores/interfaceStore";
import { toastStore } from "../stores/toastStore";
import { describeError } from "../ui/copy/describeFailure";
import { openInEditor, type EditorPosition } from "./openInShell";

/** Opens `relative` in the editor chosen in Settings, at `position` when given. */
export async function openInPreferredEditor(
  repo: string,
  relative: string,
  position: EditorPosition = {},
): Promise<void> {
  await openInEditor(repo, relative, { ...position, editor: get(interfaceStore).preferredEditor });
}

/**
 * Tells the user why a file did not open. A missing editor is the one failure
 * with something to do about it, so it carries a way to the setting; every
 * other failure is the launcher's own message.
 */
export function reportEditorFailure(error: unknown): void {
  const message = describeError(error, "repo");
  if (ipcErrorKind(error) === "editor_not_found") {
    toastStore.error(message, {
      label: "Choose editor",
      onClick: () => {
        window.dispatchEvent(new CustomEvent("gitpulse:settings"));
      },
    });
    return;
  }
  toastStore.error(message);
}
