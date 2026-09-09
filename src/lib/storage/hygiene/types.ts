export interface CacheEntry {
  id: string;
  label: string;
  path: string | null;
  bytes: number | null;
  action: string | null;
  note: string;
  error: string | null;
}

export interface CacheInventory {
  entries: CacheEntry[];
  measured_at: number;
}

export interface HygienePlan {
  id: string;
  repo_path: string;
  target: string;
  label: string;
  path: string;
  scope: string;
  command: string;
  bytes: number;
  files: number;
  expires_at: number;
  warning: string;
}

export interface HygieneOutcome {
  success: boolean;
  message: string;
  bytes_before: number;
  bytes_after: number | null;
  elapsed_ms: number;
}
