/**
 * In-app install / probe for the `devmap` and `manvi` CLIs.
 *
 * Mirrors the Rust `tool_install` module. Precedence: env → saved config →
 * PATH. Install uses a four-rung ladder (PATH → prebuilt → toolchain remote →
 * local checkout).
 */
import { invoke } from "@tauri-apps/api/core";

export type ExternalTool = "devmap" | "manvi";

export type ToolLookup =
  | "explicit_env"
  | "saved_config"
  | "path_search"
  | "missing"
  | "explicit_missing";

export type InstallRung =
  | "already_on_path"
  | "prebuilt_release"
  | "toolchain_remote"
  | "local_checkout";

export interface RungStatus {
  rung: InstallRung;
  available: boolean;
  block: string | null;
  command: string | null;
  cost: string;
}

export interface ToolStatus {
  tool: ExternalTool;
  installed: boolean;
  path: string | null;
  lookup: ToolLookup;
  version: string | null;
  reason: string | null;
  source_checkout: string | null;
  install_ready: boolean;
  install_block: string | null;
  install_command: string;
  selected_rung?: InstallRung | null;
  ladder?: RungStatus[];
  stale_config?: string | null;
}

export interface ToolsStatus {
  devmap: ToolStatus;
  manvi: ToolStatus;
}

export interface InstallOutcome {
  tool: ExternalTool;
  ok: boolean;
  binary: string | null;
  lookup: ToolLookup | null;
  version: string | null;
  source_used: string | null;
  command: string;
  exit_code: number | null;
  stdout: string;
  stderr: string;
  timed_out: boolean;
  cancelled: boolean;
  reason: string | null;
  rung?: InstallRung | null;
}

export interface ToolConfigView {
  config: {
    version: number;
    devmap: { source_root?: string | null; binary?: string | null };
    manvi: { source_root?: string | null; binary?: string | null };
    onboarding: {
      completed_at?: string | null;
      skipped_tools: string[];
      dismissed: boolean;
    };
  };
  path: string;
  stale: Array<{ field: string; path: string; reason: string; detail: string }>;
}

export interface PreflightReport {
  tool: ExternalTool;
  ok: boolean;
  requirements: Array<{
    name: string;
    found: boolean;
    path: string | null;
    version: string | null;
    satisfies: boolean;
    note: string | null;
  }>;
  estimate: string;
}

export interface VerifyReport {
  tool: ExternalTool;
  ok: boolean;
  binary: string | null;
  detail: string;
  store_schema?: number;
  expected_store_schema?: number;
  code_graph_schema?: number;
  expected_code_graph_schema?: number;
  manvi_protocol?: number;
  manvi_posture?: string;
}

/** Rate-limited progress event from `tool-install-progress`. */
export interface ToolInstallProgress {
  tool: string;
  line: string;
  rung?: string;
}

export interface LadderAssessment {
  tool: ExternalTool;
  selected: InstallRung | null;
  rungs: RungStatus[];
}

export function getExternalToolsStatus(): Promise<ToolsStatus> {
  return invoke<ToolsStatus>("cmd_external_tools_status");
}

export function installExternalTool(
  tool: ExternalTool,
  rung?: InstallRung | null,
): Promise<InstallOutcome> {
  return invoke<InstallOutcome>("cmd_external_tool_install", { tool, rung: rung ?? null });
}

export function cancelExternalToolInstall(): Promise<void> {
  return invoke("cmd_external_tool_install_cancel");
}

export function getToolConfig(): Promise<ToolConfigView> {
  return invoke<ToolConfigView>("cmd_tool_config_get");
}

export function saveToolConfig(config: ToolConfigView["config"]): Promise<ToolConfigView> {
  return invoke<ToolConfigView>("cmd_tool_config_save", { config });
}

export function getToolLadder(tool: ExternalTool): Promise<LadderAssessment> {
  return invoke<LadderAssessment>("cmd_tool_ladder", { tool });
}

export function getToolPreflight(tool: ExternalTool): Promise<PreflightReport> {
  return invoke<PreflightReport>("cmd_tool_preflight", { tool });
}

export function verifyTool(tool: ExternalTool): Promise<VerifyReport> {
  return invoke<VerifyReport>("cmd_tool_verify", { tool });
}

export function cloneToolSource(
  tool: ExternalTool,
  parentDir: string,
): Promise<{ tool: ExternalTool; ok: boolean; path: string | null; reason: string | null }> {
  return invoke("cmd_onboarding_clone_source", { tool, parentDir });
}

export function refreshToolCapability(): Promise<void> {
  return invoke("cmd_tool_capability_refresh");
}

/** One-line status for a tool row. */
export function toolStatusSummary(status: ToolStatus): string {
  if (status.lookup === "explicit_missing") {
    return status.reason ?? "Configured path is missing";
  }
  if (status.stale_config) {
    return `Stale saved path: ${status.stale_config}`;
  }
  if (status.installed) {
    const where = status.path ?? "unknown path";
    const ver = status.version ? ` · ${status.version}` : "";
    const via = status.lookup === "saved_config" ? " (saved config)" : "";
    return `${where}${via}${ver}`;
  }
  return status.reason ?? `${status.tool} is not installed`;
}

export function installButtonLabel(status: ToolStatus): string {
  if (status.installed) return `Update ${status.tool}`;
  return `Install ${status.tool}`;
}

/** Map failure modes that need different user actions. */
export type MapFailureMode = "no_cli" | "no_sqlite_store" | "no_json_artifact" | "other";

export function classifyMapFailure(input: {
  cliAvailable: boolean | null;
  cliReason?: string | null;
  mapAvailable: boolean | null;
  mapReason?: string | null;
  mapPath?: string | null;
}): MapFailureMode {
  if (input.cliAvailable === false) return "no_cli";
  const reason = (input.mapReason ?? "").toLowerCase();
  if (
    reason.includes("devmap.sqlite") ||
    reason.includes("sqlite") ||
    reason.includes("no store") ||
    reason.includes("index has not been built")
  ) {
    return "no_sqlite_store";
  }
  if (
    reason.includes("repo_map.json") ||
    reason.includes("no repo map") ||
    (input.mapAvailable === false && (input.mapPath ?? "").includes("repo_map.json"))
  ) {
    return "no_json_artifact";
  }
  if (input.mapAvailable === false) return "other";
  return "other";
}

export function mapFailureMessage(mode: MapFailureMode, fallback?: string | null): string {
  switch (mode) {
    case "no_cli":
      return "devmap CLI is not installed — run Setup to install it.";
    case "no_sqlite_store":
      return "No codeintel sqlite store yet — click Build to index this repository.";
    case "no_json_artifact":
      return "No repo_map.json artifact yet — click Build to generate the navigator map.";
    default:
      return fallback ?? "Map unavailable";
  }
}
