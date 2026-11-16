import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A plain `#[tauri::command] pub fn` runs on the GUI thread; `(async)` moves a
 * synchronous body onto the blocking pool. A command that reads files or waits
 * on a lock must not be the former, or a slow disk on a loaded host freezes the
 * window while the diagnostics view polls it (the 2026-11-16 diagnostics showed
 * exactly this for the two commands below).
 *
 * The list is the commands known to touch the filesystem from the diagnostics
 * view. Add to it when another one is made async for the same reason.
 */
const ROOT = fileURLToPath(new URL("..", import.meta.url));

const OFF_THE_GUI_THREAD: { file: string; command: string }[] = [
  { file: "src-tauri/src/logging.rs", command: "cmd_diagnostic_log_tail" },
  { file: "src-tauri/src/daemon_lock.rs", command: "cmd_daemon_status" },
];

function attributeOf(source: string, command: string): string {
  const lines = source.split("\n");
  const at = lines.findIndex((line) => new RegExp(`^pub (?:async )?fn ${command}\\b`).test(line));
  expect(at, `${command} is declared`).toBeGreaterThan(-1);
  for (let i = at - 1; i >= 0; i -= 1) {
    const line = lines[i].trim();
    if (line.startsWith("#[tauri::command")) return line;
    if (!line.startsWith("#[") && !line.startsWith("///")) break;
  }
  return "";
}

describe("file-reading commands stay off the GUI thread", () => {
  for (const { file, command } of OFF_THE_GUI_THREAD) {
    it(`${command} is #[tauri::command(async)]`, () => {
      const source = readFileSync(path.join(ROOT, file), "utf8");
      expect(attributeOf(source, command)).toBe("#[tauri::command(async)]");
    });
  }
});
