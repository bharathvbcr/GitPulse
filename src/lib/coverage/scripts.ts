import type { CoverageFamilyStatus } from "./types";

export type CoveragePipelineKind = "setup" | "generate";

export interface CoveragePipelineStep {
  family: string;
  command: string;
  kind: CoveragePipelineKind;
}

export interface MissingCoveragePipeline {
  family: string;
  label: string;
  toolReady: boolean;
  toolDetail: string;
  durationHint: string;
  /** Rust workspace commands are cumulative; other ecosystem commands are alternatives. */
  mode: "all" | "first_success";
  steps: CoveragePipelineStep[];
}

function stringCommands(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((cmd): cmd is string => typeof cmd === "string" && cmd.trim().length > 0);
}

/**
 * Commands the coverage scanner planned for a family. Hostile or missing
 * payloads yield none — the UI must never invent a command the backend did
 * not send.
 */
export function suggestedCoverageCommands(
  family: CoverageFamilyStatus | null | undefined,
): string[] {
  return stringCommands(family?.suggested_commands);
}

export function setupCoverageCommands(
  family: CoverageFamilyStatus | null | undefined,
): string[] {
  return stringCommands(family?.setup_commands);
}

export function coverageFamilyRunLabel(family: string): string {
  switch (family) {
    case "javascript":
      return "JavaScript";
    case "rust":
      return "Rust";
    case "python":
      return "Python";
    case "go":
      return "Go";
    case "jvm":
      return "JVM";
    case "native":
      return "C / C++";
    case "swift":
      return "Swift";
    case "dotnet":
      return ".NET";
    case "php":
      return "PHP";
    case "ruby":
      return "Ruby";
    case "dart":
      return "Dart";
    case "beam":
      return "Elixir / Erlang";
    default:
      return family;
  }
}

/**
 * One MANVI pipeline per missing language: setup (only when the scanner
 * reported the generator toolchain missing) then generate. Empty when the
 * family has a report or the scanner planned no generate command.
 */
export function missingCoveragePipelines(
  families: CoverageFamilyStatus[] | null | undefined,
): MissingCoveragePipeline[] {
  if (!Array.isArray(families)) return [];
  const pipelines: MissingCoveragePipeline[] = [];
  for (const family of families) {
    if (!family || typeof family !== "object" || family.found === true) continue;
    const name = typeof family.family === "string" ? family.family : "";
    if (!name) continue;
    const generate = suggestedCoverageCommands(family);
    if (generate.length === 0) continue;
    const toolReady = family.tool_ready !== false;
    const setup = toolReady ? [] : setupCoverageCommands(family);
    const durationHint = typeof family.duration_hint === "string" ? family.duration_hint : "";
    const toolDetail =
      !toolReady && typeof family.tool_detail === "string" ? family.tool_detail : "";
    const steps: CoveragePipelineStep[] = [
      ...setup.map((command) => ({ family: name, command, kind: "setup" as const })),
      ...generate.map((command) => ({ family: name, command, kind: "generate" as const })),
    ];
    pipelines.push({
      family: name,
      label: coverageFamilyRunLabel(name),
      toolReady,
      toolDetail,
      durationHint,
      mode: name === "rust" ? "all" : "first_success",
      steps,
    });
  }
  return pipelines;
}
