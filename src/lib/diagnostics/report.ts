import { formatError } from "../ui/formatError";
import { diagnostics, type DiagnosticsStore } from "./diagnostics";
import type { PersistedLog } from "./types";

/**
 * One seam between panel-level caught errors and the persistent diagnostics
 * ring: every panel catch formats once through `formatError`, feeds the ring
 * tagged with its panel source, and hands back the exact banner text the
 * panel was already showing — so the UI contract is unchanged and the log
 * side-effect is purely additive.
 */

/** Panels that report caught errors through this seam. */
export type PanelSource =
  | "blame"
  | "reflog"
  | "stack"
  | "worktrees"
  | "coverage"
  | "storage"
  | "github"
  | "health"
  | "ops"
  | "clone"
  | "rebase"
  | "remotes"
  | "submodules"
  | "conflict";

export interface ReporterOptions {
  /** Defaults to "warning": a failed panel load is degraded, not fatal. */
  severity?: "error" | "warning";
}

/**
 * Builds a reporter against an injected sink (unit-testable, mirroring
 * `installGlobalDiagnostics`). The returned reporter records `err` and
 * RETURNS the human-readable banner text.
 */
export function createReporter(
  sink: Pick<DiagnosticsStore, "error" | "warn">,
): (source: PanelSource, err: unknown, opts?: ReporterOptions) => string {
  return (source, err, opts = {}) => {
    // `formatError` is throw-resistant but not throw-proof: a hostile
    // getter on `.message` throws before its own guards run, so wrap here.
    let message: string;
    try {
      message = formatError(err);
    } catch {
      message = "Unknown error";
    }
    if ((opts.severity ?? "warning") === "error") sink.error(source, message);
    else sink.warn(source, message);
    return message;
  };
}

/** App-wide reporter bound to the diagnostics singleton. */
export const reportPanelError = createReporter(diagnostics);

/**
 * Appends the backend log tail to a copied diagnostics report. Empty tails
 * (command missing, IPC failure) leave the report byte-identical.
 */
export function withBackendLogSection(report: string, lines: readonly string[]): string {
  if (lines.length === 0) return report;
  return [
    report,
    "",
    `Backend log (last ${lines.length})`,
    ...lines.map((line) => `  ${line}`),
  ].join("\n");
}

/**
 * Appends the backend's durable log — the half that survives the process that
 * wrote it, and therefore the only half that can still describe a crash after
 * the relaunch.
 *
 * Unlike {@link withBackendLogSection} this always writes a section. An
 * omitted one would be read as "the backend had nothing to say", which is the
 * same shape as "the backend could not be asked" and as "this build keeps no
 * durable log" — three different facts, only one of them reassuring.
 */
export function withPersistedLogSection(report: string, log: PersistedLog): string {
  const header = log.path
    ? `Durable backend log (${log.lines.length} line(s) from ${log.path})`
    : "Durable backend log — unavailable";
  const note = log.degraded ? [`  ! incomplete: ${log.degraded}`] : [];
  return [report, "", header, ...note, ...log.lines.map((line) => `  ${line}`)].join("\n");
}
