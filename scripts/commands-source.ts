import { readdirSync, readFileSync } from "node:fs";

const COMMANDS_DIR = new URL("../src-tauri/src/commands/", import.meta.url);

/**
 * The production source of the IPC command modules.
 *
 * `commands/` is a module tree (one file per command group, `mod.rs` holding
 * the shared helpers and re-exports), and the contract tests that read command
 * source must see all of it. Test-only files are left out so a test body can
 * never satisfy a production assertion. `mod.rs` comes last: its trailing
 * `#[cfg(test)] mod tests;` is where `production()` cuts, which then drops only
 * the test declarations.
 */
export function readCommandsSource(): string {
  const groups = readdirSync(COMMANDS_DIR)
    .filter((name) => name.endsWith(".rs") && name !== "mod.rs")
    .filter((name) => name !== "tests.rs" && !name.endsWith("_tests.rs"))
    .sort();
  return [...groups, "mod.rs"]
    .map((name) => readFileSync(new URL(name, COMMANDS_DIR), "utf8"))
    .join("\n");
}
