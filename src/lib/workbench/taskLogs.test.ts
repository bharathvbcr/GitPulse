import { describe, expect, it } from "vitest";
import { fenceLogs, formatLogsSection, LOGS_CAP_BYTES, sanitizeLogs } from "./taskLogs";

describe("sanitizeLogs", () => {
  it("keeps stack frames and error codes and strips ANSI, NULs, and CR", () => {
    const dumped = "\u001b[31merror: E42\u001b[0m\r\n    at src/main.rs:12\u0000";
    const result = sanitizeLogs(dumped);
    expect(result.text).toBe("error: E42\n    at src/main.rs:12");
    expect(result.truncated).toBe(false);
    expect(result.notice).toBe("");
    expect(result.text).toContain("E42");
    expect(result.text).not.toContain("\u001b");
    expect(result.text).not.toContain("\u0000");
  });

  it("strips OSC hyperlinks and leftover ESC sequences", () => {
    const dumped = "\u001b]8;;https://example.test/e42\u001b\\click me\u001b]8;;\u001b\\\u001b]0;title\u0007 done";
    expect(sanitizeLogs(dumped).text).toBe("click me done");
  });

  it("does not feed logs through title extraction or trim evidence indentation", () => {
    const dumped = "    panic at 'E42'\n\tnote: run with RUST_BACKTRACE=1";
    expect(sanitizeLogs(dumped).text).toBe(dumped);
  });

  it("treats non-strings, whitespace-only, and ANSI-only pastes as empty rather than invented text", () => {
    expect(sanitizeLogs(null).text).toBe("");
    expect(sanitizeLogs(12).text).toBe("");
    expect(sanitizeLogs("   \n\t").text).toBe("");
    expect(sanitizeLogs("\u001b[2J\u001b[H").text).toBe("");
    expect(sanitizeLogs({ toString: () => "E42" }).text).toBe("");
  });

  it("announces a byte cut instead of silently dropping the tail", () => {
    const tail = "the reproduction ends with E42";
    const oversized = `${"x".repeat(LOGS_CAP_BYTES)}\n${tail}`;
    const result = sanitizeLogs(oversized);
    expect(result.truncated).toBe(true);
    expect(result.text).not.toContain(tail);
    expect(result.text).toContain("[logs truncated:");
    expect(result.text).toContain("bytes kept]");
    expect(new TextEncoder().encode(result.text).length).toBeLessThanOrEqual(LOGS_CAP_BYTES);
    expect(sanitizeLogs("x".repeat(LOGS_CAP_BYTES)).truncated).toBe(false);
  });

  it("does not split a UTF-8 code point at the cap", () => {
    const result = sanitizeLogs("😀".repeat(LOGS_CAP_BYTES));
    expect(result.text.isWellFormed()).toBe(true);
    expect(result.text).not.toContain("\uFFFD");
    expect(new TextEncoder().encode(result.text).length).toBeLessThanOrEqual(LOGS_CAP_BYTES);
  });
});

describe("fenceLogs / formatLogsSection", () => {
  it("uses a fence longer than any backtick run in the logs", () => {
    const text = "before ```` inner";
    const fenced = fenceLogs(text);
    expect(fenced.startsWith("`````\n")).toBe(true);
    expect(fenced.endsWith("\n`````")).toBe(true);
    expect(fenced).toContain(text);
  });

  it("omits the brief section when there is nothing to send", () => {
    expect(formatLogsSection("")).toBeNull();
    expect(formatLogsSection("\u001b[0m")).toBeNull();
    const section = formatLogsSection("error: E42");
    expect(section).toContain("## Raw logs");
    expect(section).toContain("error: E42");
    expect(section).toContain("Pasted evidence");
  });
});
