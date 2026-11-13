import { invoke } from "../ipc/invoke";
import { isTauri } from "../platform";

/**
 * Saving text the app made (a theme file, a Pulse card, retained terminal
 * output) to a file.
 *
 * In the app this goes through `cmd_save_export`, which opens the native save
 * panel and writes where the person chose: WKWebView ignores `<a download>`,
 * so a browser-style download did nothing in the packaged app. Only where no
 * native shell is present (a browser harness) does it fall back to an anchor
 * download, and that outcome says a download was *requested*, because the
 * page cannot know whether a file landed.
 *
 * A failed native save rejects with the backend's sentence; it never falls
 * back to the anchor, which would turn a reported failure into silence.
 */

/**
 * What is being exported. Mirrors `ExportKind` in
 * src-tauri/src/commands/export.rs (pinned by
 * scripts/enum-variant-contract.test.ts); the kind fixes the save panel's
 * title, file type and extension on the Rust side.
 */
export type ExportKind = "terminal_output" | "theme" | "pulse_card";

const MEDIA_TYPES: Record<ExportKind, string> = {
  terminal_output: "text/plain",
  theme: "application/json",
  pulse_card: "image/svg+xml",
};

export interface SaveRequest {
  readonly kind: ExportKind;
  /** A suggestion for the panel; the person can change it, and Rust reduces it to a bare name. */
  readonly fileName: string;
  readonly data: string;
}

export type SaveOutcome =
  /** Written by the app; `fileName` is the name the person saved under. */
  | { readonly status: "saved"; readonly fileName: string }
  /** The person closed the panel. Nothing was written and nothing went wrong. */
  | { readonly status: "cancelled" }
  /** No native shell: a browser download was asked for, with no way to know it landed. */
  | { readonly status: "requested"; readonly fileName: string };

export interface SaveDeps {
  /** Whether a native shell is present. Defaults to `isTauri()`. */
  readonly native?: boolean;
  /** The native save: the saved file's name, or null when the panel was cancelled. */
  readonly saveNative?: (request: SaveRequest) => Promise<unknown>;
  /** The browser fallback. */
  readonly download?: (fileName: string, blob: Blob) => void;
}

function saveThroughApp(request: SaveRequest): Promise<unknown> {
  return invoke<string | null>("cmd_save_export", {
    kind: request.kind,
    fileName: request.fileName,
    data: request.data,
  });
}

/** An anchor download, for a page with no native shell. */
export function anchorDownload(fileName: string, blob: Blob, doc: Document = document): void {
  const url = URL.createObjectURL(blob);
  const link = doc.createElement("a");
  link.href = url;
  link.download = fileName;
  doc.body.append(link);
  try {
    link.click();
  } finally {
    link.remove();
    // Revoking at once can cancel the download before the browser reads the blob.
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
}

export async function saveExport(request: SaveRequest, deps: SaveDeps = {}): Promise<SaveOutcome> {
  const native = deps.native ?? isTauri();
  if (!native) {
    (deps.download ?? anchorDownload)(
      request.fileName,
      new Blob([request.data], { type: MEDIA_TYPES[request.kind] }),
    );
    return { status: "requested", fileName: request.fileName };
  }
  const saved = await (deps.saveNative ?? saveThroughApp)(request);
  if (saved === null) return { status: "cancelled" };
  if (typeof saved === "string" && saved.length > 0) return { status: "saved", fileName: saved };
  throw new Error("The save finished without saying which file it wrote.");
}
