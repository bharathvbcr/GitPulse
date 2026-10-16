import { describe, expect, it } from "vitest";
import { createFrameProbe, handleFrameProbeCommand } from "../src/lib/diagnostics/frameProbe";
import { countDsrReplies, frame, MIN_RUNS, parseClock, parseFrameReplies, parseThreads, verdict } from "./tui-redraw-bench.mjs";

// Shaped from a real `ps -M -p <pid>` of a WebContent process on 2026-10-14:
// only the first row carries USER and COMMAND, and the thread rows end in a
// space where the command would be.
const PS_M = [
  "USER      PID   TT   %CPU STAT PRI     STIME     UTIME COMMAND",
  "me      18093   ??    0.0 S    20T   0:00.75   0:05.89 /System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
  "        18093         0.0 S    20T   0:00.02   0:00.09 ",
  "        18093         0.0 S    20T   1:02:03.50   0:00.00",
].join("\n");

describe("tui-redraw bench: sampling", () => {
  it("reads ps CPU times with minutes and with hours", () => {
    expect(parseClock("0:05.89")).toBeCloseTo(5.89);
    expect(parseClock("12:00.50")).toBeCloseTo(720.5);
    expect(parseClock("1:02:03.50")).toBeCloseTo(3723.5);
    expect(parseClock("5.89")).toBeNaN();
    expect(parseClock("")).toBeNaN();
  });

  it("takes the first thread as the main thread and sums every thread for the process", () => {
    const parsed = parseThreads(PS_M);
    expect(parsed?.threads).toBe(3);
    expect(parsed?.main).toBeCloseTo(0.75 + 5.89);
    expect(parsed?.total).toBeCloseTo(0.75 + 5.89 + 0.02 + 0.09 + 3723.5);
  });

  it("reports nothing rather than zero for a process ps did not list", () => {
    expect(parseThreads("USER      PID   TT   %CPU STAT PRI     STIME     UTIME COMMAND\n")).toBeNull();
  });

  it("counts cursor-position replies, including several in one chunk", () => {
    expect(countDsrReplies("\u001b[12;1R")).toBe(1);
    expect(countDsrReplies("x\u001b[1;80R\u001b[24;1Ry")).toBe(2);
    expect(countDsrReplies("\u001b[6n")).toBe(0);
  });

  it("reads the answer the terminal's own frame probe writes, among other replies", () => {
    // Produced by the module TerminalSession runs, so a change to the OSC
    // number or the reply's shape on either side fails here.
    let pending: ((time: number) => void) | null = null;
    const probe = createFrameProbe({ raf: (callback) => { pending = callback; return 1; }, cancel: () => { pending = null; } });
    handleFrameProbeCommand(probe, "frames-start;period=16.67");
    for (const time of [0, 16.67, 33.34, 66.68]) { const run = pending as ((time: number) => void) | null; pending = null; run?.(time); }
    const reply = handleFrameProbeCommand(probe, "frames-stop") ?? "";
    const empty = handleFrameProbeCommand(createFrameProbe({ raf: () => 1, cancel: () => {} }), "frames-stop") ?? "";
    const replies = parseFrameReplies(`\u001b[3;1R${reply}\u001b[4;1R${empty}`);
    expect(replies).toEqual([
      { n: 3, dropped: 1, fraction: 0.3333, period: 16.67, p50: 16.67, p95: 33.34, ms: 67 },
      { n: 0 },
    ]);
    expect(parseFrameReplies("\u001b]7700;frames;n=3")).toEqual([]);
  });

  it("repaints every row of the screen in place, without scrolling it", () => {
    const text = frame(3, 20, 5);
    for (let row = 1; row <= 5; row += 1) expect(text).toContain(`\u001b[${row};1H`);
    expect(text).not.toContain("\n");
    // Successive frames differ, so no renderer can skip a frame as unchanged.
    expect(frame(4, 20, 5)).not.toBe(text);
  });
});

type Run = { mode: string; arm: string; transparency: boolean; main_thread_cpu_s: number; wall_s: number;
  dsr_n: number; dsr_p95_ms: number | null; dsr_lost: number; mb_per_s?: number; dropped_frame_fraction?: number; at: string };

/** N interleaved runs of every mode and arm, with the figures given per arm. */
function runs(figures: Record<string, Partial<Run>>, n = MIN_RUNS): Run[] {
  const out: Run[] = [];
  let clock = 0;
  for (let i = 0; i < n; i += 1) {
    for (const mode of ["redraw", "flood", "idle"]) {
      for (const arm of i % 2 ? ["webgl", "dom"] : ["dom", "webgl"]) {
        out.push({
          mode, arm, transparency: true, wall_s: mode === "idle" ? 60 : 30, dsr_n: mode === "idle" ? 0 : 100,
          dsr_p95_ms: mode === "idle" ? null : 20, dsr_lost: 0, main_thread_cpu_s: 10, mb_per_s: 50,
          dropped_frame_fraction: 0.01, at: new Date(Date.UTC(2026, 9, 14, 0, 0, clock++)).toISOString(),
          ...figures[`${mode}/${arm}`],
        });
      }
    }
  }
  return out;
}

describe("tui-redraw bench: the win rule", () => {
  const winning = { "redraw/dom": { main_thread_cpu_s: 12 }, "redraw/webgl": { main_thread_cpu_s: 5 }, "idle/dom": { main_thread_cpu_s: 0.6 }, "idle/webgl": { main_thread_cpu_s: 0.9 } };

  it("ships when main-thread CPU halves and nothing else is worse", () => {
    const result = verdict(runs(winning));
    expect(result).toMatchObject({ decided: true, ships: true, reasons: [] });
  });

  it("does not ship at less than 2x", () => {
    const result = verdict(runs({ ...winning, "redraw/webgl": { main_thread_cpu_s: 6.5 } }));
    expect(result.ships).toBe(false);
    expect(result.reasons.join()).toContain("not 2x lower");
  });

  it("does not ship when any guard metric is worse", () => {
    expect(verdict(runs({ ...winning, "redraw/webgl": { main_thread_cpu_s: 5, dsr_p95_ms: 21 } })).reasons.join()).toContain("echo p95");
    expect(verdict(runs({ ...winning, "redraw/webgl": { main_thread_cpu_s: 5, dropped_frame_fraction: 0.02 } })).reasons.join()).toContain("dropped-frame");
    expect(verdict(runs({ ...winning, "flood/webgl": { mb_per_s: 49 } })).reasons.join()).toContain("flood");
    // 1 pp of a core over 60 s is 0.6 CPU-s; 1.3 s more is over it.
    expect(verdict(runs({ ...winning, "idle/webgl": { main_thread_cpu_s: 1.9 } })).reasons.join()).toContain("idle");
  });

  it("is undecided, never a pass, without the dropped-frame fraction", () => {
    const records = runs(winning).map(({ dropped_frame_fraction: _, ...rest }) => rest);
    const result = verdict(records);
    expect(result.decided).toBe(false);
    expect(result.ships).toBeNull();
    expect(result.reasons.join()).toContain("dropped-frame fraction not measured");
  });

  it("is undecided with fewer than N runs, mixed transparency, lost replies or no interleaving", () => {
    expect(verdict(runs(winning, MIN_RUNS - 1)).reasons.join()).toContain(`need ${MIN_RUNS}`);
    const mixed = runs(winning);
    mixed[0].transparency = false;
    expect(verdict(mixed).reasons.join()).toContain("different allowTransparency");
    const lost = runs(winning);
    lost[0].dsr_lost = 1;
    expect(verdict(lost).decided).toBe(false);
    const blocked = runs(winning).sort((a, b) => a.arm.localeCompare(b.arm)).map((run, i) => ({ ...run, at: new Date(Date.UTC(2026, 9, 14, 0, 0, i)).toISOString() }));
    expect(verdict(blocked).reasons.join()).toContain("not interleaved");
  });
});
