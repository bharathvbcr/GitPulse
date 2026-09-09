import { invoke } from "@tauri-apps/api/core";
import { copyText } from "../desktop/clipboard";
import { APP_BUILD_ID, APP_VERSION, formatDiagnosticFailure, redactDiagnosticText } from "../diagnostics/diagnostics";
import type { PersistedLog } from "../diagnostics/types";
import type { DevmapCliStatus } from "./types";
import type { LiveIndexSnapshot } from "./liveIndex";

const LOG_LINES = 500;
const SECTION_CHARS = 64 * 1024;
export const DEVMAP_LOG_READ_TIMEOUT_MS = 5000;

export interface DevmapDiagnosticContext {
  repository: string;
  view: string;
  building: boolean;
  cli: DevmapCliStatus | null;
  map: { available: boolean; path?: string | null; reason?: string | null } | null;
  graph: { available: boolean; kind: string; reason?: string | null } | null;
  liveIndex: LiveIndexSnapshot;
  errors: readonly (string | null)[];
}

function boundedSection(text: string): string {
  const safe = redactDiagnosticText(text);
  if (safe.length <= SECTION_CHARS) return safe;
  return `${safe.slice(0, SECTION_CHARS / 2)}\n[section truncated: showing ${SECTION_CHARS} of ${safe.length} characters]\n${safe.slice(-SECTION_CHARS / 2)}`;
}

/** Filter only native DevMap records. Every start event names its repository;
 * run_id joins subsequent progress and completion events, including after a crash. */
function nativeSection(label: string, lines: readonly string[]): string {
  const devmap = lines.filter((line) => line.includes(" [devmap] "));
  return boundedSection([
    `${label}: ${devmap.length} DevMap entries in ${lines.length} sampled backend entries (limit ${LOG_LINES}).`,
    "Recent tail across repositories; older entries may have rotated out. Match repository and run_id.",
    ...(devmap.length ? devmap : ["No DevMap entries in this sampled tail."]),
  ].join("\n"));
}

export function formatDevmapLogs(
  context: DevmapDiagnosticContext,
  memory: PromiseSettledResult<string[]>,
  durable: PromiseSettledResult<PersistedLog>,
): string {
  const contextText = boundedSection(JSON.stringify(context, null, 2));
  const memoryText = memory.status === "fulfilled"
    ? nativeSection("Current session", memory.value)
    : `Current session log unavailable: ${formatDiagnosticFailure(memory.reason)}`;
  const durableText = durable.status === "fulfilled"
    ? [
        `Durable log path: ${durable.value.path || "unavailable"}`,
        ...(durable.value.degraded ? [`Durable log incomplete: ${durable.value.degraded}`] : []),
        nativeSection("Durable log", durable.value.lines),
      ].join("\n")
    : `Durable log unavailable: ${formatDiagnosticFailure(durable.reason)}`;
  return [
    `GitPulse DevMap logs — ${new Date().toISOString()}`,
    `GitPulse ${APP_VERSION} (${APP_BUILD_ID})`,
    "Map context at copy request:", contextText,
    "", boundedSection(memoryText), "", boundedSection(durableText),
  ].join("\n");
}

async function readLog<T>(request: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      request,
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error("log read timed out after 5 seconds")), DEVMAP_LOG_READ_TIMEOUT_MS);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}

export async function copyDevmapLogs(
  context: DevmapDiagnosticContext,
  isCurrent: () => boolean,
): Promise<"copied" | "failed" | "stale"> {
  const [memory, durable] = await Promise.allSettled([
    readLog(invoke<string[]>("cmd_diagnostic_log_tail", { maxLines: LOG_LINES })),
    readLog(invoke<PersistedLog>("cmd_diagnostic_persisted_log", { maxLines: LOG_LINES })),
  ]);
  // A repository switch or destroyed panel must not copy the previous repo.
  if (!isCurrent()) return "stale";
  return await copyText(formatDevmapLogs(context, memory, durable)) ? "copied" : "failed";
}
