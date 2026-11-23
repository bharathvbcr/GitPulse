import { describe, expect, it } from "vitest";
import { AGENT_MESSAGE_MAX_BYTES } from "../workbench/sendToAgent";
import { composeSelectionContent, formatSelectionRef } from "./selectionContent";

const bytes = (text: string) => new TextEncoder().encode(text).length;
/** Characters spelled by code, so no editor or tool can turn them into the raw character. */
const ch = (...codes: number[]) => String.fromCharCode(...codes);
const base = { path: "src/a.ts", checkoutName: "main", startLine: 3 };

describe("formatSelectionRef", () => {
  it("names a file, one line, or a range, and ends with the space an agent needs after a reference", () => {
    expect(formatSelectionRef("src/a.ts", null, null)).toBe("@src/a.ts ");
    expect(formatSelectionRef("src/a.ts", 4, null)).toBe("@src/a.ts#L4 ");
    expect(formatSelectionRef("src/a.ts", 4, 4)).toBe("@src/a.ts#L4 ");
    expect(formatSelectionRef("src/a.ts", 9, 4)).toBe("@src/a.ts#L4-L9 ");
  });
});

describe("composeSelectionContent", () => {
  it("heads a fenced block with the path, range, checkout and language", () => {
    const out = composeSelectionContent({ ...base, lines: ["const a = 1;", "const b = 2;"] });
    expect(out.text).toBe("File: src/a.ts, lines 3-4, checkout main\n```typescript\nconst a = 1;\nconst b = 2;\n```");
    expect(out.omittedLines).toBe(0);
  });

  it("names a single line without a range and leaves plain text without a language", () => {
    const out = composeSelectionContent({ ...base, path: "notes.xyz", lines: ["hello"] });
    expect(out.text).toBe("File: notes.xyz, line 3, checkout main\n```\nhello\n```");
  });

  it("says the text is an unsaved buffer, so the agent knows it is not on disk", () => {
    const out = composeSelectionContent({ ...base, lines: ["x"], unsaved: true });
    expect(out.text.split("\n")[0]).toBe("File: src/a.ts, line 3, checkout main (unsaved buffer)");
  });

  it("refuses an empty selection instead of sending an empty fence", () => {
    expect(composeSelectionContent({ ...base, lines: [] }).text).toBe("");
  });

  it("drops the CR a CRLF file leaves at a line's end and shows one inside a line, keeping line numbers true", () => {
    const out = composeSelectionContent({ ...base, path: "a.txt", lines: ["one\r", "two\rthree"] });
    expect(out.text).not.toContain("\r");
    expect(out.text.split("\n").slice(2, 4)).toEqual(["one", "two\\rthree"]);
    expect(out.text.split("\n")[0]).toContain("lines 3-4");
  });

  it("opens a fence longer than any backtick run so the text cannot close it early", () => {
    const out = composeSelectionContent({ ...base, path: "a.md", lines: ["```", "inner ````` run", "```"] });
    const lines = out.text.split("\n");
    expect(lines[1]).toBe("``````markdown");
    expect(lines[lines.length - 1]).toBe("``````");
  });

  it("shows control and bidirectional characters as visible escapes", () => {
    const [rlo, esc, nul, lsep] = [ch(0x202e), ch(0x1b), ch(0x00), ch(0x2028)];
    const out = composeSelectionContent({ ...base, lines: [`a${rlo}b${esc}[31m${nul}c${lsep}d`] });
    for (const raw of [rlo, esc, nul, lsep]) expect(out.text).not.toContain(raw);
    expect(out.text).toContain("a\\u{202e}b\\u{001b}[31m\\u{0000}c\\u{2028}d");
  });

  it("escapes the same characters in a path that names a heading", () => {
    const out = composeSelectionContent({ ...base, path: "a\n# injected.ts", lines: ["x"] });
    expect(out.text.split("\n")[0]).toContain("a\\n# injected.ts");
  });

  it("replaces a lone surrogate with U+FFFD and counts the bytes it will be sent as", () => {
    const [high, low, replacement] = [ch(0xd800), ch(0xdc00), ch(0xfffd)];
    const out = composeSelectionContent({ ...base, lines: [`a${high}b`, low] });
    expect(out.text).toContain(`a${replacement}b`);
    expect(out.text).not.toContain(high);
    expect(out.text).not.toContain(low);
    // Well-formed: encoding it loses nothing, so the byte count is the count that is sent.
    expect(new TextDecoder().decode(new TextEncoder().encode(out.text))).toBe(out.text);
  });

  it("keeps astral characters whole", () => {
    const smile = String.fromCodePoint(0x1f600);
    expect(composeSelectionContent({ ...base, lines: [`x${smile}y`] }).text).toContain(`x${smile}y`);
  });

  it("fits the bound exactly and says nothing when nothing was left out", () => {
    const head = "File: a.txt, line 3, checkout main\n```\n";
    const tail = "\n```";
    const body = "x".repeat(AGENT_MESSAGE_MAX_BYTES - bytes(head + tail));
    const out = composeSelectionContent({ ...base, path: "a.txt", lines: [body] });
    expect(bytes(out.text)).toBe(AGENT_MESSAGE_MAX_BYTES);
    expect(out.omittedLines).toBe(0);
    expect(out.text.endsWith("```")).toBe(true);
  });

  it("cuts at a line boundary past the bound and states how many lines were left out", () => {
    const lines = Array.from({ length: 400 }, (_, i) => `line ${i + 1} ${"y".repeat(40)}`);
    const out = composeSelectionContent({ ...base, path: "a.txt", lines });
    expect(bytes(out.text)).toBeLessThanOrEqual(AGENT_MESSAGE_MAX_BYTES);
    expect(out.omittedLines).toBeGreaterThan(0);
    const kept = 400 - out.omittedLines;
    const marker = out.text.split("\n").at(-1) ?? "";
    expect(marker).toContain(`${out.omittedLines} more lines`);
    expect(marker).toContain(`lines ${3 + kept}-${3 + 399}`);
    // The marker sits outside the fence, after its close.
    expect(out.text.split("\n").at(-2)).toBe("```");
    // Only whole lines were kept.
    expect(out.text).toContain(`line ${kept} `);
    expect(out.text).not.toContain(`line ${kept + 1} `);
  });

  it("reports a single line longer than the bound as left out, not as a shortened line", () => {
    const out = composeSelectionContent({ ...base, path: "a.txt", lines: ["z".repeat(AGENT_MESSAGE_MAX_BYTES * 2)] });
    expect(bytes(out.text)).toBeLessThanOrEqual(AGENT_MESSAGE_MAX_BYTES);
    expect(out.text).not.toContain("zzzz");
    expect(out.omittedLines).toBe(1);
    expect(out.text).toContain("not included");
  });

  it("holds the bound when escaping makes the text longer than it looked", () => {
    const lines = Array.from({ length: 2000 }, () => ch(1).repeat(20));
    const out = composeSelectionContent({ ...base, path: "a.txt", lines });
    expect(bytes(out.text)).toBeLessThanOrEqual(AGENT_MESSAGE_MAX_BYTES);
    expect(out.omittedLines).toBeGreaterThan(0);
  });

  it("stays inside the bound even when the path alone is enormous", () => {
    const out = composeSelectionContent({ ...base, path: "p".repeat(20_000) + ".ts", lines: ["x"] });
    expect(bytes(out.text)).toBeLessThanOrEqual(AGENT_MESSAGE_MAX_BYTES);
  });
});
