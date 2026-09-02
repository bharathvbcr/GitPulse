/**
 * Wire types for local model-server discovery.
 *
 * Answers "what could I select", which the AI settings had no way to ask: the
 * harness's capability probe requires a base URL and a model, which is the
 * answer rather than the question.
 *
 * Mirrors `src-tauri/src/harness/protocol.rs`.
 */

/** One model a local server reports. */
export interface ScanModel {
  id: string;
  /**
   * Zero when the server reported no window.
   *
   * Zero is *unreported*, not "no context" — `context_window_source` says
   * which. A model rendered with a window of 0 reads as broken; one rendered
   * without a window reads as unknown, and only the second is true.
   */
  context_window: number;
  context_window_source: string;
  /**
   * A window the scanner read off the server and refused as implausible.
   * Non-zero only when it was, so the refusal is visible rather than silent.
   */
  implausible_window: number;
  /**
   * Whether the capability flags below mean anything.
   *
   * Without this, "does not support tools" and "nobody asked" are the same
   * `false`. The UI must not render an unasked model as incapable, and must
   * not offer a tool-calling feature on the strength of a flag nobody set.
   */
  capabilities_known: boolean;
  supports_tools: boolean;
  supports_reasoning: boolean;
  supports_vision: boolean;
  /** Whether the model generates text at all — an embedding model does not. */
  supports_completion: boolean;
}

/** One local model server that answered a scan. */
export interface ScanServer {
  base_url: string;
  /**
   * How the server identified itself. `openai-compatible` means it answered
   * `/v1/models` and nothing else the harness knows to ask — a working server,
   * and more honest than naming a runtime from the port number.
   */
  runtime: string;
  /** Only Ollama reports one. Never load-bearing. */
  version: string;
  models: ScanModel[];
}

/** What one discovery sweep found. */
export interface ScanResult {
  servers: ScanServer[];
  /**
   * How many endpoints were probed.
   *
   * "Nothing is running" and "we only looked in one place" are different
   * answers, and this is the difference.
   */
  scanned: number;
  /** Whether per-model capabilities were asked for at all. */
  capabilities: boolean;
}

/** How a model's tool support should be described, given what was asked. */
export function toolSupportLabel(model: ScanModel): string {
  if (!model.capabilities_known) return "tool support unknown";
  return model.supports_tools ? "tools" : "no tools";
}

/** How a model's context window should be described. */
export function contextWindowLabel(model: ScanModel): string {
  if (model.implausible_window > 0) {
    return `window refused (${model.implausible_window.toLocaleString()} reported)`;
  }
  if (model.context_window <= 0) return "window unreported";
  return `${model.context_window.toLocaleString()} tokens`;
}

/** A one-line summary of a sweep, honest about how hard it looked. */
export function sweepSummary(result: ScanResult): string {
  const servers = result.servers.length;
  const models = result.servers.reduce((n, s) => n + s.models.length, 0);
  if (servers === 0) {
    return `No model server answered on ${result.scanned} endpoint${result.scanned === 1 ? "" : "s"}.`;
  }
  return (
    `${servers} server${servers === 1 ? "" : "s"}, ${models} model${models === 1 ? "" : "s"}` +
    ` across ${result.scanned} endpoint${result.scanned === 1 ? "" : "s"}` +
    (result.capabilities ? "." : "; capabilities not queried.")
  );
}
