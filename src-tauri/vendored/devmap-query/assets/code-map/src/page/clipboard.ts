/**
 * Copy text to the clipboard from a read-only page.
 *
 * The standalone page cannot open an editor, so "open file" is "copy path".
 * `navigator.clipboard` needs a secure context and a permission some hosts
 * (a `file://` page, an embedded pane) do not grant, so a hidden textarea and
 * `execCommand("copy")` is the fallback. Resolves false when neither worked.
 */

import { h } from "./dom";

export async function copyToClipboard(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the selection-based copy.
  }
  const scratch = h("textarea", { attrs: { "aria-hidden": "true", tabindex: "-1" } });
  scratch.value = text;
  scratch.style.position = "fixed";
  scratch.style.opacity = "0";
  document.body.appendChild(scratch);
  scratch.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch {
    copied = false;
  }
  scratch.remove();
  return copied;
}
