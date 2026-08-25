/**
 * Copy text to the OS clipboard through the webview, with no plugin.
 *
 * `navigator.clipboard` needs a focused document and a secure context — both
 * true for a foreground Tauri window — but WebView permission quirks make it
 * fail occasionally, so a hidden-textarea `execCommand` fallback stands behind
 * it. Dependencies (an injected clipboard/document) keep this unit-testable in
 * a plain Node environment.
 */
export type WebClipboard = { writeText(text: string): Promise<void> };

/** The slice of the DOM the execCommand fallback needs. */
export interface ClipboardDocument {
  body: {
    appendChild(node: unknown): unknown;
    removeChild(node: unknown): unknown;
  };
  createElement(tag: string): {
    value: string;
    setAttribute(name: string, value: string): void;
    style: Record<string, string>;
    select(): void;
  };
  execCommand?(commandId: string): boolean;
}

export async function copyText(
  text: string,
  deps: {
    clipboard?: WebClipboard | null;
    document?: ClipboardDocument | null;
  } = {},
): Promise<boolean> {
  if (!text) return false;
  const clipboard =
    deps.clipboard ?? (typeof navigator !== "undefined" ? navigator.clipboard : null);
  if (clipboard) {
    try {
      await clipboard.writeText(text);
      return true;
    } catch {
      /* fall through to the legacy path */
    }
  }

  const doc =
    deps.document ??
    (typeof document !== "undefined" ? (document as unknown as ClipboardDocument) : null);
  if (!doc?.body || typeof doc.createElement !== "function") return false;
  try {
    const textarea = doc.createElement("textarea");
    textarea.value = text;
    textarea.setAttribute("readonly", "");
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    doc.body.appendChild(textarea);
    textarea.select();
    const ok = doc.execCommand ? doc.execCommand("copy") : false;
    doc.body.removeChild(textarea);
    return ok;
  } catch {
    return false;
  }
}
