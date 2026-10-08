/** One matching line (Rust `engine::content_search::ContentMatch`). */
export interface ContentMatch {
  path: string;
  line: number;
  column: number;
  text: string;
  /** The line was longer than the backend carries; `text` is its head. */
  text_clipped: boolean;
}

/** Why a search answer is partial. */
export type SearchTruncation = "match_limit" | "output_cap" | "deadline" | "cancelled";

/** A search's answer (Rust `ContentSearchReport`). */
export interface ContentSearchReport {
  matches: ContentMatch[];
  files: number;
  /** True whenever `matches` is not every match that exists. */
  truncated: boolean;
  /** One of `SearchTruncation` when partial; read through `describeSearchReport`. */
  truncated_reason: string | null;
  /** The commit searched; null for the working tree. */
  revision: string | null;
}

/** What `cmd_search_content` takes (Rust `ContentSearchOptions`). */
export interface ContentSearchOptions {
  fixed_strings?: boolean;
  ignore_case?: boolean;
  revision?: string | null;
  max_matches?: number | null;
}
