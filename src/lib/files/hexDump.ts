export interface HexDumpRow {
  offset: string;
  hex: string[];
  ascii: string;
}

const DEFAULT_CAP = 16_384;

/** First `cap` bytes as classic hex+ASCII rows. Empty input yields no rows. */
export function hexDumpRows(bytes: Uint8Array, cap: number = DEFAULT_CAP): HexDumpRow[] {
  const limit = Math.max(0, Math.min(bytes.length, cap));
  const rows: HexDumpRow[] = [];
  for (let i = 0; i < limit; i += 16) {
    const end = Math.min(i + 16, limit);
    const hex: string[] = [];
    let ascii = "";
    for (let j = i; j < end; j += 1) {
      const byte = bytes[j];
      hex.push(byte.toString(16).padStart(2, "0").toUpperCase());
      ascii += byte >= 32 && byte <= 126 ? String.fromCharCode(byte) : ".";
    }
    rows.push({
      offset: i.toString(16).padStart(8, "0").toUpperCase(),
      hex,
      ascii,
    });
  }
  return rows;
}

export function bytesFromBase64Prefix(base64: string, cap: number = DEFAULT_CAP): Uint8Array {
  if (typeof atob !== "function" || !base64) return new Uint8Array();
  try {
    const binary = atob(base64.slice(0, Math.ceil((cap * 4) / 3) + 4));
    const take = Math.min(binary.length, cap);
    const bytes = new Uint8Array(take);
    for (let i = 0; i < take; i += 1) bytes[i] = binary.charCodeAt(i);
    return bytes;
  } catch {
    return new Uint8Array();
  }
}
