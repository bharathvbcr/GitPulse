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
  /** Rust workspace and Go module commands are cumulative; other ecosystem commands are alternatives. */
  mode: "all" | "first_success";
  steps: CoveragePipelineStep[];
}

function stringCommands(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((cmd): cmd is string => typeof cmd === "string" && cmd.trim().length > 0);
}

/**
 * Rust workspace and Go module commands are cumulative (every module must
 * run). Other ecosystems expose alternative runners, so the pipeline stops
 * at the first success.
 */
export function coverageCommandsAreCumulative(family: string): boolean {
  return family === "rust" || family === "go";
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
 * One family row, with the run decision already made.
 *
 * Every place that offers to generate coverage reads this and nothing else.
 * The panel used to render the same offer twice — once in the header strip
 * and once in the empty-state sidebar — from two different predicates, and
 * they disagreed. The strip drew a Run button whenever the scanner published
 * a command, while the click handler looked the family up in
 * `missingCoveragePipelines`, which additionally required a non-empty family
 * name; a row that failed only that second test rendered a button that did
 * nothing at all when pressed. The sidebar, meanwhile, showed a family's
 * `tool_detail` only when it came attached to a pipeline, so a family with no
 * planned generator — the `native` and `beam` rows — lost the one sentence
 * explaining why it could not run.
 *
 * Deciding once removes both.
 */
export interface CoverageFamilyView {
  /** The scanner's row, unmodified. Presentation reads names/colors/paths here. */
  status: CoverageFamilyStatus;
  family: string;
  label: string;
  found: boolean;
  /** The plan to run, or null when nothing about this family is runnable. */
  pipeline: MissingCoveragePipeline | null;
  /**
   * Generate commands offered as individual chips. Empty unless the toolchain
   * is ready — an individual command cannot stand in for a pipeline whose
   * first step installs the tool it needs.
   */
  commands: string[];
  /** Why this family is not ready, whether or not anything is runnable. */
  toolDetail: string;
  durationHint: string;
}

/**
 * The single decision point: for each family the scanner reported, what may
 * be run and what must be said.
 *
 * Hostile or missing payloads yield a view with no pipeline and no commands —
 * never an invented one.
 */
export function coverageFamilyViews(
  families: CoverageFamilyStatus[] | null | undefined,
): CoverageFamilyView[] {
  if (!Array.isArray(families)) return [];
  const views: CoverageFamilyView[] = [];
  for (const family of families) {
    if (!family || typeof family !== "object") continue;
    const name = typeof family.family === "string" ? family.family : "";
    if (!name) continue;
    const found = family.found === true;
    const toolReady = family.tool_ready !== false;
    const generate = suggestedCoverageCommands(family);
    const durationHint = typeof family.duration_hint === "string" ? family.duration_hint : "";
    // Carried whether or not a pipeline exists: for a family GitPulse cannot
    // generate at all, this sentence is the entire answer.
    const toolDetail =
      !toolReady && typeof family.tool_detail === "string" ? family.tool_detail : "";

    let pipeline: MissingCoveragePipeline | null = null;
    if (!found && generate.length > 0) {
      const setup = toolReady ? [] : setupCoverageCommands(family);
      pipeline = {
        family: name,
        label: coverageFamilyRunLabel(name),
        toolReady,
        toolDetail,
        durationHint,
        mode: coverageCommandsAreCumulative(name) ? "all" : "first_success",
        steps: [
          ...setup.map((command) => ({ family: name, command, kind: "setup" as const })),
          ...generate.map((command) => ({ family: name, command, kind: "generate" as const })),
        ],
      };
    }

    views.push({
      status: family,
      family: name,
      label: coverageFamilyRunLabel(name),
      found,
      pipeline,
      commands: !found && toolReady ? generate : [],
      toolDetail,
      durationHint,
    });
  }
  return views;
}

/**
 * One MANVI pipeline per missing language: setup (only when the scanner
 * reported the generator toolchain missing) then generate. Empty when the
 * family has a report or the scanner planned no generate command.
 *
 * A projection of {@link coverageFamilyViews} rather than a second traversal,
 * so "is this family runnable" has exactly one answer.
 */
export function missingCoveragePipelines(
  families: CoverageFamilyStatus[] | null | undefined,
): MissingCoveragePipeline[] {
  return coverageFamilyViews(families)
    .map((view) => view.pipeline)
    .filter((pipeline): pipeline is MissingCoveragePipeline => pipeline !== null);
}
