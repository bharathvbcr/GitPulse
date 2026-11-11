export const MAX_CLIPBOARD_IMAGE_BYTES = 16 * 1024 * 1024;

export interface ClipboardItemLike {
  kind: string;
  type: string;
  getAsFile: () => File | null;
}

export interface ClipboardDataLike {
  getData: (format: string) => string;
  items?: ArrayLike<ClipboardItemLike> | null;
  files?: ArrayLike<File> | null;
}

/**
 * Extracts a pasted image `File` from clipboard data ONLY when `text/plain` is
 * empty, so rich-text copies from code editors or slide apps (which place both
 * `text/plain` and a rendered `image/png` preview on macOS `NSPasteboard`) keep
 * pasting text as expected, while screenshots (`⌘⌃⇧4`) and copied images
 * attach as scratch PNGs.
 */
export function extractClipboardImageFile(data: ClipboardDataLike | null | undefined): File | null {
  if (!data) return null;
  const plain = data.getData("text/plain");
  if (plain && plain.length > 0) return null;

  if (data.items) {
    for (let i = 0; i < data.items.length; i++) {
      const item = data.items[i];
      if (item && item.kind === "file" && item.type.toLowerCase().startsWith("image/")) {
        const file = item.getAsFile();
        if (file && file.size > 0) return file;
      }
    }
  }

  if (data.files) {
    for (let i = 0; i < data.files.length; i++) {
      const file = data.files[i];
      if (file && file.type.toLowerCase().startsWith("image/") && file.size > 0) {
        return file;
      }
    }
  }

  return null;
}

/** Encodes raw bytes to standard base64 without blowing the call stack on multi-MiB images. */
export function bytesToBase64(bytes: Uint8Array): string {
  const CHUNK = 0x8000;
  const parts: string[] = [];
  for (let offset = 0; offset < bytes.length; offset += CHUNK) {
    const slice = bytes.subarray(offset, Math.min(offset + CHUNK, bytes.length));
    parts.push(String.fromCharCode(...slice));
  }
  return btoa(parts.join(""));
}

/**
 * Normalizes a clipboard image `Blob` into PNG bytes encoded as base64.
 * PNG blobs pass straight through; JPEG/WebP/GIF blobs are transcoded via an
 * offscreen `<canvas>`.
 */
export async function clipboardImageToPngBase64(blob: Blob): Promise<string> {
  if (blob.size === 0) throw new Error("Clipboard image is empty.");
  if (blob.size > MAX_CLIPBOARD_IMAGE_BYTES) {
    throw new Error("Clipboard image exceeds 16 MiB.");
  }

  if (blob.type.toLowerCase() === "image/png") {
    const buf = await blob.arrayBuffer();
    return bytesToBase64(new Uint8Array(buf));
  }

  if (typeof createImageBitmap !== "function" || typeof document === "undefined") {
    throw new Error(`Unsupported clipboard image format (${blob.type || "unknown"}).`);
  }

  const bitmap = await createImageBitmap(blob);
  try {
    const canvas = document.createElement("canvas");
    canvas.width = bitmap.width;
    canvas.height = bitmap.height;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("Could not initialize 2D canvas to encode clipboard image.");
    ctx.drawImage(bitmap, 0, 0);
    const pngBlob = await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((out) => {
        if (out) resolve(out);
        else reject(new Error("Could not encode clipboard image as PNG."));
      }, "image/png");
    });
    if (pngBlob.size > MAX_CLIPBOARD_IMAGE_BYTES) {
      throw new Error("Encoded clipboard PNG exceeds 16 MiB.");
    }
    const buf = await pngBlob.arrayBuffer();
    return bytesToBase64(new Uint8Array(buf));
  } finally {
    bitmap.close();
  }
}
