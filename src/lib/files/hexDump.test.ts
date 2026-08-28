import { describe, expect, it } from "vitest";
import { bytesFromBase64Prefix, hexDumpRows } from "./hexDump";

describe("hexDumpRows", () => {
  it("emits 16-byte rows with offset, hex, and printable ASCII", () => {
    const bytes = Uint8Array.from([0x00, 0x41, 0x7f, 0x20, 0xff]);
    const rows = hexDumpRows(bytes);
    expect(rows).toHaveLength(1);
    expect(rows[0].offset).toBe("00000000");
    expect(rows[0].hex).toEqual(["00", "41", "7F", "20", "FF"]);
    expect(rows[0].ascii).toBe(".A. .");
  });

  it("caps the dump and returns nothing for empty input", () => {
    expect(hexDumpRows(new Uint8Array())).toEqual([]);
    const many = new Uint8Array(40);
    many.fill(0x61);
    const rows = hexDumpRows(many, 16);
    expect(rows).toHaveLength(1);
    expect(rows[0].hex).toHaveLength(16);
  });

  it("decodes a base64 prefix into bytes and fails closed on garbage", () => {
    const bytes = bytesFromBase64Prefix(btoa("Hello"), 16);
    expect([...bytes]).toEqual([72, 101, 108, 108, 111]);
    expect(bytesFromBase64Prefix("")).toEqual(new Uint8Array());
    expect(bytesFromBase64Prefix("!!!!")).toEqual(new Uint8Array());
  });
});
