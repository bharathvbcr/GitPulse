/**
 * In-process code intelligence types matching `src-tauri/src/codeintel/mod.rs`.
 */

export interface CodeintelSymbolHit {
  symbol_name: string;
  file_path: string;
  kind: string;
  span_start_line: number;
  span_end_line: number;
  source_span: string;
  /**
   * Set when `source_span` is empty because the file could not be read — a map
   * generation newer than the checkout, or a file since deleted — rather than
   * because the symbol has no body. Without it the two render identically.
   */
  source_unavailable_reason?: string | null;
  score: number;
}

export interface CodeintelEdge {
  source_file: string;
  target_file: string;
  source_symbol: string;
  target_symbol: string;
  confidence: number;
}

export interface CodeintelDeadSymbol {
  symbol_name: string;
  file_path: string;
  confidence: number;
  is_exempt: boolean;
  exemption_reason?: string | null;
}

export interface CodeintelResponse<T> {
  available: boolean;
  reason?: string | null;
  items: T[];
  total: number;
  shown: number;
  truncated: boolean;
}

export interface CodeintelStatus {
  available: boolean;
  db_path: string;
  generation_id?: number | null;
  total_files?: number | null;
  total_symbols?: number | null;
  total_edges?: number | null;
  reason?: string | null;
}
