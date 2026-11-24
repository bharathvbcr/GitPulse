/**
 * Copy text to the OS clipboard through the webview, with no plugin.
 *
 * `navigator.clipboard` needs a focused document and a secure context — both
 * true for a foreground Tauri window — but WebView permission quirks make it
 * fail occasionally, so a hidden-textarea `execCommand` fallback stands behind
 * it. Dependencies (an injected clipboard/document) keep this unit-testable in
 * a plain Node environment.
 */
export type WebClipboard = {
  writeText(text: string): Promise<void>;
  write?(items: unknown[]): Promise<void>;
};

/**
 * Copies a raster image blob (or a gesture-scoped promise resolving to one, so
 * Safari/WKWebView retains the user activation) to the OS clipboard when the
 * host supports `navigator.clipboard.write` + `ClipboardItem`.
 */
export async function copyImageBlob(
  blobOrPromise: Blob | Promise<Blob>,
  mime = "image/png",
  deps: {
    clipboard?: { write?(items: unknown[]): Promise<void> } | null;
    ClipboardItemCtor?: (new (items: Record<string, Blob | PromiseLike<Blob>>) => unknown) | null;
  } = {},
): Promise<boolean> {
  const clipboard =
    deps.clipboard !== undefined
      ? deps.clipboard
      : typeof navigator !== "undefined"
        ? (navigator.clipboard as { write?(items: unknown[]): Promise<void> } | null)
        : null;
  const ItemCtor =
    deps.ClipboardItemCtor !== undefined
      ? deps.ClipboardItemCtor
      : typeof ClipboardItem !== "undefined"
        ? ClipboardItem
        : null;
  if (!clipboard?.write || !ItemCtor) {
    if (blobOrPromise instanceof Promise) void blobOrPromise.catch(() => {});
    return false;
  }
  const blobPromise = blobOrPromise instanceof Promise ? blobOrPromise : null;
  if (blobPromise) void blobPromise.catch(() => {});
  try {
    const item = new ItemCtor({ [mime]: blobOrPromise });
    await clipboard.write([item]);
    return true;
  } catch {
    if (!blobPromise) return false;
    try {
      const resolved = await blobPromise;
      const item = new ItemCtor({ [mime]: resolved });
      await clipboard.write([item]);
      return true;
    } catch {
      return false;
    }
  }
}

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

export type ClipboardRead =
  | { ok: true; text: string }
  | { ok: false; reason: "unavailable" | "failed" };

/**
 * Reads text from the OS clipboard through the webview. There is no fallback
 * for reading: `unavailable` means this webview has no read API, `failed`
 * that it refused (permission or focus).
 */
export async function readClipboardText(
  deps: { clipboard?: { readText?(): Promise<string> } | null } = {},
): Promise<ClipboardRead> {
  const clipboard =
    deps.clipboard !== undefined ? deps.clipboard : typeof navigator !== "undefined" ? navigator.clipboard : null;
  if (!clipboard?.readText) return { ok: false, reason: "unavailable" };
  try {
    return { ok: true, text: await clipboard.readText() };
  } catch {
    return { ok: false, reason: "failed" };
  }
}

/**
 * A "Copied" indicator that lights for `ms` after a successful copy and goes
 * out on its own. A second copy restarts the countdown, and `dispose` cancels
 * it so a closed panel never writes to unmounted state.
 */
export function createCopyFlash(setCopied: (copied: boolean) => void, ms = 1500) {
  let timer: ReturnType<typeof setTimeout> | null = null;
  const clear = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  const flash = () => {
    setCopied(true);
    clear();
    timer = setTimeout(() => {
      timer = null;
      setCopied(false);
    }, ms);
  };
  return {
    /** Copies, then lights the indicator; returns false (and leaves it alone) on failure. */
    async copy(text: string, deps?: Parameters<typeof copyText>[1]): Promise<boolean> {
      if (!(await copyText(text, deps))) return false;
      flash();
      return true;
    },
    /** Lights the indicator for a copy the caller already made and has checked is still wanted. */
    flash,
    /** Puts the indicator out at once: the thing it described is gone. */
    reset(): void {
      clear();
      setCopied(false);
    },
    dispose: clear,
  };
}
