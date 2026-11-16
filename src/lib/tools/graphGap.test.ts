import { describe, expect, it, vi } from "vitest";
import type { DevmapBuildOutcome } from "../codeintel/types";
import { buildGraphIndex, classifyGraphGap, readGraphReading } from "./graphGap";

describe("classifyGraphGap", () => {
  it("offers Setup when the graph is missing and the CLI is not installed", () => {
    expect(classifyGraphGap({ graph: "unavailable", devmapInstalled: false })).toBe("cli_missing");
  });

  it("offers Build index when the graph is missing and the CLI is installed", () => {
    expect(classifyGraphGap({ graph: "unavailable", devmapInstalled: true })).toBe("no_index");
  });

  it("does not claim the CLI is missing before the tools status has been read", () => {
    expect(classifyGraphGap({ graph: "unavailable", devmapInstalled: null })).toBe("no_index");
  });

  it("never prompts to install when the graph answers", () => {
    expect(classifyGraphGap({ graph: "available", devmapInstalled: false })).toBe("other");
  });

  it("never prompts to install when the status read itself could not run", () => {
    expect(classifyGraphGap({ graph: "unknown", devmapInstalled: false })).toBe("other");
  });
});

describe("readGraphReading", () => {
  it("reads available and unavailable from the status", async () => {
    expect(await readGraphReading("/r", async () => ({ available: true }))).toBe("available");
    expect(await readGraphReading("/r", async () => ({ available: false }))).toBe("unavailable");
  });

  it("reports a thrown read as unknown, not as unavailable", async () => {
    const read = vi.fn().mockRejectedValue(new Error("ipc down"));
    expect(await readGraphReading("/r", read)).toBe("unknown");
    expect(read).toHaveBeenCalledWith("/r");
  });
});

function outcome(over: Partial<DevmapBuildOutcome>): DevmapBuildOutcome {
  return {
    ok: true,
    binary: "devmap",
    lookup: "path",
    stdout: "",
    stderr: "",
    timed_out: false,
    ...over,
  } as DevmapBuildOutcome;
}

describe("buildGraphIndex", () => {
  it("succeeds when the build does", async () => {
    expect(await buildGraphIndex("/r", async () => outcome({ ok: true }))).toEqual({ ok: true });
  });

  it("returns the CLI's own failure reason", async () => {
    const result = await buildGraphIndex("/r", async () =>
      outcome({ ok: false, stderr: "schema too new" }),
    );
    expect(result).toEqual({ ok: false, error: "schema too new" });
  });

  it("treats a thrown build as a failure", async () => {
    const result = await buildGraphIndex("/r", async () => {
      throw new Error("spawn refused");
    });
    expect(result).toEqual({ ok: false, error: "spawn refused" });
  });
});
