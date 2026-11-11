import { describe, expect, it } from "vitest";
import {
  bytesToBase64,
  clipboardImageToPngBase64,
  clipboardMayHoldFiles,
  extractClipboardImageFile,
  type ClipboardDataLike,
} from "./clipboardImage";

function fakeImageFile(name: string, type: string, bytes: Uint8Array<ArrayBuffer>): File {
  return new File([bytes], name, { type });
}

describe("clipboardImage", () => {
  it("extracts image files only when text/plain is empty", () => {
    const png = fakeImageFile("shot.png", "image/png", new Uint8Array([137, 80, 78, 71]));
    const screenshotBoard: ClipboardDataLike = {
      getData: () => "",
      items: [{ kind: "file", type: "image/png", getAsFile: () => png }],
    };
    expect(extractClipboardImageFile(screenshotBoard)).toBe(png);

    const richTextBoard: ClipboardDataLike = {
      getData: (fmt) => (fmt === "text/plain" ? "const x = 1;" : ""),
      items: [{ kind: "file", type: "image/png", getAsFile: () => png }],
    };
    expect(extractClipboardImageFile(richTextBoard)).toBeNull();
  });

  it("encodes PNG blobs directly to base64 without loss", async () => {
    const raw = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    const png = fakeImageFile("shot.png", "image/png", raw);
    const encoded = await clipboardImageToPngBase64(png);
    expect(encoded).toBe(bytesToBase64(raw));
  });

  it("detects when a paste event may represent copied Finder files vs plain text", () => {
    const plainTextBoard: ClipboardDataLike = {
      getData: (fmt) => (fmt === "text/plain" ? "git status" : ""),
      items: [{ kind: "string", type: "text/plain", getAsFile: () => null }],
      files: [],
      types: ["text/plain"],
    };
    expect(clipboardMayHoldFiles(plainTextBoard, "git status")).toBe(false);

    const finderFileBoard: ClipboardDataLike = {
      getData: (fmt) => (fmt === "text/plain" ? "main.rs" : ""),
      items: [{ kind: "file", type: "text/plain", getAsFile: () => null }],
      files: [fakeImageFile("main.rs", "text/plain", new Uint8Array([1]))],
      types: ["text/plain", "Files"],
    };
    expect(clipboardMayHoldFiles(finderFileBoard, "main.rs")).toBe(true);

    const fileUrlOnlyBoard: ClipboardDataLike = {
      getData: () => "",
      items: [],
      files: [],
      types: [],
    };
    expect(clipboardMayHoldFiles(fileUrlOnlyBoard, "")).toBe(true);
  });
});
