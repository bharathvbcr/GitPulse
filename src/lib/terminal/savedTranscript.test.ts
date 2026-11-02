import { describe, expect, it } from "vitest";
import {
  parseSavedTranscript,
  readSavedTranscript,
  transcriptNotices,
  transcriptPlainText,
  type FoundTranscript,
  type TranscriptDeps,
} from "./savedTranscript";

function b64(text: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(text)));
}

function foundWire(text: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    state: "found",
    data_b64: b64(text),
    bytes: new TextEncoder().encode(text).length,
    retained: new TextEncoder().encode(text).length,
    truncated: false,
    gap_markers: 0,
    redacted: false,
    live: false,
    settled: true,
    recording_error: null,
    ...over,
  };
}

function deps(answer: () => Promise<unknown>, deadlineMs = 1000): TranscriptDeps {
  return { read: answer, deadlineMs };
}

describe("parseSavedTranscript", () => {
  it("decodes a found transcript, multi-byte text included", () => {
    const parsed = parseSavedTranscript(foundWire("héllo \u001b[31mred\u001b[0m\r\n"));
    expect(parsed.kind).toBe("found");
    if (parsed.kind !== "found") return;
    expect(parsed.text).toBe("héllo \u001b[31mred\u001b[0m\r\n");
    expect(parsed.live).toBe(false);
    expect(parsed.recordingError).toBeNull();
  });

  it("keeps the four states the host can answer apart", () => {
    expect(parseSavedTranscript({ state: "missing" })).toEqual({ kind: "missing" });
    expect(parseSavedTranscript({ state: "unreadable", reason: "denied" })).toEqual({ kind: "unreadable", reason: "denied" });
    expect(parseSavedTranscript({ state: "unavailable", reason: "in-process" })).toEqual({
      kind: "unavailable",
      reason: "in-process",
    });
  });

  it("turns anything it cannot vouch for into failed, never missing", () => {
    const bad: unknown[] = [
      null,
      "found",
      [],
      {},
      { state: "perhaps" },
      { state: "unreadable" },
      { state: "unreadable", reason: "" },
      foundWire("x", { data_b64: 5 }),
      foundWire("x", { bytes: -1 }),
      foundWire("x", { gap_markers: 1.5 }),
      foundWire("x", { truncated: "no" }),
      foundWire("x", { settled: undefined }),
      foundWire("x", { recording_error: 3 }),
      foundWire("x", { data_b64: "***not base64***" }),
      foundWire("hello", { bytes: 2 }),
    ];
    for (const raw of bad) {
      const parsed = parseSavedTranscript(raw);
      expect(parsed.kind, JSON.stringify(raw)).toBe("failed");
    }
  });
});

describe("readSavedTranscript", () => {
  it("asks for the session's transcript with its bounds and parses the answer", async () => {
    const asked: Array<{ sessionId: string; maxBytes: number; redact: boolean }> = [];
    const read = await readSavedTranscript(
      "term-1-a",
      { maxBytes: 4096, redact: true },
      deps(async () => foundWire("done\r\n")),
    );
    expect(read.kind).toBe("found");
    const spy: TranscriptDeps = {
      read: async (args) => {
        asked.push(args);
        return { state: "missing" };
      },
      deadlineMs: 1000,
    };
    expect(await readSavedTranscript("term-1-a", { maxBytes: 4096, redact: true }, spy)).toEqual({ kind: "missing" });
    expect(asked).toEqual([{ sessionId: "term-1-a", maxBytes: 4096, redact: true }]);
  });

  it("reports a rejected call as failed with its reason, not as missing", async () => {
    const read = await readSavedTranscript(
      "term-1-a",
      {},
      deps(async () => {
        throw new Error("The connection to the terminal host was lost");
      }),
    );
    expect(read).toEqual({ kind: "failed", reason: "The connection to the terminal host was lost" });
  });

  it("reports a call that never answers as failed once its deadline passes", async () => {
    const read = await readSavedTranscript(
      "term-1-a",
      {},
      deps(() => new Promise(() => {}), 30),
    );
    expect(read.kind).toBe("failed");
  });
});

describe("transcriptNotices", () => {
  const base: FoundTranscript = {
    kind: "found",
    text: "",
    bytes: 10,
    retained: 10,
    truncated: false,
    gapMarkers: 0,
    redacted: false,
    live: false,
    settled: true,
    recordingError: null,
  };

  it("says nothing about a complete, finished transcript", () => {
    expect(transcriptNotices(base)).toEqual([]);
  });

  it("names every way the text falls short", () => {
    const notices = transcriptNotices({
      ...base,
      bytes: 1000,
      retained: 5000,
      truncated: true,
      gapMarkers: 2,
      redacted: true,
      live: true,
      settled: false,
      recordingError: "disk full.",
    });
    expect(notices).toHaveLength(6);
    expect(notices[0]).toContain("last 1,000 of 5,000");
    expect(notices.join("\n")).toMatch(/2 stretches/);
    expect(notices.join("\n")).toContain("disk full.");
    expect(notices.join("\n")).toContain("still running");
    expect(notices.join("\n")).toContain("end may be missing");
    expect(notices.join("\n")).toContain("masked");
  });

  it("tells a rotated-away start from a clipped read", () => {
    expect(transcriptNotices({ ...base, truncated: true })[0]).toContain("rotated away");
  });
});

describe("transcriptPlainText", () => {
  it("removes colour, cursor and title sequences and keeps the words", () => {
    const text = "\u001b]0;title\u0007\u001b[1;32mok\u001b[0m \u001b[2K\u001b[?25lgo\u001b(B done\r\n";
    expect(transcriptPlainText(text)).toBe("ok go done\n");
  });

  it("lets a carriage return overwrite its line, as a progress bar means it", () => {
    expect(transcriptPlainText("10%\r50%\r100%\r\nnext\r\n")).toBe("100%\nnext\n");
  });

  it("erases a backspaced character and drops other control bytes", () => {
    expect(transcriptPlainText("helo\blo\u0007\u0000!\n")).toBe("hello!\n");
  });

  it("leaves a gap marker readable", () => {
    const marker = "[gitpulse: 4096 bytes of output were not recorded]";
    expect(transcriptPlainText(`a\r\n${marker}\r\nb`)).toBe(`a\n${marker}\nb`);
  });
});
