/**
 * Contract: the modes this UI offers are the modes Rust can expand.
 *
 * The flags themselves live in exactly one place — `policy()` in
 * `src-tauri/src/workbench/terminal_command.rs` — and the frontend carries
 * only names. That split is what keeps a flag table from being transcribed
 * into TypeScript and drifting; this test is what keeps the *names* from
 * drifting instead.
 *
 * A mode offered here that Rust cannot expand is a setting that fails at
 * spawn. A mode Rust supports that is missing here is a capability the reader
 * can never reach. Both are silent without this file.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  BYPASS_MODE,
  PERMISSION_LABELS,
  PERMISSION_LAUNCHERS,
  PERMISSION_MODES,
} from "./agentDefaults";
import { LAUNCHERS } from "./tabs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const rust = readFileSync(
  join(repoRoot, "src-tauri", "src", "workbench", "terminal_command.rs"),
  "utf8",
);

/** Every `("provider", "mode")` arm of the policy table. */
const arms = (() => {
  const start = rust.indexOf("let flags = match (provider, mode) {");
  expect(start, "the policy table's match was not found").toBeGreaterThan(-1);
  const body = rust.slice(start, rust.indexOf("\n    };", start));
  const found = [...body.matchAll(/\("([a-z_]+)",\s*"([a-z_]+)"\)\s*=>/g)].map((m) => ({
    provider: m[1],
    mode: m[2],
  }));
  // A reshaped table must fail loudly rather than silently stop checking:
  // zero arms parsed is not the same fact as zero arms existing.
  expect(found.length, "no (provider, mode) arms parsed from the policy table").toBeGreaterThan(0);
  return found;
})();

describe("permission modes", () => {
  it("are exactly the modes the Rust table declares", () => {
    const declared = (() => {
      const match = rust.match(/pub\(crate\) const PERMISSION_MODES:\s*\[&str;\s*\d+\]\s*=\s*\[([^\]]*)\]/);
      expect(match, "PERMISSION_MODES declaration not found in terminal_command.rs").not.toBeNull();
      return [...(match?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
    })();
    // Order included: both lists are "least authority first", and a chooser
    // that reordered them would put bypass next to the safe default.
    expect([...PERMISSION_MODES]).toEqual(declared);
  });

  it("are each expandable by every launcher this UI offers them for", () => {
    for (const launcher of PERMISSION_LAUNCHERS) {
      for (const mode of PERMISSION_MODES) {
        expect(
          arms.some((arm) => arm.provider === launcher && arm.mode === mode),
          `the policy table has no arm for (${launcher}, ${mode}), so choosing it would fail at spawn`,
        ).toBe(true);
      }
    }
  });

  it("cover every launcher the Rust table has arms for", () => {
    const providers = [...new Set(arms.map((arm) => arm.provider))].sort();
    expect([...PERMISSION_LAUNCHERS].sort()).toEqual(providers);
  });

  it("each have a label and a description a reader can act on", () => {
    for (const mode of PERMISSION_MODES) {
      const entry = PERMISSION_LABELS[mode];
      expect(entry, `${mode} has no label`).toBeTruthy();
      expect(entry.label.trim().length, `${mode} has an empty label`).toBeGreaterThan(0);
      expect(entry.detail.trim().length, `${mode} has an empty description`).toBeGreaterThan(0);
      // A label that is the flag's spelling tells a reader nothing about how
      // much authority they are handing over.
      expect(entry.label).not.toMatch(/^--/);
    }
  });

  it("name bypass as the one mode that disables a safety control", () => {
    // Pinned against the Rust constant so the two cannot come to mean
    // different modes — the acknowledgement gate keys off this name.
    expect(rust).toContain(`pub(crate) const BYPASS_MODE: &str = "${BYPASS_MODE}"`);
    expect(PERMISSION_MODES.at(-1)).toBe(BYPASS_MODE);
  });
});

describe("permission launchers", () => {
  it("are a strict subset of the tab strip, excluding the shell", () => {
    const strip = LAUNCHERS.map((entry) => entry.kind);
    for (const launcher of PERMISSION_LAUNCHERS) {
      expect(strip, `${launcher} is not a launcher the strip offers`).toContain(launcher);
    }
    expect(PERMISSION_LAUNCHERS).not.toContain("shell");
    // manvi is a launcher with no policy. Offering it a mode would produce a
    // refusal at spawn rather than a setting, which is why the two lists differ.
    expect(PERMISSION_LAUNCHERS).not.toContain("manvi");
    expect(PERMISSION_LAUNCHERS.length).toBeLessThan(strip.length);
  });
});

describe("the launch boundary", () => {
  it("refuses a mode for a launcher with no policy rather than dropping it", () => {
    const start = rust.indexOf("pub(crate) fn apply_permission_mode(");
    expect(start, "apply_permission_mode not found").toBeGreaterThan(-1);
    const body = rust.slice(start, rust.indexOf("\nfn policy(", start));
    expect(body).toContain("if !is_terminal_provider(provider) {");
    expect(body).toContain("return Err(");
  });

  it("gates bypass on an acknowledgement, symmetrically", () => {
    // The symmetry is what makes a caller that hardcoded `acknowledged: true`
    // fail on its first ordinary launch instead of lying dormant.
    expect(rust).toContain("if (mode == BYPASS_MODE) != acknowledged {");
  });

  it("puts the policy flags ahead of the caller's own arguments", () => {
    // `-- <text>` makes everything after it positional, so a permission flag
    // appended behind a prompt is prompt text rather than a permission.
    expect(rust).toContain("expanded.extend(args.unwrap_or_default());");
  });
});
