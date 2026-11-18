import { describe, expect, it, vi } from "vitest";
import {
  createSelectToCopyController,
  formatSelectionForAgent,
  formatTerminalMarkdownBlock,
  MAX_AGENT_SELECTION_BYTES,
  MAX_OSC52_BASE64_BYTES,
  normalizeSelectedText,
  parseOsc52,
} from "./copyActions";

describe("normalizeSelectedText", () => {
  it("strips trailing spaces/tabs per line and end of selection while keeping indentation", () => {
    const raw = "  indented line   \t\nnext line  \r\n   last   ";
    expect(normalizeSelectedText(raw)).toBe("  indented line\nnext line\r\n   last");
  });

  it("handles empty strings cleanly", () => {
    expect(normalizeSelectedText("")).toBe("");
  });
});

describe("createSelectToCopyController", () => {
  it("coalesces multiple selection changes during a left-button drag until pointer release", async () => {
    let selection = "";
    const copied: string[] = [];
    const onCopied: string[] = [];
    const copy = vi.fn(async (text: string) => {
      copied.push(text);
      return true;
    });

    const controller = createSelectToCopyController({
      enabled: () => true,
      hasSelection: () => selection.length > 0,
      getSelection: () => selection,
      copy,
      onCopied: (text) => onCopied.push(text),
    });

    controller.onPointerDown(0);
    for (let i = 1; i <= 10; i++) {
      selection = `line ${i}   `;
      controller.onSelectionChange();
    }
    expect(copy).not.toHaveBeenCalled();

    controller.onPointerUp();
    await Promise.resolve();
    await Promise.resolve();

    expect(copy).toHaveBeenCalledTimes(1);
    expect(copied).toEqual(["line 10"]);
    expect(onCopied).toEqual(["line 10"]);
  });

  it("copies immediately on non-drag selection changes (double/triple click or Select Last Output)", async () => {
    let selection = "hello world  ";
    const copy = vi.fn(async () => true);
    const controller = createSelectToCopyController({
      enabled: () => true,
      hasSelection: () => selection.length > 0,
      getSelection: () => selection,
      copy,
    });

    controller.onSelectionChange();
    await Promise.resolve();
    expect(copy).toHaveBeenCalledTimes(1);
    expect(copy).toHaveBeenCalledWith("hello world");

    // Identical selection change does not duplicate clipboard writes.
    controller.onSelectionChange();
    expect(copy).toHaveBeenCalledTimes(1);

    // Clearing selection resets deduplication so selecting the same text again copies.
    selection = "";
    controller.onSelectionChange();
    selection = "hello world";
    controller.onSelectionChange();
    expect(copy).toHaveBeenCalledTimes(2);
  });

  it("refuses whitespace-only selections and respects enabled() === false", () => {
    let enabled = false;
    let selection = "   \t\n   ";
    const copy = vi.fn(async () => true);
    const controller = createSelectToCopyController({
      enabled: () => enabled,
      hasSelection: () => selection.length > 0,
      getSelection: () => selection,
      copy,
    });

    selection = "real text";
    controller.onSelectionChange();
    expect(copy).not.toHaveBeenCalled();

    enabled = true;
    selection = "   \t  \n  ";
    controller.onSelectionChange();
    expect(copy).not.toHaveBeenCalled();
  });

  it("does not swallow drag state for non-primary mouse buttons", () => {
    let selection = "double-clicked";
    const copy = vi.fn(async () => true);
    const controller = createSelectToCopyController({
      enabled: () => true,
      hasSelection: () => selection.length > 0,
      getSelection: () => selection,
      copy,
    });

    controller.onPointerDown(2);
    controller.onSelectionChange();
    expect(copy).toHaveBeenCalledWith("double-clicked");
  });
});

describe("parseOsc52", () => {
  const toB64 = (text: string) => Buffer.from(text, "utf8").toString("base64");

  it("decodes valid c; and empty-target ; UTF-8 clipboard payloads", () => {
    expect(parseOsc52(`c;${toB64("cargo test 🚀")}`)).toBe("cargo test 🚀");
    expect(parseOsc52(`;${toB64("plain text")}`)).toBe("plain text");
    expect(parseOsc52(`cp;${toB64("multi target")}`)).toBe("multi target");
  });

  it("refuses clipboard read probes (?) and empty/malformed/NUL/oversized payloads", () => {
    expect(parseOsc52("c;?")).toBeNull();
    expect(parseOsc52(";?")).toBeNull();
    expect(parseOsc52("c;")).toBeNull();
    expect(parseOsc52("no-semicolon")).toBeNull();
    expect(parseOsc52(`invalid_target;${toB64("hi")}`)).toBeNull();
    expect(parseOsc52("c;!!!")).toBeNull();
    expect(parseOsc52("c;a")).toBeNull();
    expect(parseOsc52(`c;${toB64("bad\0nul")}`)).toBeNull();
    expect(parseOsc52(`c;${"A".repeat(MAX_OSC52_BASE64_BYTES + 4)}`)).toBeNull();
  });
});

describe("Markdown and agent formatting", () => {
  it("formats fenced Markdown block with command, cwd, and exit code metadata", () => {
    const md = formatTerminalMarkdownBlock({
      command: "cargo test",
      cwd: "src-tauri",
      exitCode: 101,
      text: "test failed   \nline 2  ",
    });
    expect(md).toBe(
      "```text\n# cwd: src-tauri\n$ cargo test\n# exit 101\ntest failed\nline 2\n```",
    );
  });

  it("uses longer backtick fence if body already contains triple backticks", () => {
    const md = formatTerminalMarkdownBlock({
      text: "before\n```js\n1\n```\nafter",
    });
    expect(md.startsWith("````text\n")).toBe(true);
    expect(md.endsWith("\n````")).toBe(true);
  });

  it("formats and bounds selected output for an agent", () => {
    const short = formatSelectionForAgent({ text: "panic at main.rs:42   ", cwd: "src-tauri" });
    expect(short).toBe("Terminal output (in `src-tauri`):\n```text\npanic at main.rs:42\n```");

    const huge = "x".repeat(MAX_AGENT_SELECTION_BYTES + 500);
    const bounded = formatSelectionForAgent({ text: huge });
    expect(bounded).toContain("… [truncated]");
  });
});
