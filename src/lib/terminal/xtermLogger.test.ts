import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { get } from "svelte/store";
import { describe, expect, it, vi } from "vitest";
import { createDiagnostics, installGlobalDiagnostics } from "../diagnostics/diagnostics";
import { createXtermLogger, xtermLogger, type XtermLogSink } from "./xtermLogger";

const require = createRequire(import.meta.url);
const { Terminal } = require("@xterm/xterm") as {
  Terminal: new (options?: Record<string, unknown>) => {
    _core: {
      _logService: {
        error(message: unknown, ...args: unknown[]): void;
        warn(message: unknown, ...args: unknown[]): void;
        info(message: unknown, ...args: unknown[]): void;
      };
    };
    dispose(): void;
  };
};

function makeSink(): XtermLogSink & {
  calls: Record<"log" | "debug" | "info" | "warn" | "error", unknown[][]>;
} {
  const calls = {
    log: [] as unknown[][],
    debug: [] as unknown[][],
    info: [] as unknown[][],
    warn: [] as unknown[][],
    error: [] as unknown[][],
  };
  return {
    calls,
    log: (...args) => calls.log.push(args),
    debug: (...args) => calls.debug.push(args),
    info: (...args) => calls.info.push(args),
    warn: (...args) => calls.warn.push(args),
    error: (...args) => calls.error.push(args),
  };
}

describe("createXtermLogger", () => {
  it("downgrades null/empty/NUL-only WebGL shader info-log errors to debug", () => {
    const sink = makeSink();
    const logger = createXtermLogger(sink);

    logger.error("null");
    logger.error("");
    logger.error("   ");
    logger.error("\0");
    logger.error("\0\n");
    logger.error("undefined");
    logger.error(null as unknown as string);

    expect(sink.calls.error).toEqual([]);
    expect(sink.calls.debug).toHaveLength(7);
  });

  it("preserves real xterm errors, Error instances, and calls with extra arguments", () => {
    const sink = makeSink();
    const logger = createXtermLogger(sink);
    const err = new Error("null");

    logger.error("ERROR: 0:1: undeclared identifier");
    logger.error("Parsing error: ", { state: 1 });
    logger.error("null", { extra: true });
    logger.error(err);

    expect(sink.calls.error).toEqual([
      ["xterm.js: ERROR: 0:1: undeclared identifier"],
      ["xterm.js: Parsing error: ", { state: 1 }],
      ["xterm.js: null", { extra: true }],
      ["xterm.js:", err],
    ]);
    expect(sink.calls.debug).toEqual([]);
  });

  it("prefixes trace, debug, info and warn like xterm's default LogService", () => {
    const sink = makeSink();
    const logger = createXtermLogger(sink);

    logger.trace("t", 1);
    logger.debug("d", 2);
    logger.info("i", 3);
    logger.warn("w", 4);

    expect(sink.calls.log).toEqual([
      ["xterm.js: t", 1],
      ["xterm.js: d", 2],
    ]);
    expect(sink.calls.info).toEqual([["xterm.js: i", 3]]);
    expect(sink.calls.warn).toEqual([["xterm.js: w", 4]]);
  });
});

describe("real @xterm/xterm LogService integration", () => {
  it("reproduces xterm.js: null without a custom logger and prevents it with createXtermLogger", () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    const defaultTerm = new Terminal({ cols: 40, rows: 10 });
    try {
      // Exact call made by @xterm/addon-webgl's WebglUtils.ts when
      // gl.getShaderInfoLog / gl.getProgramInfoLog returns null on a lost context.
      defaultTerm._core._logService.error(null);
      expect(errorSpy).toHaveBeenCalledWith("xterm.js: null");
    } finally {
      defaultTerm.dispose();
      errorSpy.mockRestore();
    }

    const sink = makeSink();
    const guardedTerm = new Terminal({
      cols: 40,
      rows: 10,
      logger: createXtermLogger(sink),
    });
    try {
      guardedTerm._core._logService.error(null);
      guardedTerm._core._logService.error("");
      guardedTerm._core._logService.error("\0");
      expect(sink.calls.error).toEqual([]);
      expect(sink.calls.debug).toHaveLength(3);

      guardedTerm._core._logService.error("ERROR: 0:1: shader compile failed");
      guardedTerm._core._logService.error("Parsing error: ", { code: 42 });
      guardedTerm._core._logService.warn("webgl context not restored");

      expect(sink.calls.error).toEqual([
        ["xterm.js: ERROR: 0:1: shader compile failed"],
        ["xterm.js: Parsing error: ", { code: 42 }],
      ]);
      expect(sink.calls.warn).toEqual([["xterm.js: webgl context not restored"]]);
    } finally {
      guardedTerm.dispose();
    }
  });

  it("keeps global diagnostics clean on lost-context null logs while still capturing real xterm errors", () => {
    const store = createDiagnostics({ storage: null, development: false });
    const debugSpy = vi.spyOn(console, "debug").mockImplementation(() => {});
    const originalError = console.error;
    const originalWarn = console.warn;
    console.error = () => {};
    console.warn = () => {};
    const uninstall = installGlobalDiagnostics(store, { target: null });
    const term = new Terminal({ cols: 40, rows: 10, logger: xtermLogger });
    try {
      term._core._logService.error(null);
      expect(get(store)).toEqual([]);

      term._core._logService.error("Parsing error: ", "bad-seq");
      expect(get(store)).toHaveLength(1);
      expect(get(store)[0]).toMatchObject({
        severity: "error",
        source: "console",
        message: "xterm.js: Parsing error: bad-seq",
      });
    } finally {
      term.dispose();
      uninstall();
      console.error = originalError;
      console.warn = originalWarn;
      debugSpy.mockRestore();
    }
  });

  it("wires xtermLogger and attachWebglAddon into TerminalSession.svelte", () => {
    const here = dirname(fileURLToPath(import.meta.url));
    const source = readFileSync(join(here, "..", "components", "TerminalSession.svelte"), "utf8");
    expect(source).toContain("logger: xtermLogger");
    expect(source).toContain("attachWebglAddon(t, () => new WebglAddon())");
  });
});
