import type { ILogger } from "@xterm/xterm";

export interface XtermLogSink {
  log(...args: unknown[]): void;
  debug(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
}

const LOG_PREFIX = "xterm.js: ";

/**
 * `@xterm/addon-webgl`'s `createShader` and `createProgram` pass
 * `gl.getShaderInfoLog(shader)` / `gl.getProgramInfoLog(program)` straight to
 * `logService.error(...)`. Per the WebGL spec both return `null` when the
 * context is lost (wake from sleep, GPU process restart, or WebKit context
 * eviction), and some drivers return an empty string. xterm's `LogService._log`
 * coerces `'' + message` before invoking a custom logger (turning `null` into
 * `"null"`), and without a custom logger emits `console.error("xterm.js: null")`
 * right before throwing into `createGpuRenderer`'s DOM fallback.
 */
function isEmptyShaderInfoLog(message: unknown): boolean {
  if (message == null) return true;
  if (typeof message !== "string") return false;
  // Strip NUL terminators too: some OpenGL/ANGLE/Metal drivers return "\0"
  // for an empty shader/program info log, and String.prototype.trim keeps "\0".
  const trimmed = message.replace(/\0/g, "").trim();
  return trimmed === "" || trimmed === "null" || trimmed === "undefined";
}

function formatXtermMessage(message: unknown): string {
  return `${LOG_PREFIX}${String(message)}`;
}

/**
 * Builds an xterm `ILogger` that preserves xterm's `"xterm.js: "` prefix and
 * forwards genuine errors and warnings through `console` (keeping global
 * diagnostics in the path), while downgrading xterm's null/empty shader
 * info-log calls on lost WebGL contexts to `console.debug`.
 *
 * Reads `console` at call time when no `sink` is supplied so
 * `installGlobalDiagnostics`'s wrappers remain active regardless of module
 * initialization order.
 */
export function createXtermLogger(sink?: XtermLogSink): ILogger {
  const target = () => sink ?? console;
  return {
    trace(message: string, ...args: unknown[]) {
      target().log(formatXtermMessage(message), ...args);
    },
    debug(message: string, ...args: unknown[]) {
      target().log(formatXtermMessage(message), ...args);
    },
    info(message: string, ...args: unknown[]) {
      target().info(formatXtermMessage(message), ...args);
    },
    warn(message: string, ...args: unknown[]) {
      target().warn(formatXtermMessage(message), ...args);
    },
    error(message: string | Error, ...args: unknown[]) {
      if (args.length === 0 && isEmptyShaderInfoLog(message)) {
        target().debug("xterm.js: suppressed empty WebGL shader/program info log (context lost)");
        return;
      }
      if (message instanceof Error) {
        target().error("xterm.js:", message, ...args);
        return;
      }
      target().error(formatXtermMessage(message), ...args);
    },
  };
}

/** Stateless logger shared across `TerminalSession` instances. */
export const xtermLogger: ILogger = createXtermLogger();
