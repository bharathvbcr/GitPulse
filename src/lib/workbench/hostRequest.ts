/**
 * Narrow typed transport over `cmd_workbench_request` for host-coordinated
 * workbench methods (DAG readiness, cross-repository queue, GitHub issue sync,
 * and durable outbox) without modifying `client.ts` or adding Tauri commands.
 */

import { invoke } from "../ipc/invoke";
import { WorkbenchError, explainError } from "./client";

const MAX_RESPONSE_BYTES = 2 * 1024 * 1024;

export type HostWorkbenchMethod =
  | "items.readiness"
  | "queue.enqueue_host"
  | "queue.acquire"
  | "queue.get"
  | "queue.list"
  | "queue.member.update"
  | "queue.verify_combined"
  | "queue.sweep"
  | "queue.cancel"
  | "github_sync.link"
  | "github_sync.get"
  | "github_sync.list"
  | "github_sync.poll"
  | "github_sync.accept"
  | "github_sync.dismiss"
  | "outbox.preview"
  | "outbox.enqueue"
  | "outbox.get"
  | "outbox.list"
  | "outbox.deliver"
  | "outbox.cancel";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function decodeResponse(value: unknown): Record<string, unknown> {
  let decoded: unknown = value;
  if (typeof value === "string") {
    if (value.length > MAX_RESPONSE_BYTES) {
      throw new WorkbenchError("protocol_error", "Workbench response exceeded the size limit.");
    }
    try {
      decoded = JSON.parse(value);
    } catch {
      throw new WorkbenchError("protocol_error", "Workbench returned malformed JSON.");
    }
  }
  if (!isRecord(decoded)) {
    throw new WorkbenchError("protocol_error", "Workbench returned an unexpected response shape.");
  }
  if (decoded.ok === false) {
    const code = typeof decoded.code === "string" ? decoded.code : "store_error";
    const message = typeof decoded.message === "string" ? decoded.message : "Workbench request failed.";
    throw new WorkbenchError(code, message);
  }
  return decoded;
}

export async function hostRequest(
  method: HostWorkbenchMethod,
  input: Record<string, unknown>,
): Promise<Record<string, unknown>> {
  try {
    const raw = await invoke<unknown>("cmd_workbench_request", {
      method,
      input: JSON.stringify(input),
    });
    return decodeResponse(raw);
  } catch (error) {
    if (error instanceof WorkbenchError) throw error;
    const code =
      isRecord(error) && typeof error.code === "string" ? error.code : "transport_error";
    throw new WorkbenchError(code, explainError(error));
  }
}
