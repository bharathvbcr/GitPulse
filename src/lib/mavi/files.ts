/**
 * The repository-relative paths a batch of a local thread's lines says
 * changed: each finished edit (`tool_use_result.filePath`) and each path that
 * moved outside the edit tools when a turn ended (`mavi_turn_files`). Read
 * for every thread, opened or not, so the filters are cheap substring checks
 * and only matching lines are parsed.
 */
import type { MaviEntry } from "./types";

export function changedPaths(root: string, entries: readonly Pick<MaviEntry, "line">[]): string[] {
  const base = root.replace(/[\\/]+$/, "");
  const paths: string[] = [];
  for (const { line } of entries) {
    const edit = line.includes('"tool_use_result"') && line.includes('"filePath"');
    const moved = line.includes('"mavi_turn_files"');
    if (!edit && !moved) continue;
    let value: Record<string, unknown>;
    try {
      value = JSON.parse(line) as Record<string, unknown>;
    } catch {
      continue;
    }
    if (value.type === "user" && edit) {
      const result = value.tool_use_result as Record<string, unknown> | undefined;
      const file = typeof result?.filePath === "string" ? result.filePath : null;
      if (file?.startsWith(`${base}/`)) paths.push(file.slice(base.length + 1));
    } else if (value.type === "mavi_turn_files" && Array.isArray(value.moved)) {
      for (const m of value.moved) {
        const path = m && typeof m === "object" ? (m as Record<string, unknown>).path : null;
        if (typeof path === "string") paths.push(path);
      }
    }
  }
  return paths;
}
