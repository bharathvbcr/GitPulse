export interface CleanerConfig {
  version: number;
  revision: number;
  roots: string[];
  exclusions: string[];
  enabled: boolean;
  run_when_closed: boolean;
  interval_hours: number;
  next_run_at: number;
  retention_days: number;
  max_run_bytes: number;
  max_targets: number;
}
export interface CleanerCandidate { repo_path: string; path: string; provider: string; bytes: number; }
export interface CleanerInventory { candidates: CleanerCandidate[]; repositories: number; visited_entries: number; partial: boolean; issues: string[]; }
export interface CleanerItem { repo_path: string; path: string; status: string; message: string; bytes_before: number; bytes_after: number | null; }
export interface CleanerRun { id: string; trigger: string; started_at: number; finished_at: number; status: string; repositories: number; partial: boolean; issues: string[]; items: CleanerItem[]; }
export interface CleanerState { config: CleanerConfig; history: CleanerRun[]; running: boolean; supported: boolean; background_supported: boolean; background_error: string | null; agent_rules: string; }
