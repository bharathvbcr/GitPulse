import { readFileSync } from "node:fs";
import path from "node:path";
import { deflateSync, inflateSync } from "node:zlib";
import { describe, expect, it } from "vitest";
import { sameAssetContent } from "./generate-brand-assets.mjs";

const root = path.resolve(import.meta.dirname, "..");
const read = (rel: string) => readFileSync(path.join(root, rel));

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

function crc32(buf: Buffer): number {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type: string, data: Buffer): Buffer {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])), 0);
  return Buffer.concat([len, typeBuf, data, crc]);
}

/**
 * The same PNG written by a different deflate: its pixels re-compressed at
 * another level, the way another zlib build writes the same image. `edit`
 * may change the raw (filtered) pixel stream first.
 */
function recompressPng(png: Buffer, edit?: (raw: Buffer) => void): Buffer {
  const out = [png.subarray(0, 8)];
  const idat: Buffer[] = [];
  let at = 8;
  while (at < png.length) {
    const len = png.readUInt32BE(at);
    const type = png.toString("ascii", at + 4, at + 8);
    const data = png.subarray(at + 8, at + 8 + len);
    if (type === "IDAT") {
      idat.push(data);
    } else {
      if (type === "IEND") {
        const raw = inflateSync(Buffer.concat(idat));
        edit?.(raw);
        out.push(chunk("IDAT", deflateSync(raw, { level: 1 })));
      }
      out.push(chunk(type, data));
    }
    at += 12 + len;
  }
  return Buffer.concat(out);
}

function recompressIco(ico: Buffer, edit?: (raw: Buffer) => void): Buffer {
  const count = ico.readUInt16LE(4);
  const dir = Buffer.from(ico.subarray(6, 6 + count * 16));
  const images: Buffer[] = [];
  let offset = 6 + count * 16;
  for (let i = 0; i < count; i++) {
    const base = i * 16;
    const size = ico.readUInt32LE(6 + base + 8);
    const start = ico.readUInt32LE(6 + base + 12);
    const png = recompressPng(ico.subarray(start, start + size), i === 0 ? edit : undefined);
    dir.writeUInt32LE(png.length, base + 8);
    dir.writeUInt32LE(offset, base + 12);
    offset += png.length;
    images.push(png);
  }
  return Buffer.concat([ico.subarray(0, 6), dir, ...images]);
}

function recompressIcns(icns: Buffer): Buffer {
  const parts: Buffer[] = [];
  let at = 8;
  while (at < icns.length) {
    const len = icns.readUInt32BE(at + 4);
    const png = recompressPng(icns.subarray(at + 8, at + len));
    const head = Buffer.alloc(8);
    icns.copy(head, 0, at, at + 4);
    head.writeUInt32BE(8 + png.length, 4);
    parts.push(head, png);
    at += len;
  }
  const header = Buffer.alloc(8);
  header.write("icns", 0, 4, "ascii");
  header.writeUInt32BE(8 + parts.reduce((sum, b) => sum + b.length, 0), 4);
  return Buffer.concat([header, ...parts]);
}

describe("brand asset check compares what an image holds, not how it was compressed", () => {
  it("accepts a PNG, ICO and ICNS whose pixels another deflate compressed", () => {
    for (const [rel, recompress] of [
      ["src-tauri/icons/icon.png", recompressPng],
      ["src-tauri/icons/icon.ico", recompressIco],
      ["src-tauri/icons/icon.icns", recompressIcns],
    ] as const) {
      const committed = read(rel);
      const other = recompress(committed);
      // The premise: a different compressor really did write different bytes.
      expect(other.equals(committed), rel).toBe(false);
      expect(sameAssetContent(rel, other, committed), rel).toBe(true);
    }
  });

  it("still refuses a changed pixel, in a PNG and inside an ICO", () => {
    const flip = (raw: Buffer) => {
      raw[raw.length - 1] ^= 0xff;
    };
    const png = read("src-tauri/icons/icon.png");
    expect(sameAssetContent("src-tauri/icons/icon.png", recompressPng(png, flip), png)).toBe(false);
    const ico = read("src-tauri/icons/icon.ico");
    expect(sameAssetContent("src-tauri/icons/icon.ico", recompressIco(ico, flip), ico)).toBe(false);
  });

  it("still refuses a changed ICO entry and compares other files byte for byte", () => {
    const ico = read("src-tauri/icons/icon.ico");
    const resized = Buffer.from(ico);
    resized[6] = resized[6] === 16 ? 17 : 16; // the first entry's width
    expect(sameAssetContent("src-tauri/icons/icon.ico", resized, ico)).toBe(false);
    const svg = read("public/waylode-mark.svg");
    expect(sameAssetContent("public/waylode-mark.svg", Buffer.concat([svg, Buffer.from(" ")]), svg)).toBe(false);
    expect(sameAssetContent("src-tauri/icons/icon.png", Buffer.from("not a png"), read("src-tauri/icons/icon.png"))).toBe(false);
  });
});
