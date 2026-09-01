/**
 * Shared usage printer for the repository's script entry points.
 *
 * Backlog A2: `parseArgs` rejected an unknown flag with exit 2 and no way to
 * discover the valid ones short of reading the source. Asking for help is not
 * an error, so `--help` prints this and exits 0 — the distinction between
 * "helped" and "failed" is the point.
 *
 * @typedef {{ flag: string, description: string }} UsageFlag
 * @typedef {{ name: string, summary: string, usage?: string, flags: UsageFlag[], exits?: string }} UsageSpec
 */

import { alignFlags } from "./columns.mjs";

/** Flags every entry point accepts. */
export const HELP_FLAGS = Object.freeze(["--help", "-h"]);

/** @param {string[]} argv */
export function wantsHelp(argv) {
  return argv.some((arg) => HELP_FLAGS.includes(arg));
}

/**
 * Render aligned usage text. Descriptions line up in a single column so a
 * long flag in the list does not ragged-edge the rest.
 *
 * @param {UsageSpec} spec
 * @returns {string}
 */
export function formatUsage({ name, summary, usage, flags, exits }) {
  const lines = [summary, "", `Usage: ${usage ?? `node scripts/${name}.mjs [options]`}`, ""];
  lines.push(...alignFlags(flags));
  if (exits) lines.push("", `Exit codes: ${exits}`);
  return lines.join("\n");
}
