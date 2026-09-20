import { describe, expect, it } from "vitest";
import { fenceLogs, formatLogsSection, LOGS_CAP_BYTES, sanitizeLogs } from "./taskLogs";

const POLLUTION = ["__proto__", "constructor", "prototype", "toString", "valueOf", "hasOwnProperty", "isPrototypeOf"];
const INVISIBLE = ["\u202e", "\u200b", "\u00a0", "\u0000", "\u001b", "\u007f"];

describe("raw logs under hostile paste", () => {
  it("never returns a non-string, a prototype member, or a payload over the byte cap", () => {
    const hostile: unknown[] = [
      "",
      " ",
      "\t\n",
      ...INVISIBLE,
      null,
      undefined,
      0,
      12,
      true,
      ["error: E42"],
      { text: "error: E42" },
      Object.create(null),
      ...POLLUTION,
      "\u001b[31m" + "x".repeat(10_000) + "\u001b[0m",
      "\u001b]8;;http://x\u001b\\" + "click".repeat(200) + "\u001b]8;;\u001b\\",
      "\r".repeat(500) + "progress 100%",
      "`".repeat(80) + "\ncode\n" + "`".repeat(80),
      "😀".repeat(20_000),
      "界".repeat(20_000),
      "x".repeat(LOGS_CAP_BYTES + 50),
      "\u0000".repeat(1000) + "E42",
    ];
    for (const input of hostile) {
      const result = sanitizeLogs(input);
      expect(typeof result.text, JSON.stringify(input)).toBe("string");
      expect(typeof result.notice, JSON.stringify(input)).toBe("string");
      expect(typeof result.truncated, JSON.stringify(input)).toBe("boolean");
      expect(Number.isSafeInteger(result.originalBytes), JSON.stringify(input)).toBe(true);
      expect(Number.isSafeInteger(result.keptBytes), JSON.stringify(input)).toBe(true);
      expect(result.text.includes("\0"), JSON.stringify(input)).toBe(false);
      expect(result.text.includes("\u001b"), JSON.stringify(input)).toBe(false);
      expect(new TextEncoder().encode(result.text).length, JSON.stringify(input)).toBeLessThanOrEqual(LOGS_CAP_BYTES);
      expect(result.text.isWellFormed(), JSON.stringify(input)).toBe(true);
    }
  });

  it("keeps identifiers through 200 stacked pastes and still names a cut", () => {
    let text = "";
    for (let n = 1; n <= 200; n++) {
      text = sanitizeLogs(`${text}\nerror: E${n} at frame ${n}`).text;
    }
    expect(text).toContain("E1");
    expect(text).toContain("E200");
    const cut = sanitizeLogs(`${"y".repeat(LOGS_CAP_BYTES)}E99999`);
    expect(cut.truncated).toBe(true);
    expect(cut.text).toContain("[logs truncated:");
    expect(cut.text).not.toContain("E99999");
  });

  it("fences adversarial backtick runs so a brief cannot be closed early", () => {
    for (let n = 1; n <= 20; n++) {
      const payload = `${"`".repeat(n)}\nE42\n${"`".repeat(n)}`;
      const fenced = fenceLogs(payload);
      const mark = fenced.slice(0, fenced.indexOf("\n"));
      expect(mark.length, String(n)).toBeGreaterThan(n);
      expect(fenced.startsWith(`${mark}\n`)).toBe(true);
      expect(fenced.endsWith(`\n${mark}`)).toBe(true);
      const section = formatLogsSection(payload);
      expect(section, String(n)).toContain("E42");
      expect(section, String(n)).toContain("## Raw logs");
    }
  });
});
