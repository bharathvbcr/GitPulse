import { deflateRawSync } from "node:zlib";
import { describe, expect, it } from "vitest";
import { deriveTokens, SHIPPED_CONTRAST } from "../color/derive";
import { parseHex, rgbToOklch } from "../color/oklch";
import { importThemeText } from "./themeFile";
import {
  MAX_VSCODE_COLOR_KEYS,
  MAX_VSIX_ENTRY_BYTES,
  MIN_STATUS_ACCENT_SEPARATION,
  clearHueFromAccent,
  importVsCodeThemeText,
  importVsixBytes,
  parseHexWithAlpha,
  signedHueDistance,
  stripJsonc,
} from "./vscodeTheme";

function buildZipArchive(
  files: readonly { name: string; content: Uint8Array | string; deflate?: boolean }[],
): Uint8Array {
  const encoder = new TextEncoder();
  const chunks: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;

  for (const file of files) {
    const nameBytes = encoder.encode(file.name);
    const raw = typeof file.content === "string" ? encoder.encode(file.content) : file.content;
    const method = file.deflate ? 8 : 0;
    const payload = file.deflate ? new Uint8Array(deflateRawSync(raw)) : raw;

    const lfh = new Uint8Array(30 + nameBytes.length);
    const lfhView = new DataView(lfh.buffer);
    lfhView.setUint32(0, 0x04034b50, true);
    lfhView.setUint16(4, 20, true);
    lfhView.setUint16(8, method, true);
    lfhView.setUint32(18, payload.length, true);
    lfhView.setUint32(22, raw.length, true);
    lfhView.setUint16(26, nameBytes.length, true);
    lfh.set(nameBytes, 30);

    chunks.push(lfh, payload);

    const cd = new Uint8Array(46 + nameBytes.length);
    const cdView = new DataView(cd.buffer);
    cdView.setUint32(0, 0x02014b50, true);
    cdView.setUint16(4, 20, true);
    cdView.setUint16(6, 20, true);
    cdView.setUint16(10, method, true);
    cdView.setUint32(20, payload.length, true);
    cdView.setUint32(24, raw.length, true);
    cdView.setUint16(28, nameBytes.length, true);
    cdView.setUint32(42, offset, true);
    cd.set(nameBytes, 46);
    central.push(cd);

    offset += lfh.length + payload.length;
  }

  const cdStart = offset;
  let cdSize = 0;
  for (const cd of central) {
    chunks.push(cd);
    cdSize += cd.length;
  }

  const eocd = new Uint8Array(22);
  const eocdView = new DataView(eocd.buffer);
  eocdView.setUint32(0, 0x06054b50, true);
  eocdView.setUint16(8, files.length, true);
  eocdView.setUint16(10, files.length, true);
  eocdView.setUint32(12, cdSize, true);
  eocdView.setUint32(16, cdStart, true);
  chunks.push(eocd);

  const total = chunks.reduce((sum, c) => sum + c.length, 0);
  const out = new Uint8Array(total);
  let cursor = 0;
  for (const c of chunks) {
    out.set(c, cursor);
    cursor += c.length;
  }
  return out;
}

describe("stripJsonc", () => {
  it("strips line comments, block comments, and trailing commas while preserving URLs inside strings", () => {
    const raw = `{
      // Top comment
      "name": "http://localhost:5173/path /* not a comment */",
      /* Multi-line
         comment */
      "colors": {
        "editor.background": "#10141f", // inline comment
        "focusBorder": "#569cd6",
      },
    }`;
    const parsed = JSON.parse(stripJsonc(raw)) as { name: string; colors: Record<string, string> };
    expect(parsed.name).toBe("http://localhost:5173/path /* not a comment */");
    expect(parsed.colors["editor.background"]).toBe("#10141f");
    expect(parsed.colors.focusBorder).toBe("#569cd6");
  });
});

describe("parseHexWithAlpha", () => {
  it("composites #rrggbbaa and #rgba colors onto the backdrop", () => {
    expect(parseHexWithAlpha("#ff000080", [0, 0, 0])).toEqual([128, 0, 0]);
    expect(parseHexWithAlpha("#0f08", [0, 0, 0])).toEqual([0, 136, 0]);
    expect(parseHexWithAlpha("url(https://evil)", [0, 0, 0])).toBeNull();
  });
});

describe("clearHueFromAccent", () => {
  it("shifts status hues >= 30 deg away from vibrant accents while leaving neutral accents alone", () => {
    const blueAccent = rgbToOklch([59, 130, 246]);
    const clearedInfo = clearHueFromAccent(blueAccent.h + 5, blueAccent);
    expect(Math.abs(signedHueDistance(blueAccent.h, clearedInfo))).toBeGreaterThanOrEqual(
      MIN_STATUS_ACCENT_SEPARATION - 0.01,
    );

    const grayAccent = rgbToOklch([160, 160, 160]);
    expect(clearHueFromAccent(240, grayAccent)).toBe(240);
  });
});

describe("importVsCodeThemeText", () => {
  it("converts a JSONC VS Code theme into a contrast-safe gitpulse-theme/1 palette", () => {
    const jsonc = `{
      // VS Code dark theme
      "name": "Midnight Sky (url(x) clean?)",
      "type": "dark",
      "colors": {
        "editor.background": "#121622",
        "editor.foreground": "#e4e8f5",
        "focusBorder": "#e5484d", // vibrant red colliding with danger hue
        "editorError.foreground": "#e5484d",
        "gitDecoration.addedResourceForeground": "#3dd68c",
        "editorWarning.foreground": "#f5a623",
        "editorInfo.foreground": "#569cd680",
      },
    }`;
    const result = importThemeText(jsonc);
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.theme.schema).toBe("gitpulse-theme/1");
    expect(result.theme.appearance).toBe("dark");
    expect(result.theme.colors.base).toBe("#121622");

    // Danger hue must be cleared >= 30 deg away from the red accent.
    const accentRgb = parseHex(result.theme.colors.accent)!;
    const dangerRgb = parseHex(result.theme.colors.danger)!;
    const sep = Math.abs(signedHueDistance(rgbToOklch(accentRgb).h, rgbToOklch(dangerRgb).h));
    expect(sep).toBeGreaterThanOrEqual(MIN_STATUS_ACCENT_SEPARATION - 1.5);

    // Must derive cleanly through GitPulse's runtime contrast guard.
    const tokens = deriveTokens(
      { ...result.theme.colors, contrast: result.theme.colors.contrast ?? SHIPPED_CONTRAST },
      result.theme.appearance,
    );
    expect(tokens.ok).toBe(true);
  });

  it("refuses themes exceeding MAX_VSCODE_COLOR_KEYS", () => {
    const colors: Record<string, string> = {};
    for (let i = 0; i <= MAX_VSCODE_COLOR_KEYS; i += 1) {
      colors[`k${i}`] = "#112233";
    }
    const result = importVsCodeThemeText(JSON.stringify({ name: "Huge", colors }));
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.reason).toContain("limit");
  });

  it("sanitizes dangerous name fragments without rejecting safe themes", () => {
    const result = importVsCodeThemeText(
      JSON.stringify({
        name: "Bad; {css:1} <script>",
        type: "light",
        colors: { "editor.background": "#fafafa", focusBorder: "#2563eb" },
      }),
    );
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.theme.appearance).toBe("light");
    expect(result.theme.name).not.toMatch(/[;<>{}]/);
  });
});

describe("importVsixBytes", () => {
  it("extracts deflated VS Code themes from a valid .vsix archive", async () => {
    const vsix = buildZipArchive([
      {
        name: "extension/package.json",
        deflate: true,
        content: JSON.stringify({
          displayName: "Tokyo Pack",
          contributes: {
            themes: [{ label: "Tokyo Night Clean", uiTheme: "vs-dark", path: "./themes/tokyo.json" }],
          },
        }),
      },
      {
        name: "extension/themes/tokyo.json",
        deflate: true,
        content: `{
          "colors": {
            "editor.background": "#1a1b26",
            "editor.foreground": "#c0caf5",
            "focusBorder": "#7aa2f7",
          }
        }`,
      },
    ]);
    const result = await importVsixBytes(vsix, "tokyo-pack.vsix");
    expect(result.ok).toBe(true);
    if (!result.ok) return;
    expect(result.theme.name).toBe("Tokyo Night Clean");
    expect(result.theme.appearance).toBe("dark");
    expect(result.theme.colors.base).toBe("#1a1b26");
  });

  it("refuses path-traversal entries inside .vsix archives", async () => {
    const vsix = buildZipArchive([
      { name: "extension/../../etc/passwd", content: "root:x:0:0" },
    ]);
    const result = await importVsixBytes(vsix);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.reason).toContain("unsafe archive path");
  });

  it("refuses zip-bomb entries exceeding MAX_VSIX_ENTRY_BYTES", async () => {
    const big = "A".repeat(MAX_VSIX_ENTRY_BYTES + 2048);
    const vsix = buildZipArchive([
      {
        name: "extension/package.json",
        deflate: true,
        content: big,
      },
    ]);
    const result = await importVsixBytes(vsix);
    expect(result.ok).toBe(false);
    if (result.ok) return;
    expect(result.reason).toContain("limit");
  });
});
