import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { MANAGED_PROVIDERS, PROVIDER_CHOICES, supportsManaged } from "../src/lib/workbench/taskHandoff";
import type { AgentProvider } from "../src/lib/workbench/vocabulary";

/**
 * Which providers GitPulse will run in the *managed* lane is decided in three
 * places, in three languages: the renderer that offers the choice, the Rust
 * workbench that prepares and launches the attempt, and — when the harness is
 * checked out beside this repository — the Go adapter set that has to speak the
 * provider's protocol.
 *
 * They are not redundant copies that could be collapsed. Each one is the gate
 * for a different process, and a disagreement between them is a specific,
 * silent failure:
 *
 *   - renderer wider than Rust → the option is offered and the launch is
 *     refused, with no way for the reader to know that was inevitable.
 *   - Rust wider than the harness → the attempt is stored and claimed, then
 *     dies at `prepare` with the repository capacity already taken.
 *   - harness wider than Rust → an adapter nobody can reach.
 *
 * So the lists are checked against each other rather than trusted, and the
 * harness arm degrades to a skip with a named reason when the sibling checkout
 * is absent, instead of quietly passing as if it had been compared.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Providers the Rust workbench admits, read from the function that decides. */
function rustManagedProviders(): string[] {
  const source = readFileSync(path.join(ROOT, "src-tauri/src/workbench/terminal_command.rs"), "utf8");
  const fn = source.slice(source.indexOf("fn is_managed_provider"));
  const body = fn.slice(0, fn.indexOf("\n}"));
  const match = body.match(/matches!\(\s*provider\s*,([^)]*)\)/);
  if (!match) throw new Error("is_managed_provider no longer uses a matches! pattern this test can read");
  return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();
}

/** Providers the harness has an adapter for, read from its own exported list. */
function harnessManagedProviders(): string[] | null {
  const source = path.join(ROOT, "..", "Manvi", "manvi", "codingagent", "codex.go");
  if (!existsSync(source)) return null;
  const text = readFileSync(source, "utf8");
  const match = text.match(/var ManagedProviders = \[\]string\{([^}]*)\}/);
  if (!match) throw new Error("codingagent.ManagedProviders is no longer a literal slice this test can read");
  return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();
}

describe("managed provider parity", () => {
  const renderer: AgentProvider[] = [...MANAGED_PROVIDERS].sort();

  it("offers a managed lane for Codex and Claude Code, and for nothing else", () => {
    expect(renderer).toEqual(["claude", "codex"]);
    for (const provider of PROVIDER_CHOICES) {
      expect(supportsManaged(provider)).toBe(renderer.includes(provider));
    }
  });

  it("agrees with the Rust workbench about who may be launched", () => {
    expect(rustManagedProviders()).toEqual(renderer);
  });

  it("never offers a managed lane to a provider with no terminal lane either", () => {
    // A managed provider is always also a terminal provider: the terminal lane
    // is the fallback every launch path can reach, so a provider that only had
    // a managed one would have no way to recover from a harness failure.
    const terminal = readFileSync(path.join(ROOT, "src-tauri/src/workbench/terminal_command.rs"), "utf8");
    const fn = terminal.slice(terminal.indexOf("fn is_terminal_provider"));
    const body = fn.slice(0, fn.indexOf("\n}"));
    const terminalProviders = [...body.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    for (const provider of renderer) {
      expect(terminalProviders).toContain(provider);
    }
  });

  it("leaves no wire guard with its own transcribed provider list", () => {
    // The fourth decision site, and the one that drifted. `launchManagedRun`
    // validated the launch reply with `run.provider === "codex"` written out
    // by hand, so when Claude Code gained an adapter every managed Claude
    // launch came back "Task storage returned an invalid response" — a
    // protocol error naming a cause that was not true, from a lane that had
    // been opened everywhere else. Three whole-process gates agreeing is worth
    // nothing if a response parser quietly holds a fourth opinion.
    //
    // Whole-word, so `MANAGED_PROVIDERS` and `supportsManaged` do not match
    // and the fixtures' own provider values are not mistaken for a gate.
    const client = readFileSync(path.join(ROOT, "src/lib/workbench/client.ts"), "utf8");
    for (const line of client.split("\n")) {
      if (!/\bprovider\b/.test(line)) continue;
      expect(line, "client.ts compares a provider to a literal instead of asking supportsManaged")
        .not.toMatch(/\bprovider\b[^\n]*(===|!==)\s*"(codex|claude|grok|agy)"/);
    }
    expect(client).toContain("supportsManaged(run.provider)");
  });

  it("agrees with the harness's adapter set, or says it could not look", () => {
    const harness = harnessManagedProviders();
    if (harness === null) {
      // Not a pass. The sibling checkout is absent, so this comparison did not
      // run, and saying so is the whole point — a check that could not run must
      // never read like one that ran and agreed.
      expect(existsSync(path.join(ROOT, "..", "Manvi"))).toBe(false);
      return;
    }
    expect(harness).toEqual(renderer);
  });
});
