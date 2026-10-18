import type { FileBlob } from "./types";

/**
 * Why Overwrite will replace the disk file with the draft exactly as typed
 * rather than restoring the disk file's line endings, or `null` when it will
 * not. An ordinary save refuses a file whose bytes the editor cannot show
 * faithfully (invalid UTF-8, mixed line endings); Overwrite discards the disk
 * content by intent, so it writes the buffer byte for byte instead.
 */
export function overwriteNote(
  blob: Pick<FileBlob, "invalid_utf8_bytes" | "eol"> | null,
): string | null {
  if (!blob) return null;
  if ((blob.invalid_utf8_bytes ?? 0) > 0) {
    return "The file on disk is not valid UTF-8. Overwrite replaces it with your draft exactly as typed.";
  }
  if (blob.eol === "mixed") {
    return "The file on disk mixes line endings. Overwrite replaces it with your draft exactly as typed, with LF endings.";
  }
  return null;
}
