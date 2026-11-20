/**
 * Wire types for GitPulse's periodic Manvi system steward
 * (`src-tauri/src/tool_config.rs` and `src-tauri/src/harness/steward.rs`).
 *
 * Checked against the Rust definitions by `scripts/check-coverage-types.mjs`.
 */

export interface RepoStewardOverride {
  enabled?: boolean;
  message_agents?: boolean;
  slow_critical?: boolean;
  clean_critical?: boolean;
}

export interface EffectiveStewardPolicy {
  enabled: boolean;
  interval_secs: number;
  message_agents: boolean;
  slow_critical: boolean;
  clean_critical: boolean;
  clean_retention_days: number;
  overridden: boolean;
}

export interface ManviStewardSettings {
  enabled: boolean;
  interval_secs: number;
  message_agents: boolean;
  slow_critical: boolean;
  clean_critical: boolean;
  clean_retention_days: number;
  repositories: Record<string, RepoStewardOverride>;
}

export interface StewardCleanItem {
  repo_path: string;
  target: string;
  status: string;
  measured_bytes: number;
  bytes_after: number | null;
  detail: string;
}

export interface StewardTickRecord {
  id: string;
  trigger: string;
  ran_at_ms: number;
  outcome: string;
  tier: string;
  binding: string;
  manvi_mode: string;
  manvi_model: string;
  model_generated: boolean;
  summary: string;
  directive: string;
  messaged_sessions: number;
  messaged_repositories: number;
  skipped_untrusted_repositories: number;
  throttled_processes: number;
  restored_processes: number;
  reclaimed_bytes: number;
  cleaned_items: StewardCleanItem[];
  warnings: string[];
}

export interface ManviStewardStatus {
  settings: ManviStewardSettings;
  effective: EffectiveStewardPolicy;
  repo_common_dir: string | null;
  /** What GITPULSE_MANVI_STEWARD says, for a person to read. */
  env_override: string | null;
  /** What GITPULSE_MANVI_STEWARD forces (true on, false off, null unset). */
  env_forced: boolean | null;
  manvi_state: string;
  manvi_mode: string;
  manvi_model: string;
  manvi_detail: string;
  running: boolean;
  critical_throttle_active: boolean;
  throttled_count: number;
  last_check_ms: number | null;
  next_check_ms: number | null;
  history: StewardTickRecord[];
}
