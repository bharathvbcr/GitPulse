import { invoke } from "@tauri-apps/api/core";
import type { CollisionRisk, InsightsSnapshot, McpInfo } from "./types";

export function getInsightsSnapshot(repoPath: string): Promise<InsightsSnapshot> {
  return invoke<InsightsSnapshot>("cmd_insights_snapshot", { repoPath });
}

export function getCollisionRisk(repoPath: string): Promise<CollisionRisk> {
  return invoke<CollisionRisk>("cmd_collision_risk", { repoPath });
}

export function getMcpInfo(): Promise<McpInfo> {
  return invoke<McpInfo>("cmd_mcp_info");
}
