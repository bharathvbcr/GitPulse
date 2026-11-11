/**
 * The palette's search modes and the prefixes that pick them.
 *
 * Its own module, with no imports, because the view bar's search field reads
 * it on every keystroke to tell a page search from a palette query, and the
 * view bar is in the entry chunk while the palette (and its model's ranking
 * helpers) loads on demand. `model.ts` re-exports all of it: one owner.
 */

export type PaletteMode = "commands" | "files" | "repositories" | "commits" | "branches" | "symbols" | "workspace" | "help";

export const MAX_QUERY_LENGTH = 256;

export const PALETTE_MODES: ReadonlyArray<{ mode: PaletteMode; prefix: string; label: string; hint: string }> = [
  { mode: "commands", prefix: ">", label: "Commands", hint: "Search actions, settings and views" },
  { mode: "files", prefix: "/", label: "Files", hint: "Search repository file names and paths" },
  { mode: "repositories", prefix: "%", label: "Repositories", hint: "Switch tabs or open a recent repository" },
  { mode: "commits", prefix: "#", label: "Commits", hint: "Search loaded history by hash, message or author" },
  { mode: "branches", prefix: "@", label: "Branches", hint: "Search local and remote branches" },
  { mode: "symbols", prefix: ":", label: "Symbols", hint: "Search the active repository's code map" },
  { mode: "workspace", prefix: "::", label: "Workspace", hint: "Search registered repositories; append ~ for TF-IDF name ranking" },
  { mode: "help", prefix: "?", label: "Help", hint: "Discover search modes and keyboard shortcuts" },
];

export function parsePaletteQuery(query: string) {
  const trimmed = query.trim().slice(0, MAX_QUERY_LENGTH);
  let mode: PaletteMode = "commands";
  let prefix = "";
  if (trimmed.startsWith("::")) { mode = "workspace"; prefix = "::"; }
  else if (trimmed.startsWith("#")) { mode = "commits"; prefix = "#"; }
  else if (trimmed.startsWith("@")) { mode = "branches"; prefix = "@"; }
  else if (trimmed.startsWith(":")) { mode = "symbols"; prefix = ":"; }
  else if (trimmed.startsWith("?")) { mode = "help"; prefix = "?"; }
  else if (trimmed.startsWith("/")) { mode = "files"; prefix = "/"; }
  else if (trimmed.startsWith("%")) { mode = "repositories"; prefix = "%"; }
  else if (trimmed.startsWith(">")) prefix = ">";
  let text = trimmed.slice(prefix.length).trim();
  const semantic = mode === "workspace" && text.endsWith("~");
  if (semantic) text = text.slice(0, -1).trim();
  return { mode, text, semantic };
}

/** True when `query` names a palette mode by its prefix (">", "/", "@", ...). */
export function hasPalettePrefix(query: string): boolean {
  const trimmed = query.trimStart();
  return PALETTE_MODES.some((entry) => trimmed.startsWith(entry.prefix));
}
